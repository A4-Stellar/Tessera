//! EigenDA data-availability commitments (issue #469).
//!
//! After each indexer cycle the full state snapshot is serialised, compressed
//! with zstd, and dispersed to EigenDA with an 80% quorum threshold. We poll
//! until the blob is `Confirmed`, then persist the batch header hash and blob
//! index in Postgres together with a proof the Soroban verifier can check.
//!
//! The module is deliberately split into three traits — [`Disperser`],
//! [`ProofStore`] and the thin `commit_snapshot` orchestration — so the
//! network and database halves can be tested independently with in-memory
//! fakes.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest as _;
use sqlx::{PgPool, Row};

use crate::routes::ApiError;

/// EigenDA requires an 80% stake-weighted quorum before a blob is considered
/// available. This value is also enforced by the Soroban verifier contract.
pub const QUORUM_THRESHOLD_PERCENT: u8 = 80;
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub const DEFAULT_MAX_POLLS: u32 = 120;

#[derive(Debug, thiserror::Error)]
pub enum DaError {
    #[error("snapshot serialisation failed: {0}")]
    Serialisation(#[from] serde_json::Error),
    #[error("compression failed: {0}")]
    Compression(String),
    #[error("disperser request failed: {0}")]
    Disperser(String),
    #[error("blob was rejected: {0}")]
    Rejected(String),
    #[error("blob confirmation timed out after {0} polls")]
    Timeout(u32),
    #[error("database error: {0}")]
    Database(String),
    #[error("invalid configuration: {0}")]
    Config(String),
}

impl From<DaError> for ApiError {
    fn from(error: DaError) -> Self {
        ApiError::Unavailable(error.to_string())
    }
}

/// Serialise any snapshot to a compact, deterministic zstd blob.
pub fn serialise_blob<T: Serialize>(snapshot: &T) -> Result<Vec<u8>, DaError> {
    let json = serde_json::to_vec(snapshot)?;
    zstd::encode_all(json.as_slice(), 3).map_err(|e| DaError::Compression(e.to_string()))
}

#[expect(
    dead_code,
    reason = "retrieval-side counterpart of serialise_blob, used by the DA light-client reader"
)]
pub fn deserialise_blob<T: DeserializeOwned>(blob: &[u8]) -> Result<T, DaError> {
    let json = zstd::decode_all(blob).map_err(|e| DaError::Compression(e.to_string()))?;
    Ok(serde_json::from_slice(&json)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BlobStatus {
    Pending,
    Processing,
    Confirmed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisperseReply {
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlobStatusReply {
    pub status: BlobStatus,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub batch_header_hash: Option<String>,
    #[serde(default)]
    pub blob_index: Option<u64>,
    /// Aggregate BLS signature + stake snapshot from the disperser.
    #[serde(default)]
    pub proof: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetrieveReply {
    /// base64-encoded blob bytes.
    pub data: String,
}

#[async_trait]
pub trait Disperser: Send + Sync {
    async fn disperse(&self, blob: &[u8]) -> Result<DisperseReply, DaError>;
    async fn status(&self, request_id: &str) -> Result<BlobStatusReply, DaError>;
    async fn retrieve(&self, batch_header_hash: &str, blob_index: u64) -> Result<Vec<u8>, DaError>;
}

/// Real EigenDA disperser client. All endpoints are configurable so the same
/// code targets local devnets, testnet and mainnet.
pub struct HttpDisperser {
    client: reqwest::Client,
    base_url: String,
}

impl HttpDisperser {
    pub fn from_env() -> Result<Self, DaError> {
        let base_url = std::env::var("EIGENDA_DISPERSER_URL")
            .map_err(|_| DaError::Config("EIGENDA_DISPERSER_URL is not set".to_owned()))?;
        Ok(Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }
}

#[async_trait]
impl Disperser for HttpDisperser {
    async fn disperse(&self, blob: &[u8]) -> Result<DisperseReply, DaError> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(blob);
        let response = self
            .client
            .post(format!("{}/disperseBlob", self.base_url))
            .json(&json!({
                "data": encoded,
                "quorum_threshold": QUORUM_THRESHOLD_PERCENT,
            }))
            .send()
            .await
            .map_err(|e| DaError::Disperser(e.to_string()))?;
        if !response.status().is_success() {
            return Err(DaError::Disperser(format!(
                "disperseBlob returned {}",
                response.status()
            )));
        }
        response
            .json::<DisperseReply>()
            .await
            .map_err(|e| DaError::Disperser(e.to_string()))
    }

    async fn status(&self, request_id: &str) -> Result<BlobStatusReply, DaError> {
        let response = self
            .client
            .get(format!("{}/blobStatus", self.base_url))
            .query(&[("request_id", request_id)])
            .send()
            .await
            .map_err(|e| DaError::Disperser(e.to_string()))?;
        if !response.status().is_success() {
            return Err(DaError::Disperser(format!(
                "blobStatus returned {}",
                response.status()
            )));
        }
        response
            .json::<BlobStatusReply>()
            .await
            .map_err(|e| DaError::Disperser(e.to_string()))
    }

    async fn retrieve(&self, batch_header_hash: &str, blob_index: u64) -> Result<Vec<u8>, DaError> {
        let response = self
            .client
            .get(format!("{}/retrieveBlob", self.base_url))
            .query(&[
                ("batch_header_hash", batch_header_hash),
                ("blob_index", &blob_index.to_string()),
            ])
            .send()
            .await
            .map_err(|e| DaError::Disperser(e.to_string()))?;
        if !response.status().is_success() {
            return Err(DaError::Disperser(format!(
                "retrieveBlob returned {}",
                response.status()
            )));
        }
        let reply = response
            .json::<RetrieveReply>()
            .await
            .map_err(|e| DaError::Disperser(e.to_string()))?;
        base64::engine::general_purpose::STANDARD
            .decode(reply.data)
            .map_err(|e| DaError::Disperser(format!("invalid base64 blob: {e}")))
    }
}

/// Persisted proof record returned by `GET /v1/assets/:id/da-proof`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DaProof {
    pub asset_id: u64,
    pub ledger_sequence: u64,
    pub batch_header_hash: String,
    pub blob_index: u64,
    pub blob_hash: String,
    pub mmr_root: String,
    pub quorum_threshold: u8,
    pub status: String,
    pub proof: Value,
    pub created_at: Option<String>,
}

#[async_trait]
pub trait ProofStore: Send + Sync {
    async fn put(&self, proof: &DaProof) -> Result<(), DaError>;
    async fn latest(&self, asset_id: u64) -> Result<Option<DaProof>, DaError>;
}

pub struct PgProofStore {
    pool: PgPool,
}

impl PgProofStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ProofStore for PgProofStore {
    async fn put(&self, proof: &DaProof) -> Result<(), DaError> {
        sqlx::query(
            "INSERT INTO eigenda_da_proofs
                (asset_id, ledger_sequence, batch_header_hash, blob_index,
                 blob_hash, mmr_root, quorum_threshold, status, proof, confirmed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())
             ON CONFLICT (asset_id, ledger_sequence) DO UPDATE SET
                 batch_header_hash = EXCLUDED.batch_header_hash,
                 blob_index = EXCLUDED.blob_index,
                 blob_hash = EXCLUDED.blob_hash,
                 mmr_root = EXCLUDED.mmr_root,
                 quorum_threshold = EXCLUDED.quorum_threshold,
                 status = EXCLUDED.status,
                 proof = EXCLUDED.proof,
                 confirmed_at = NOW()",
        )
        .bind(proof.asset_id as i64)
        .bind(proof.ledger_sequence as i64)
        .bind(&proof.batch_header_hash)
        .bind(proof.blob_index as i64)
        .bind(&proof.blob_hash)
        .bind(&proof.mmr_root)
        .bind(proof.quorum_threshold as i32)
        .bind(&proof.status)
        .bind(&proof.proof)
        .execute(&self.pool)
        .await
        .map_err(|e| DaError::Database(e.to_string()))?;
        Ok(())
    }

    async fn latest(&self, asset_id: u64) -> Result<Option<DaProof>, DaError> {
        let row = sqlx::query(
            "SELECT asset_id, ledger_sequence, batch_header_hash, blob_index,
                    blob_hash, mmr_root, quorum_threshold, status, proof
             FROM eigenda_da_proofs WHERE asset_id = $1
             ORDER BY ledger_sequence DESC LIMIT 1",
        )
        .bind(asset_id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DaError::Database(e.to_string()))?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(DaProof {
            asset_id: row.try_get::<i64, _>("asset_id").unwrap_or(0) as u64,
            ledger_sequence: row.try_get::<i64, _>("ledger_sequence").unwrap_or(0) as u64,
            batch_header_hash: row.try_get("batch_header_hash").unwrap_or_default(),
            blob_index: row.try_get::<i64, _>("blob_index").unwrap_or(0) as u64,
            blob_hash: row.try_get("blob_hash").unwrap_or_default(),
            mmr_root: row.try_get("mmr_root").unwrap_or_default(),
            quorum_threshold: row.try_get::<i32, _>("quorum_threshold").unwrap_or(0) as u8,
            status: row.try_get("status").unwrap_or_default(),
            proof: row.try_get("proof").unwrap_or_else(|_| json!({})),
            created_at: None,
        }))
    }
}

/// Options controlling the confirmation poll.
#[derive(Debug, Clone, Copy)]
pub struct PollConfig {
    pub interval: Duration,
    pub max_polls: u32,
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            interval: DEFAULT_POLL_INTERVAL,
            max_polls: DEFAULT_MAX_POLLS,
        }
    }
}

/// Disperse a snapshot, wait for `Confirmed`, and persist the proof.
pub async fn commit_snapshot<T: Serialize>(
    disperser: &dyn Disperser,
    store: &dyn ProofStore,
    asset_id: u64,
    ledger_sequence: u64,
    mmr_root: &str,
    snapshot: &T,
    poll: PollConfig,
) -> Result<DaProof, DaError> {
    let blob = serialise_blob(snapshot)?;
    let blob_hash = format!("0x{}", hex::encode(sha2::Sha256::digest(&blob)));
    let reply = disperser.disperse(&blob).await?;

    let mut confirmed: Option<BlobStatusReply> = None;
    for _ in 0..poll.max_polls {
        let status = disperser.status(&reply.request_id).await?;
        match status.status {
            BlobStatus::Confirmed => {
                confirmed = Some(status);
                break;
            }
            BlobStatus::Failed => {
                return Err(DaError::Rejected(format!(
                    "blob {} was rejected",
                    reply.request_id
                )));
            }
            BlobStatus::Pending | BlobStatus::Processing => {
                tokio::time::sleep(poll.interval).await;
            }
        }
    }
    let confirmed = confirmed.ok_or(DaError::Timeout(poll.max_polls))?;
    let batch_header_hash = confirmed
        .batch_header_hash
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| DaError::Rejected("confirmed blob has no batch header hash".to_owned()))?;
    let blob_index = confirmed
        .blob_index
        .ok_or_else(|| DaError::Rejected("confirmed blob has no blob index".to_owned()))?;

    // Fail closed if a misconfigured disperser reports a weaker quorum.
    let reported_quorum = confirmed
        .proof
        .get("quorum_threshold")
        .and_then(Value::as_u64)
        .unwrap_or(QUORUM_THRESHOLD_PERCENT as u64) as u8;
    if reported_quorum < QUORUM_THRESHOLD_PERCENT {
        return Err(DaError::Rejected(format!(
            "disperser quorum {reported_quorum}% is below required {QUORUM_THRESHOLD_PERCENT}%"
        )));
    }

    let proof = DaProof {
        asset_id,
        ledger_sequence,
        batch_header_hash,
        blob_index,
        blob_hash,
        mmr_root: mmr_root.to_owned(),
        quorum_threshold: reported_quorum,
        status: "confirmed".to_owned(),
        proof: confirmed.proof,
        created_at: None,
    };
    store.put(&proof).await?;
    Ok(proof)
}

/// Domain-separated commitment binding an MMR root to a DA blob. Used by
/// `mmr_checkpoint.rs` so every checkpoint carries a DA anchor.
pub fn da_commitment_hash(
    mmr_root: &[u8; 32],
    batch_header_hash: &str,
    blob_index: u64,
) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"TESSERA-DA-COMMITMENT-V1");
    hasher.update(mmr_root);
    hasher.update(batch_header_hash.as_bytes());
    hasher.update(blob_index.to_be_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeDisperser {
        statuses: Arc<Mutex<Vec<BlobStatus>>>,
    }

    #[async_trait]
    impl Disperser for FakeDisperser {
        async fn disperse(&self, blob: &[u8]) -> Result<DisperseReply, DaError> {
            assert!(!blob.is_empty());
            Ok(DisperseReply {
                request_id: "request-1".to_owned(),
            })
        }

        async fn status(&self, _request_id: &str) -> Result<BlobStatusReply, DaError> {
            let mut statuses = self.statuses.lock().unwrap();
            let status = if statuses.is_empty() {
                BlobStatus::Confirmed
            } else {
                statuses.remove(0)
            };
            Ok(BlobStatusReply {
                status,
                request_id: Some("request-1".to_owned()),
                batch_header_hash: Some(format!("0x{}", "ab".repeat(32))),
                blob_index: Some(3),
                proof: json!({
                    "quorum_threshold": 80,
                    "aggregate_signature": "cc".repeat(64),
                    "stake_weights": [40, 35, 25],
                }),
            })
        }

        async fn retrieve(
            &self,
            _batch_header_hash: &str,
            _blob_index: u64,
        ) -> Result<Vec<u8>, DaError> {
            Ok(vec![1, 2, 3])
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        proofs: Arc<Mutex<Vec<DaProof>>>,
    }

    #[async_trait]
    impl ProofStore for MemoryStore {
        async fn put(&self, proof: &DaProof) -> Result<(), DaError> {
            self.proofs.lock().unwrap().push(proof.clone());
            Ok(())
        }
        async fn latest(&self, asset_id: u64) -> Result<Option<DaProof>, DaError> {
            Ok(self
                .proofs
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|p| p.asset_id == asset_id)
                .cloned())
        }
    }

    #[test]
    fn blob_round_trips_and_compresses() {
        let snapshot = json!({
            "ledger": 7,
            "holders": (0..100).map(|i| json!({ "address": format!("G{i}"), "balance": i })).collect::<Vec<_>>(),
        });
        let blob = serialise_blob(&snapshot).unwrap();
        let raw = serde_json::to_vec(&snapshot).unwrap();
        assert!(blob.len() < raw.len(), "zstd should shrink repetitive data");
        let decoded: Value = deserialise_blob(&blob).unwrap();
        assert_eq!(decoded, snapshot);
    }

    #[tokio::test]
    async fn commit_polls_until_confirmed_and_stores_the_proof() {
        let disperser = FakeDisperser {
            statuses: Arc::new(Mutex::new(vec![
                BlobStatus::Pending,
                BlobStatus::Processing,
                BlobStatus::Confirmed,
            ])),
        };
        let store = MemoryStore::default();
        let proof = commit_snapshot(
            &disperser,
            &store,
            42,
            100,
            "0x1234",
            &json!({"state": "snapshot"}),
            PollConfig {
                interval: Duration::ZERO,
                max_polls: 5,
            },
        )
        .await
        .unwrap();
        assert_eq!(proof.asset_id, 42);
        assert_eq!(proof.quorum_threshold, 80);
        assert_eq!(proof.blob_index, 3);
        assert_eq!(store.latest(42).await.unwrap().unwrap(), proof);
    }

    #[tokio::test]
    async fn rejects_a_weak_quorum() {
        struct WeakQuorum(FakeDisperser);
        #[async_trait]
        impl Disperser for WeakQuorum {
            async fn disperse(&self, blob: &[u8]) -> Result<DisperseReply, DaError> {
                self.0.disperse(blob).await
            }
            async fn status(&self, id: &str) -> Result<BlobStatusReply, DaError> {
                let mut reply = self.0.status(id).await.unwrap();
                reply.proof = json!({"quorum_threshold": 50});
                Ok(reply)
            }
            async fn retrieve(&self, h: &str, i: u64) -> Result<Vec<u8>, DaError> {
                self.0.retrieve(h, i).await
            }
        }
        let err = commit_snapshot(
            &WeakQuorum(FakeDisperser::default()),
            &MemoryStore::default(),
            1,
            1,
            "0x00",
            &json!({}),
            PollConfig::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DaError::Rejected(_)));
    }

    #[test]
    fn da_commitment_is_domain_separated_and_deterministic() {
        let root = [7u8; 32];
        let a = da_commitment_hash(&root, "0xabc", 1);
        let b = da_commitment_hash(&root, "0xabc", 1);
        assert_eq!(a, b);
        assert_ne!(a, da_commitment_hash(&root, "0xabc", 2));
        assert_ne!(a, da_commitment_hash(&[8u8; 32], "0xabc", 1));
    }
}
