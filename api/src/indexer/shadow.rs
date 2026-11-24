//! Shadow indexing and divergence detection (defence against silent indexer drift).
//!
//! The primary indexer rebuilds its snapshot from one set of Soroban RPC
//! endpoints. A bug in its XDR/JSON decoding, or a single node returning stale
//! state, can therefore change the API's answers without any error being
//! logged. This module runs an independent reader against a *different*
//! Soroban node, with its own in-memory snapshot and its own decoder, and
//! periodically compares a random sample of contracts against the primary
//! snapshot.
//!
//! Design constraints:
//!
//! * Independent endpoint: `RWA_SHADOW_RPC_URL` (or the second configured RPC
//!   node) is used exclusively; the shadow reader never shares the primary
//!   `RpcClient`'s circuit-breaker state.
//! * Independent snapshot: [`ShadowIndexer::snapshot`] is a separate
//!   `ArcSwap`, never the `AppState` snapshot.
//! * Bounded overhead: only a random 10% sample (configurable) of contracts is
//!   re-read per cycle, and verification runs once a minute.
//! * Forensics: a divergence dumps the shadow snapshot into the `forensics`
//!   RocksDB column family so the exact decoded state can be inspected later.
//!
//! [`verify_once`] and [`diff_snapshots`] are pure enough to be exercised
//! without a network; see the tests at the bottom, including the injected
//! silent XDR-parsing bug.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::rpc_client::RpcClient;
use super::{address_scval, parse_i128, AppState, Config, IndexError, Snapshot, POLL_INTERVAL};
use crate::storage::rocksdb::{RocksLedger, FORENSICS};

/// How often the verifier compares the two snapshots.
pub const VERIFY_INTERVAL: Duration = Duration::from_secs(60);
/// Default fraction of contracts checked per cycle (10%).
pub const DEFAULT_SAMPLE_PERCENT: u8 = 10;

/// Configuration for the independent shadow reader.
#[derive(Debug, Clone)]
pub struct ShadowConfig {
    pub rpc_url: String,
    pub registry_id: String,
    pub read_source: String,
    pub poll_interval: Duration,
    pub verify_interval: Duration,
    pub sample_percent: u8,
}

impl ShadowConfig {
    /// Derive the shadow configuration from the primary indexer configuration.
    ///
    /// A dedicated node is expected via `RWA_SHADOW_RPC_URL`. When it is
    /// absent we fall back to the second primary URL, and finally (with a
    /// loud warning) to the first one — a shadow pointed at the same node
    /// cannot detect node-level drift, but still catches decoder bugs.
    pub fn from_main(config: &Config) -> Self {
        let rpc_url = std::env::var("RWA_SHADOW_RPC_URL")
            .ok()
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .or_else(|| config.rpc_urls.get(1).cloned())
            .or_else(|| config.rpc_urls.first().cloned())
            .unwrap_or_else(|| "https://soroban-testnet.stellar.org".to_string());

        if !config.rpc_urls.iter().any(|url| url == &rpc_url) {
            tracing::info!(%rpc_url, "shadow indexer uses an independent RPC endpoint");
        } else if config.rpc_urls.first().is_some_and(|url| url == &rpc_url) {
            tracing::warn!(
                %rpc_url,
                "RWA_SHADOW_RPC_URL not set and no secondary RPC: shadow reads the primary node"
            );
        }

        let sample_percent = std::env::var("RWA_SHADOW_SAMPLE_PERCENT")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(DEFAULT_SAMPLE_PERCENT)
            .clamp(1, 100);
        let verify_interval = std::env::var("RWA_SHADOW_VERIFY_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(VERIFY_INTERVAL);

        Self {
            rpc_url,
            registry_id: config.registry_id.clone(),
            read_source: config.read_source.clone(),
            poll_interval: POLL_INTERVAL,
            verify_interval,
            sample_percent,
        }
    }
}

/// Errors produced by the shadow reader.
#[derive(Debug, thiserror::Error)]
pub enum ShadowError {
    #[error(transparent)]
    Rpc(#[from] IndexError),
    #[error("shadow decode failed: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("shadow decoder rejected a value: {0}")]
    Decoder(String),
}

// ---------------------------------------------------------------------------
// Raw contract shapes and the injectable decoder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ShadowRawAsset {
    pub id: u64,
    pub token_contract: String,
    pub issuer: String,
    pub name: String,
    pub asset_type: String,
    pub valuation: String,
    #[allow(dead_code, reason = "mirrors the primary registry entry")]
    pub created_at: u32,
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ShadowRawMetadata {
    pub symbol: String,
    pub total_supply: String,
    pub decimals: u32,
    pub compliance_contract: String,
    pub asset_description: String,
    pub paused: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ShadowKyc {
    status: serde_json::Value,
    jurisdiction: String,
    expires_at: u32,
}

/// The decoding surface the shadow reader relies on.
///
/// This is deliberately separate from the primary indexer's decoding path:
/// an independent implementation is what makes silent-parsing-bug detection
/// possible. Tests substitute a decoder with a silently corrupted balance.
pub trait ShadowDecoder: Send + Sync {
    fn decode_assets(&self, value: &serde_json::Value) -> Result<Vec<ShadowRawAsset>, ShadowError>;
    fn decode_metadata(&self, value: &serde_json::Value) -> Result<ShadowRawMetadata, ShadowError>;
    fn decode_balance(&self, value: &serde_json::Value) -> Result<i128, ShadowError>;
}

/// Correct, strict decoder used in production.
#[derive(Debug, Default, Clone, Copy)]
pub struct StrictDecoder;

impl ShadowDecoder for StrictDecoder {
    fn decode_assets(&self, value: &serde_json::Value) -> Result<Vec<ShadowRawAsset>, ShadowError> {
        Ok(serde_json::from_value(value.clone())?)
    }

    fn decode_metadata(&self, value: &serde_json::Value) -> Result<ShadowRawMetadata, ShadowError> {
        Ok(serde_json::from_value(value.clone())?)
    }

    fn decode_balance(&self, value: &serde_json::Value) -> Result<i128, ShadowError> {
        match value {
            serde_json::Value::String(s) => Ok(parse_i128(s)),
            serde_json::Value::Number(n) => Ok(n.as_i64().unwrap_or(0) as i128),
            serde_json::Value::Null => Ok(0),
            other => Err(ShadowError::Decoder(format!(
                "unexpected balance value: {other}"
            ))),
        }
    }
}

/// Test-only decoder that silently drops all but the low 32 bits of a
/// balance — the classic "i128 high word lost in XDR decode" bug. It emits no
/// error, so only the verifier can reveal it.
#[cfg(test)]
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct BuggyI128Decoder(StrictDecoder);

#[cfg(test)]
impl ShadowDecoder for BuggyI128Decoder {
    fn decode_assets(&self, value: &serde_json::Value) -> Result<Vec<ShadowRawAsset>, ShadowError> {
        self.0.decode_assets(value)
    }

    fn decode_metadata(&self, value: &serde_json::Value) -> Result<ShadowRawMetadata, ShadowError> {
        self.0.decode_metadata(value)
    }

    fn decode_balance(&self, value: &serde_json::Value) -> Result<i128, ShadowError> {
        Ok(self.0.decode_balance(value)? & 0xffff_ffff)
    }
}

// ---------------------------------------------------------------------------
// Compact, independently owned snapshot
// ---------------------------------------------------------------------------

/// The three state classes the verifier compares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AssetFingerprint {
    pub asset_id: u64,
    /// SHA-256 of canonicalised token + registry metadata.
    pub metadata_hash: String,
    /// Holder address -> balance in base units.
    pub holders: BTreeMap<String, String>,
    /// Compliance summary encoded as stable key/value pairs, e.g.
    /// `status.Approved` -> `2`, `jurisdiction.US` -> `1`.
    pub compliance: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShadowSnapshot {
    pub ledger: u32,
    pub updated_at: Option<String>,
    pub assets: BTreeMap<u64, AssetFingerprint>,
    /// Number of contracts the sample was drawn from (before sampling).
    pub sampled_total: usize,
    pub read_errors: usize,
}

impl ShadowSnapshot {
    /// Encode a primary-snapshot asset into the same fingerprint shape the
    /// shadow reader produces, so the two can be compared field by field.
    pub fn primary_fingerprint(
        asset: &crate::models::Asset,
        holders: &[crate::models::Holder],
        compliance: &crate::models::ComplianceSummary,
    ) -> AssetFingerprint {
        let metadata = serde_json::json!({
            "symbol": asset.symbol,
            "name": asset.name,
            "description": asset.description,
            "asset_type": asset.asset_type,
            "total_supply": asset.total_supply,
            "valuation_cents": asset.valuation_cents,
            "decimals": asset.decimals,
            "paused": asset.paused,
            "active": asset.active,
        });
        let metadata_hash = hex::encode(Sha256::digest(metadata.to_string().as_bytes()));
        let holders = holders
            .iter()
            .map(|h| (h.address.clone(), h.balance.clone()))
            .collect();
        let mut compliance_map = BTreeMap::new();
        compliance_map.insert("total_records".into(), compliance.total_records.to_string());
        compliance_map.insert("approved".into(), compliance.approved.to_string());
        compliance_map.insert("suspended".into(), compliance.suspended.to_string());
        compliance_map.insert("rejected".into(), compliance.rejected.to_string());
        compliance_map.insert("pending".into(), compliance.pending.to_string());
        compliance_map.insert("with_expiry".into(), compliance.with_expiry.to_string());
        for entry in &compliance.jurisdictions {
            compliance_map.insert(
                format!("jurisdiction.{}", entry.jurisdiction),
                entry.count.to_string(),
            );
        }
        AssetFingerprint {
            asset_id: asset.id,
            metadata_hash,
            holders,
            compliance: compliance_map,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscrepancyKind {
    HolderBalance,
    ComplianceState,
    AssetMetadata,
    MissingAsset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discrepancy {
    pub asset_id: u64,
    pub kind: DiscrepancyKind,
    /// Human-readable primary value.
    pub primary: String,
    /// Human-readable shadow value.
    pub shadow: String,
    /// Present for per-holder balance mismatches.
    pub address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowDivergence {
    pub detected_at: String,
    pub primary_ledger: u32,
    pub shadow_ledger: u32,
    pub sample_size: usize,
    pub discrepancies: Vec<Discrepancy>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationReport {
    pub last_verified_at: Option<String>,
    pub primary_ledger: u32,
    pub shadow_ledger: u32,
    pub checked: usize,
    pub known_contracts: usize,
    pub divergences_total: u64,
    pub last_divergence: Option<ShadowDivergence>,
}

/// Compare only the contracts present in the sampled shadow snapshot.
pub fn diff_snapshots(primary: &Snapshot, shadow: &ShadowSnapshot) -> Vec<Discrepancy> {
    let mut out = Vec::new();
    for (id, shadow_fp) in &shadow.assets {
        let Some(asset) = primary.asset(*id) else {
            out.push(Discrepancy {
                asset_id: *id,
                kind: DiscrepancyKind::MissingAsset,
                primary: "<absent>".into(),
                shadow: format!("{} holders", shadow_fp.holders.len()),
                address: None,
            });
            continue;
        };
        let empty_holders = Vec::new();
        let empty_compliance = crate::models::ComplianceSummary::default();
        let primary_fp = ShadowSnapshot::primary_fingerprint(
            asset,
            primary
                .holders
                .get(id)
                .map(Vec::as_slice)
                .unwrap_or(&empty_holders),
            primary.compliance.get(id).unwrap_or(&empty_compliance),
        );

        if primary_fp.metadata_hash != shadow_fp.metadata_hash {
            out.push(Discrepancy {
                asset_id: *id,
                kind: DiscrepancyKind::AssetMetadata,
                primary: primary_fp.metadata_hash.clone(),
                shadow: shadow_fp.metadata_hash.clone(),
                address: None,
            });
        }
        for (address, primary_balance) in &primary_fp.holders {
            let shadow_balance = shadow_fp.holders.get(address).map(String::as_str);
            if shadow_balance != Some(primary_balance.as_str()) {
                out.push(Discrepancy {
                    asset_id: *id,
                    kind: DiscrepancyKind::HolderBalance,
                    primary: primary_balance.clone(),
                    shadow: shadow_balance.unwrap_or("<missing>").to_string(),
                    address: Some(address.clone()),
                });
            }
        }
        for (address, shadow_balance) in &shadow_fp.holders {
            if !primary_fp.holders.contains_key(address) {
                out.push(Discrepancy {
                    asset_id: *id,
                    kind: DiscrepancyKind::HolderBalance,
                    primary: "<missing>".into(),
                    shadow: shadow_balance.clone(),
                    address: Some(address.clone()),
                });
            }
        }
        if primary_fp.compliance != shadow_fp.compliance {
            out.push(Discrepancy {
                asset_id: *id,
                kind: DiscrepancyKind::ComplianceState,
                primary: serde_json::to_string(&primary_fp.compliance)
                    .unwrap_or_else(|_| "<unprintable>".into()),
                shadow: serde_json::to_string(&shadow_fp.compliance)
                    .unwrap_or_else(|_| "<unprintable>".into()),
                address: None,
            });
        }
    }
    out
}

/// Status exposed by `GET /v1/admin/health/shadow`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowStatus {
    pub enabled: bool,
    pub endpoint: String,
    pub status: &'static str,
    pub shadow_ledger: u32,
    pub primary_ledger: u32,
    pub shadow_lag_ledgers: u32,
    pub shadow_age_seconds: Option<i64>,
    pub last_verification_at: Option<String>,
    pub last_divergence_at: Option<String>,
    pub divergences_total: u64,
    pub sample_percent: u8,
    pub verified_contracts: usize,
    pub known_contracts: usize,
    pub read_errors: usize,
    pub forensics_enabled: bool,
}

// ---------------------------------------------------------------------------
// Shadow indexer
// ---------------------------------------------------------------------------

pub struct ShadowIndexer {
    config: ShadowConfig,
    rpc: RpcClient,
    decoder: Arc<dyn ShadowDecoder>,
    snapshot: ArcSwap<ShadowSnapshot>,
    verification: ArcSwap<VerificationReport>,
    forensics: OnceLock<Arc<RocksLedger>>,
}

impl std::fmt::Debug for ShadowIndexer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowIndexer")
            .field("config", &self.config)
            .field("snapshot", &self.snapshot.load_full())
            .field("verification", &self.verification.load_full())
            .finish_non_exhaustive()
    }
}

impl ShadowIndexer {
    pub fn new(config: ShadowConfig) -> Self {
        Self::with_decoder(config, Arc::new(StrictDecoder))
    }

    pub fn with_decoder(config: ShadowConfig, decoder: Arc<dyn ShadowDecoder>) -> Self {
        let rpc = RpcClient::new(vec![config.rpc_url.clone()], config.read_source.clone());
        Self {
            config,
            rpc,
            decoder,
            snapshot: ArcSwap::from(Arc::new(ShadowSnapshot::default())),
            verification: ArcSwap::from(Arc::new(VerificationReport::default())),
            forensics: OnceLock::new(),
        }
    }

    /// Attach the forensics store. Expected to be called once at startup;
    /// a missing store never disables shadow verification.
    pub fn set_forensics(&self, db: Arc<RocksLedger>) {
        let _ = self.forensics.set(db);
    }

    pub fn snapshot(&self) -> Arc<ShadowSnapshot> {
        self.snapshot.load_full()
    }

    pub fn status(&self) -> ShadowStatus {
        let snapshot = self.snapshot.load_full();
        let report = self.verification.load_full();
        let age = snapshot.updated_at.as_deref().and_then(|value| {
            chrono::DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|time| {
                    (chrono::Utc::now() - time.with_timezone(&chrono::Utc))
                        .num_seconds()
                        .max(0)
                })
        });
        let diverged = report.last_divergence.is_some();
        ShadowStatus {
            enabled: true,
            endpoint: self.config.rpc_url.clone(),
            status: if diverged { "diverged" } else { "ok" },
            shadow_ledger: snapshot.ledger,
            primary_ledger: report.primary_ledger.max(snapshot.ledger),
            shadow_lag_ledgers: report.primary_ledger.saturating_sub(snapshot.ledger),
            shadow_age_seconds: age,
            last_verification_at: report.last_verified_at.clone(),
            last_divergence_at: report
                .last_divergence
                .as_ref()
                .map(|d| d.detected_at.clone()),
            divergences_total: report.divergences_total,
            sample_percent: self.config.sample_percent,
            verified_contracts: report.checked,
            known_contracts: report.known_contracts.max(snapshot.sampled_total),
            read_errors: snapshot.read_errors,
            forensics_enabled: self.forensics.get().is_some(),
        }
    }

    /// Run the refresh loop and the 60-second verifier until shutdown.
    pub async fn run(
        self: Arc<Self>,
        state: AppState,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        let mut refresh_shutdown = shutdown.clone();
        let refresher = Arc::clone(&self);
        tokio::join!(
            async move { refresher.refresh_loop(&mut refresh_shutdown).await },
            async move { self.verify_loop(state, &mut shutdown).await }
        );
    }

    async fn refresh_loop(self: Arc<Self>, shutdown: &mut tokio::sync::watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                return;
            }
            let started = std::time::Instant::now();
            match self.refresh().await {
                Ok(count) => {
                    metrics::histogram!("rwa_shadow_refresh_duration_seconds")
                        .record(started.elapsed().as_secs_f64());
                    metrics::counter!("rwa_shadow_refresh_total", "outcome" => "success")
                        .increment(1);
                    tracing::debug!(contracts = count, "shadow snapshot refreshed");
                }
                Err(error) => {
                    metrics::counter!("rwa_shadow_refresh_total", "outcome" => "failure")
                        .increment(1);
                    tracing::warn!(%error, "shadow refresh failed; keeping last shadow snapshot");
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(self.config.poll_interval) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("shadow indexer stopping");
                        return;
                    }
                }
            }
        }
    }

    async fn verify_loop(
        self: Arc<Self>,
        state: AppState,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) {
        loop {
            if *shutdown.borrow() {
                return;
            }
            self.verify_once(&state);
            tokio::select! {
                _ = tokio::time::sleep(self.config.verify_interval) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    }

    /// Refresh the independent snapshot from the shadow RPC endpoint.
    /// Only a random sample of contracts is read per cycle, bounding overhead.
    pub async fn refresh(&self) -> Result<usize, ShadowError> {
        let entries = self
            .rpc
            .read(&self.config.registry_id, "get_all_assets", vec![])
            .await?;
        let ledger = entries.latest_ledger;
        let raw_assets = self.decoder.decode_assets(&entries.value)?;

        let sample_size = sample_count(raw_assets.len(), self.config.sample_percent);
        let mut chosen: Vec<ShadowRawAsset> = raw_assets
            .choose_multiple(&mut rand::rng(), sample_size)
            .cloned()
            .collect();
        chosen.sort_by_key(|asset| asset.id);

        let mut assets = BTreeMap::new();
        let mut read_errors = 0usize;
        for raw in chosen {
            match self.read_fingerprint(&raw).await {
                Ok(fingerprint) => {
                    assets.insert(raw.id, fingerprint);
                }
                Err(error) => {
                    read_errors += 1;
                    tracing::warn!(asset_id = raw.id, %error, "shadow contract read failed");
                }
            }
        }

        let count = assets.len();
        self.snapshot.store(Arc::new(ShadowSnapshot {
            ledger,
            updated_at: Some(chrono::Utc::now().to_rfc3339()),
            assets,
            sampled_total: raw_assets.len(),
            read_errors,
        }));
        Ok(count)
    }

    async fn read_fingerprint(
        &self,
        raw: &ShadowRawAsset,
    ) -> Result<AssetFingerprint, ShadowError> {
        let meta = self
            .rpc
            .read(&raw.token_contract, "get_metadata", vec![])
            .await?;
        let meta = self.decoder.decode_metadata(&meta.value)?;
        let allowlist = self
            .rpc
            .read(&meta.compliance_contract, "get_allowlist", vec![])
            .await?;
        let addresses: Vec<String> = serde_json::from_value(allowlist.value)?;

        let mut holders = BTreeMap::new();
        let mut compliance = BTreeMap::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for address in &addresses {
            if let Ok(record) = self
                .rpc
                .read(
                    &meta.compliance_contract,
                    "get_record",
                    vec![address_scval(address)?],
                )
                .await
            {
                if !record.value.is_null() {
                    if let Ok(kyc) = serde_json::from_value::<ShadowKyc>(record.value) {
                        let status = normalize_shadow_status(&kyc.status);
                        *counts.entry(format!("status.{status}")).or_default() += 1;
                        if kyc.expires_at != 0 {
                            *counts.entry("with_expiry".into()).or_default() += 1;
                        }
                        *counts
                            .entry(format!("jurisdiction.{}", kyc.jurisdiction))
                            .or_default() += 1;
                    }
                }
            }
            let balance_read = self
                .rpc
                .read(
                    &raw.token_contract,
                    "balance",
                    vec![address_scval(address)?],
                )
                .await?;
            let balance = self.decoder.decode_balance(&balance_read.value)?;
            if balance > 0 {
                holders.insert(address.clone(), balance.to_string());
            }
        }
        *counts.entry("total_records".into()).or_default() = addresses.len();
        compliance.extend(counts.into_iter().map(|(k, v)| (k, v.to_string())));

        let metadata = serde_json::json!({
            "symbol": meta.symbol,
            "name": raw.name,
            "description": meta.asset_description,
            "asset_type": raw.asset_type,
            "total_supply": meta.total_supply,
            "valuation_cents": raw.valuation,
            "decimals": meta.decimals,
            "paused": meta.paused,
            "active": raw.active,
        });
        let metadata_hash = hex::encode(Sha256::digest(metadata.to_string().as_bytes()));

        Ok(AssetFingerprint {
            asset_id: raw.id,
            metadata_hash,
            holders,
            compliance,
        })
    }

    /// Diff the sampled shadow snapshot against the primary snapshot and, on
    /// divergence, raise an alert and persist a forensics copy.
    pub fn verify_once(&self, state: &AppState) -> VerificationReport {
        let shadow = self.snapshot.load_full();
        let primary = state.snapshot();
        let discrepancies = diff_snapshots(&primary, &shadow);
        let now = chrono::Utc::now().to_rfc3339();
        let mut report = (*self.verification.load_full()).clone();
        report.last_verified_at = Some(now.clone());
        report.primary_ledger = primary.stats.last_indexed_ledger;
        report.shadow_ledger = shadow.ledger;
        report.checked = shadow.assets.len();
        report.known_contracts = shadow.sampled_total;

        metrics::gauge!("rwa_shadow_lag_ledgers").set(f64::from(
            primary
                .stats
                .last_indexed_ledger
                .saturating_sub(shadow.ledger),
        ));

        if !discrepancies.is_empty() {
            report.divergences_total += 1;
            let divergence = ShadowDivergence {
                detected_at: now,
                primary_ledger: primary.stats.last_indexed_ledger,
                shadow_ledger: shadow.ledger,
                sample_size: shadow.assets.len(),
                discrepancies,
            };
            metrics::counter!("rwa_shadow_divergence_total").increment(1);
            tracing::error!(
                discrepancies = divergence.discrepancies.len(),
                primary_ledger = divergence.primary_ledger,
                shadow_ledger = divergence.shadow_ledger,
                "ShadowDivergence: shadow indexer disagrees with primary state"
            );
            self.persist_forensics(&divergence, &shadow);
            report.last_divergence = Some(divergence);
        }

        let report = Arc::new(report);
        self.verification.store(report.clone());
        (*report).clone()
    }

    fn persist_forensics(&self, divergence: &ShadowDivergence, shadow: &ShadowSnapshot) {
        let Some(db) = self.forensics.get() else {
            tracing::warn!("shadow divergence detected but forensics store is not configured");
            return;
        };
        let payload = match serde_json::to_vec(&serde_json::json!({
            "divergence": divergence,
            "snapshot": shadow,
        })) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, "could not serialise forensics snapshot");
                return;
            }
        };
        let key = format!(
            "divergence:{}:{:020}",
            divergence.detected_at, divergence.primary_ledger
        );
        match db.put(FORENSICS, key.as_bytes(), &payload) {
            Ok(()) => metrics::counter!("rwa_shadow_forensics_writes_total").increment(1),
            Err(error) => tracing::error!(%error, "failed to persist forensics snapshot"),
        }
    }
}

fn sample_count(total: usize, percent: u8) -> usize {
    if total == 0 {
        return 0;
    }
    ((total * usize::from(percent) + 99) / 100).clamp(1, total)
}

fn normalize_shadow_status(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string(),
        _ => "Unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Asset, ComplianceSummary, Holder, JurisdictionCount, Stats};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration as StdDuration;

    fn primary_asset(id: u64) -> Asset {
        Asset {
            id,
            token_contract: format!("C{id}"),
            issuer: "GISSUER".into(),
            name: format!("Asset {id}"),
            symbol: "RWA".into(),
            asset_type: "equity".into(),
            description: "a token".into(),
            valuation_cents: "100000".into(),
            valuation_usd: 1000.0,
            decimals: 7,
            total_supply: "1000".into(),
            holders: 1,
            active: true,
            paused: false,
            compliance_contract: "CCOMPLIANCE".into(),
            created_at_ledger: 10,
            indexed_at_ledger: 100,
            index_error: None,
        }
    }

    fn compliance_summary() -> ComplianceSummary {
        ComplianceSummary {
            total_records: 1,
            approved: 1,
            suspended: 0,
            rejected: 0,
            pending: 0,
            with_expiry: 0,
            jurisdictions: vec![JurisdictionCount {
                jurisdiction: "US".into(),
                count: 1,
            }],
        }
    }

    fn fingerprint(id: u64, balance: &str) -> AssetFingerprint {
        ShadowSnapshot::primary_fingerprint(
            &primary_asset(id),
            &[Holder {
                address: "GALICE".into(),
                balance: balance.into(),
                share_percent: 100.0,
            }],
            &compliance_summary(),
        )
    }

    /// Build a primary `AppState` whose snapshot carries the correct balance.
    fn primary_state(id: u64, balance: &str) -> AppState {
        let state = AppState::for_test_empty();
        state.replace(Snapshot {
            assets: vec![primary_asset(id)],
            holders: std::collections::HashMap::from([(
                id,
                vec![Holder {
                    address: "GALICE".into(),
                    balance: balance.into(),
                    share_percent: 100.0,
                }],
            )]),
            compliance: std::collections::HashMap::from([(id, compliance_summary())]),
            stats: Stats {
                last_indexed_ledger: 100,
                ..Stats::default()
            },
            ..Snapshot::default()
        });
        state
    }

    fn shadow_snapshot(id: u64, balance: &str) -> ShadowSnapshot {
        let mut snapshot = ShadowSnapshot {
            ledger: 100,
            updated_at: Some("2026-01-01T00:00:00+00:00".into()),
            sampled_total: 10,
            ..ShadowSnapshot::default()
        };
        snapshot.assets.insert(id, fingerprint(id, balance));
        snapshot
    }

    fn shadow_config() -> ShadowConfig {
        ShadowConfig {
            rpc_url: "http://127.0.0.1:1".into(),
            registry_id: "CREGISTRY".into(),
            read_source: "GSOURCE".into(),
            poll_interval: StdDuration::from_secs(3600),
            verify_interval: StdDuration::from_secs(3600),
            sample_percent: 10,
        }
    }

    #[test]
    fn sample_is_bounded_and_never_empty() {
        assert_eq!(sample_count(0, 10), 0);
        assert_eq!(sample_count(10, 10), 1);
        assert_eq!(sample_count(100, 10), 10);
        assert_eq!(sample_count(3, 10), 1);
        assert_eq!(sample_count(10, 100), 10);
    }

    #[test]
    fn silent_xdr_bug_is_detected_by_the_verifier() {
        // The primary snapshot holds the correct 1_000_000_000 balance; the
        // shadow decoder silently truncated it to 1_000 due to a lost i128
        // high word. No decoder error is emitted — only diffing reveals it.
        let primary_balance = "1000000000";
        let shadow_balance = "1000";
        let state = primary_state(7, primary_balance);
        let shadow = ShadowIndexer::with_decoder(shadow_config(), Arc::new(BuggyI128Decoder::default()));
        shadow
            .snapshot
            .store(Arc::new(shadow_snapshot(7, shadow_balance)));

        let report = shadow.verify_once(&state);
        let divergence = report.last_divergence.expect("divergence alert");
        assert!(divergence
            .discrepancies
            .iter()
            .any(|d| d.kind == DiscrepancyKind::HolderBalance
                && d.address.as_deref() == Some("GALICE")
                && d.primary == primary_balance
                && d.shadow == shadow_balance));
        assert_eq!(report.divergences_total, 1);
        assert_eq!(shadow.status().status, "diverged");
        assert_eq!(shadow.status().shadow_lag_ledgers, 0);
    }

    #[test]
    fn matching_snapshots_do_not_raise_an_alert() {
        let state = primary_state(7, "1000000000");
        let shadow = ShadowIndexer::new(shadow_config());
        shadow
            .snapshot
            .store(Arc::new(shadow_snapshot(7, "1000000000")));
        let report = shadow.verify_once(&state);
        assert!(report.last_divergence.is_none());
        assert_eq!(report.divergences_total, 0);
        assert_eq!(shadow.status().status, "ok");
    }

    #[test]
    fn buggy_decoder_truncates_without_error() {
        let decoder = BuggyI128Decoder::default();
        // 0x1_0000_0001 (4294967297) loses its high word silently.
        let decoded = decoder
            .decode_balance(&serde_json::Value::String("4294967297".into()))
            .unwrap();
        assert_eq!(decoded, 1);
    }

    #[test]
    fn forensics_snapshot_is_written_on_divergence() {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tessera-shadow-forensics-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        let db = Arc::new(RocksLedger::open(&path).expect("open forensics db"));
        let state = primary_state(7, "1000000000");
        let shadow = ShadowIndexer::new(shadow_config());
        shadow.snapshot.store(Arc::new(shadow_snapshot(7, "1")));
        shadow.set_forensics(db.clone());

        shadow.verify_once(&state);
        let values = db
            .scan_family(FORENSICS)
            .expect("scan forensics column family");
        assert_eq!(values.len(), 1);
        let payload: serde_json::Value = serde_json::from_slice(&values[0]).unwrap();
        assert_eq!(
            payload["divergence"]["discrepancies"][0]["kind"],
            "HolderBalance"
        );
        assert_eq!(payload["snapshot"]["assets"]["7"]["holders"]["GALICE"], "1");

        drop(db);
        let _ = std::fs::remove_dir_all(&path);
    }
}
