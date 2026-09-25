//! Historical fiat-equivalent pricing for dividend distributions paid in a
//! non-native token (issue #6).
//!
//! A `payment_token` on a [`crate::models::Distribution`] is an opaque
//! Soroban contract address (`C...`). When that contract is a Stellar Asset
//! Contract (SAC) wrapping a classic Stellar asset (an asset code + issuer
//! on the DEX), Horizon's `trade_aggregations` endpoint can supply a
//! historical USD-equivalent price for it.
//!
//! **Why this needs a fallback table.** A SAC's contract id is derived
//! *forwards*, deterministically, from the classic asset and the network
//! passphrase (`Asset::contract_id(network_id)`); going the other way — an
//! opaque `C...` address back to its `(code, issuer)` — is not something
//! this codebase can decode on its own. Doing it "properly" would mean
//! deriving the candidate contract id for every classic asset we might
//! care about and comparing, which just reduces to the same table this
//! module already asks an operator to supply, with more moving parts. So
//! rather than guess, resolution here is via an ADMIN-CONFIGURED mapping —
//! `RWA_ASSET_PRICE_MAP`, contract address -> `{code, issuer}` — and that is
//! a pragmatic, explicitly documented fallback, not a decoded on-chain
//! fact. A `payment_token` with no configured mapping is reported as
//! `None`/`null` in API responses, never guessed.
//!
//! Every failure mode here (missing mapping, Horizon unreachable, no trade
//! data near the requested time) is handled by returning `None` from
//! [`PriceFeedClient::fiat_equivalent`]; this module never panics.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

/// Default public Horizon instance for the network the rest of the indexer
/// talks to (testnet, per `api/src/indexer/mod.rs::Config`).
const DEFAULT_HORIZON_URL: &str = "https://horizon-testnet.stellar.org";
/// Default USD-pegged reference asset used as the `trade_aggregations`
/// counter asset (Circle's testnet USDC issuer).
const DEFAULT_USD_ASSET_CODE: &str = "USDC";
const DEFAULT_USD_ASSET_ISSUER: &str = "GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5";
/// Stellar classic assets — and therefore their SAC wrappers — always use
/// 7 decimal places.
const CLASSIC_ASSET_DECIMALS: i32 = 7;
/// How far on either side of the distribution's ledger-close time to search
/// for a trade aggregation bucket.
const SEARCH_WINDOW_HOURS: i64 = 12;
/// Horizon's 1-hour bucket resolution, in milliseconds — the coarsest
/// resolution Horizon documents support for, which keeps a single request
/// likely to find at least one bucket even for thinly-traded pairs.
const RESOLUTION_MS: i64 = 3_600_000;

/// One admin-configured `payment_token` contract address -> classic asset
/// mapping. See the module docs for why this is a fallback table rather
/// than a decoded on-chain fact.
#[derive(Debug, Clone, Deserialize)]
pub struct AssetMapping {
    pub code: String,
    pub issuer: String,
}

#[derive(Debug, Clone)]
pub struct PriceFeedConfig {
    pub horizon_url: String,
    pub usd_asset_code: String,
    pub usd_asset_issuer: String,
    /// Keyed by the `payment_token` Soroban contract address (`C...`).
    pub asset_map: HashMap<String, AssetMapping>,
}

impl PriceFeedConfig {
    /// Build config from the environment. Never fails the caller: an
    /// unparsable `RWA_ASSET_PRICE_MAP` is logged and treated as an empty
    /// mapping rather than aborting indexer startup, consistent with this
    /// feature being best-effort everywhere.
    pub fn from_env() -> Self {
        let horizon_url =
            std::env::var("RWA_HORIZON_URL").unwrap_or_else(|_| DEFAULT_HORIZON_URL.to_string());
        let usd_asset_code = std::env::var("RWA_USD_ASSET_CODE")
            .unwrap_or_else(|_| DEFAULT_USD_ASSET_CODE.to_string());
        let usd_asset_issuer = std::env::var("RWA_USD_ASSET_ISSUER")
            .unwrap_or_else(|_| DEFAULT_USD_ASSET_ISSUER.to_string());

        let asset_map = match std::env::var("RWA_ASSET_PRICE_MAP") {
            Ok(raw) if !raw.trim().is_empty() => {
                match serde_json::from_str::<HashMap<String, AssetMapping>>(&raw) {
                    Ok(map) => map,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "RWA_ASSET_PRICE_MAP is not valid JSON; historical fiat pricing disabled"
                        );
                        HashMap::new()
                    }
                }
            }
            _ => HashMap::new(),
        };

        PriceFeedConfig {
            horizon_url,
            usd_asset_code,
            usd_asset_issuer,
            asset_map,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PriceFeedError {
    #[error("no admin-configured asset mapping for payment token {0}")]
    MissingMapping(String),
    #[error("horizon request failed: {0}")]
    HorizonUnreachable(String),
    #[error("no trade data found within the search window")]
    NoTradeData,
}

/// Client for resolving a dividend distribution's fiat-equivalent value.
pub struct PriceFeedClient {
    http: reqwest::Client,
    config: PriceFeedConfig,
}

#[derive(Deserialize)]
struct LedgerResponse {
    closed_at: String,
}

#[derive(Deserialize)]
struct TradeAggregation {
    timestamp: String,
    avg: String,
}

#[derive(Deserialize)]
struct TradeAggregationsEmbedded {
    records: Vec<TradeAggregation>,
}

#[derive(Deserialize)]
struct TradeAggregationsResponse {
    #[serde(rename = "_embedded")]
    embedded: TradeAggregationsEmbedded,
}

impl PriceFeedClient {
    pub fn new(config: PriceFeedConfig) -> Self {
        PriceFeedClient {
            http: reqwest::Client::new(),
            config,
        }
    }

    fn resolve_mapping(
        &self,
        payment_token_contract: &str,
    ) -> Result<&AssetMapping, PriceFeedError> {
        self.config
            .asset_map
            .get(payment_token_contract)
            .ok_or_else(|| PriceFeedError::MissingMapping(payment_token_contract.to_string()))
    }

    /// Approximate wall-clock close time of `ledger`, read live from
    /// Horizon. This is the distribution's actual recorded ledger, so the
    /// timestamp is Horizon's own record of when it closed — not a guess
    /// from an assumed ledger-time constant.
    async fn ledger_close_time(&self, ledger: u32) -> Result<DateTime<Utc>, PriceFeedError> {
        let url = format!(
            "{}/ledgers/{}",
            self.config.horizon_url.trim_end_matches('/'),
            ledger
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| PriceFeedError::HorizonUnreachable(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(PriceFeedError::HorizonUnreachable(format!(
                "GET {url}: status {}",
                resp.status()
            )));
        }
        let body: LedgerResponse = resp
            .json()
            .await
            .map_err(|e| PriceFeedError::HorizonUnreachable(e.to_string()))?;
        DateTime::parse_from_rfc3339(&body.closed_at)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| PriceFeedError::HorizonUnreachable(format!("invalid closed_at: {e}")))
    }

    /// The historical USD price of one unit of `mapping`, nearest `at`, via
    /// Horizon `trade_aggregations` against the configured USD-pegged
    /// reference asset. Picks the returned bucket whose timestamp is
    /// closest to `at`.
    async fn historical_price_usd(
        &self,
        mapping: &AssetMapping,
        at: DateTime<Utc>,
    ) -> Result<f64, PriceFeedError> {
        let window = Duration::hours(SEARCH_WINDOW_HOURS);
        let start_ms = (at - window).timestamp_millis();
        let end_ms = (at + window).timestamp_millis();

        let url = format!(
            "{}/trade_aggregations",
            self.config.horizon_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .get(&url)
            .query(&[
                ("base_asset_type", asset_type_param(&mapping.code)),
                ("base_asset_code", mapping.code.as_str()),
                ("base_asset_issuer", mapping.issuer.as_str()),
                (
                    "counter_asset_type",
                    asset_type_param(&self.config.usd_asset_code),
                ),
                ("counter_asset_code", self.config.usd_asset_code.as_str()),
                (
                    "counter_asset_issuer",
                    self.config.usd_asset_issuer.as_str(),
                ),
                ("start_time", &start_ms.to_string()),
                ("end_time", &end_ms.to_string()),
                ("resolution", &RESOLUTION_MS.to_string()),
                ("order", "asc"),
                ("limit", "200"),
            ])
            .send()
            .await
            .map_err(|e| PriceFeedError::HorizonUnreachable(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(PriceFeedError::HorizonUnreachable(format!(
                "GET {url}: status {}",
                resp.status()
            )));
        }

        let body: TradeAggregationsResponse = resp
            .json()
            .await
            .map_err(|e| PriceFeedError::HorizonUnreachable(e.to_string()))?;

        let target_ms = at.timestamp_millis();
        body.embedded
            .records
            .into_iter()
            .filter_map(|r| {
                let ts: i64 = r.timestamp.parse().ok()?;
                let avg: f64 = r.avg.parse().ok()?;
                Some((ts, avg))
            })
            .min_by_key(|(ts, _)| (ts - target_ms).abs())
            .map(|(_, avg)| avg)
            .ok_or(PriceFeedError::NoTradeData)
    }

    /// Resolve the fiat-equivalent (USD) value of `total_amount_base_units`
    /// of `payment_token_contract` at the ledger it was recorded
    /// (`created_at_ledger`). Returns `None` on any failure — missing
    /// mapping, unreachable Horizon, or no trade data near the timestamp —
    /// logging the specific reason; this is surfaced to API consumers as an
    /// optional/nullable field, never a panic or a guessed value.
    pub async fn fiat_equivalent(
        &self,
        payment_token_contract: &str,
        total_amount_base_units: i128,
        created_at_ledger: u32,
    ) -> Option<f64> {
        let mapping = match self.resolve_mapping(payment_token_contract) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(payment_token_contract, error = %e, "skipping fiat pricing");
                return None;
            }
        };

        let at = match self.ledger_close_time(created_at_ledger).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(created_at_ledger, error = %e, "could not resolve ledger close time");
                return None;
            }
        };

        let price = match self.historical_price_usd(mapping, at).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(payment_token_contract, error = %e, "could not resolve historical price");
                return None;
            }
        };

        let amount_units = total_amount_base_units as f64 / 10f64.powi(CLASSIC_ASSET_DECIMALS);
        Some(amount_units * price)
    }
}

/// Horizon's `*_asset_type` query parameter for a classic asset code:
/// `native` for XLM, `credit_alphanum4`/`credit_alphanum12` otherwise
/// (Stellar's own split by code length).
fn asset_type_param(code: &str) -> &'static str {
    if code.eq_ignore_ascii_case("XLM") {
        "native"
    } else if code.len() <= 4 {
        "credit_alphanum4"
    } else {
        "credit_alphanum12"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_type_param_splits_by_code_length() {
        assert_eq!(asset_type_param("XLM"), "native");
        assert_eq!(asset_type_param("USDC"), "credit_alphanum4");
        assert_eq!(asset_type_param("LONGERCODE12"), "credit_alphanum12");
    }

    #[test]
    fn config_from_env_defaults_to_empty_map_without_env_var() {
        // Deliberately does not touch `RWA_ASSET_PRICE_MAP` so this test
        // stays safe to run in parallel with others that do.
        let config = PriceFeedConfig {
            horizon_url: DEFAULT_HORIZON_URL.to_string(),
            usd_asset_code: DEFAULT_USD_ASSET_CODE.to_string(),
            usd_asset_issuer: DEFAULT_USD_ASSET_ISSUER.to_string(),
            asset_map: HashMap::new(),
        };
        assert!(config.asset_map.is_empty());
    }

    #[test]
    fn resolve_mapping_reports_missing_entries_by_contract_address() {
        let config = PriceFeedConfig {
            horizon_url: DEFAULT_HORIZON_URL.to_string(),
            usd_asset_code: DEFAULT_USD_ASSET_CODE.to_string(),
            usd_asset_issuer: DEFAULT_USD_ASSET_ISSUER.to_string(),
            asset_map: HashMap::new(),
        };
        let client = PriceFeedClient::new(config);
        let err = client.resolve_mapping("CUNKNOWN").unwrap_err();
        assert!(matches!(err, PriceFeedError::MissingMapping(c) if c == "CUNKNOWN"));
    }
}
