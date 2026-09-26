/* Authorized Protocol Quality Assurance & Formal Verification Test Suite */
//! Route handlers for the Cryptographic Audit Logging Engine (Issue #73).
//!
//! Exposes:
//! - `GET /v1/audit/verify`: Mathematical proof and validation of log integrity.
//! - `GET /v1/audit/entries`: Paginated list of audit trail entries.
//! - `GET /v1/audit/entries/:sequence`: Single entry with cryptographic Merkle proof.
//! - `GET /v1/audit/anchors`: Published Stellar ledger root hash anchors.
//! - `POST /v1/audit/anchor`: Publish current root hash to the Stellar ledger.
//! - `POST /v1/audit/entries`: Record a new administrative action into the append-only log.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use super::ApiError;
use crate::audit::{AnchorRecord, AuditAction, AuditEntry, AuditVerificationResult, MerkleProof};
use crate::indexer::AppState;

const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_PAGE_SIZE: usize = 100;

/// Query parameters for `GET /v1/audit/verify`.
#[derive(Debug, Deserialize, Default)]
pub struct VerifyQuery {
    /// Optional sequence number to generate an inclusion Merkle proof for.
    pub entry_seq: Option<u64>,
}

/// Query parameters for `GET /v1/audit/entries`.
#[derive(Debug, Deserialize, Default)]
pub struct EntriesQuery {
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Paginated audit entries response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedEntriesResponse {
    pub entries: Vec<AuditEntry>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
}

/// Detailed single entry response with cryptographic Merkle proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryDetailResponse {
    pub entry: AuditEntry,
    pub inclusion_proof: Option<MerkleProof>,
}

/// Request body for `POST /v1/audit/entries`.
#[derive(Debug, Deserialize)]
pub struct CreateAuditEntryRequest {
    pub action: AuditAction,
    pub actor: String,
    pub target: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Request body for `POST /v1/audit/anchor`.
#[derive(Debug, Deserialize, Default)]
pub struct PublishAnchorRequest {
    pub ledger_sequence: Option<u32>,
}

/// Mathematical verification endpoint allowing external auditors to prove log integrity.
///
/// `GET /v1/audit/verify`
pub async fn verify(
    State(state): State<AppState>,
    Query(query): Query<VerifyQuery>,
) -> Result<Json<AuditVerificationResult>, ApiError> {
    let result = state.audit.verify_integrity(query.entry_seq).await;
    Ok(Json(result))
}

/// Paginated retrieval of audit log entries.
///
/// `GET /v1/audit/entries`
pub async fn list_entries(
    State(state): State<AppState>,
    Query(query): Query<EntriesQuery>,
) -> Result<Json<PaginatedEntriesResponse>, ApiError> {
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE).min(MAX_PAGE_SIZE);

    let (entries, total) = state.audit.list_entries(offset, limit).await;

    Ok(Json(PaginatedEntriesResponse {
        entries,
        total,
        offset,
        limit,
    }))
}

/// Fetch a single audit log entry by sequence, including its Merkle inclusion proof.
///
/// `GET /v1/audit/entries/:sequence`
pub async fn get_entry(
    State(state): State<AppState>,
    Path(sequence): Path<u64>,
) -> Result<Json<EntryDetailResponse>, ApiError> {
    let entry = state.audit.get_entry(sequence).await.ok_or_else(|| {
        ApiError::NotFound(format!("Audit entry with sequence {sequence} not found"))
    })?;

    let tree = state.audit.current_merkle_tree().await;
    let inclusion_proof = if sequence > 0 {
        tree.generate_proof((sequence - 1) as usize)
    } else {
        None
    };

    Ok(Json(EntryDetailResponse {
        entry,
        inclusion_proof,
    }))
}

/// List all published root hash anchors on the Stellar ledger.
///
/// `GET /v1/audit/anchors`
pub async fn list_anchors(
    State(state): State<AppState>,
) -> Result<Json<Vec<AnchorRecord>>, ApiError> {
    let anchors = state.audit.list_anchors().await;
    Ok(Json(anchors))
}

/// Publish the current audit log root hash to the Stellar ledger as a 32-byte memo hash.
///
/// `POST /v1/audit/anchor`
pub async fn publish_anchor(
    State(state): State<AppState>,
    Json(payload): Json<PublishAnchorRequest>,
) -> Result<Json<AnchorRecord>, ApiError> {
    let ledger_seq = payload
        .ledger_sequence
        .unwrap_or_else(|| state.last_indexed_ledger());

    let record = state
        .audit
        .publish_anchor(ledger_seq)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    Ok(Json(record))
}

/// Record a new administrative action into the tamper-proof append-only audit log.
///
/// `POST /v1/audit/entries`
pub async fn create_entry(
    State(state): State<AppState>,
    Json(payload): Json<CreateAuditEntryRequest>,
) -> Result<Json<AuditEntry>, ApiError> {
    let entry = state
        .audit
        .append(
            payload.action,
            payload.actor,
            payload.target,
            payload.payload,
        )
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    Ok(Json(entry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    };
    use serde_json::json;
    use tower::ServiceExt as _;

    fn test_app() -> (Router, AppState) {
        let state = AppState::for_test_empty();
        let app = Router::new()
            .route("/v1/audit/verify", axum::routing::get(verify))
            .route(
                "/v1/audit/entries",
                axum::routing::get(list_entries).post(create_entry),
            )
            .route("/v1/audit/entries/:sequence", axum::routing::get(get_entry))
            .route("/v1/audit/anchors", axum::routing::get(list_anchors))
            .route("/v1/audit/anchor", axum::routing::post(publish_anchor))
            .with_state(state.clone());
        (app, state)
    }

    #[tokio::test]
    async fn test_route_audit_verify_empty() {
        let (app, _) = test_app();
        let req = Request::builder()
            .uri("/v1/audit/verify")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let val: AuditVerificationResult = serde_json::from_slice(&bytes).unwrap();
        assert!(val.is_valid);
        assert_eq!(val.total_entries, 0);
        assert_eq!(val.genesis_hash, crate::audit::GENESIS_HASH);
    }

    #[tokio::test]
    async fn test_route_create_and_get_entry() {
        let (app, _) = test_app();

        let create_body = json!({
            "action": {
                "type": "token_freeze",
                "address": "GXYZ12345",
                "asset_id": 1
            },
            "actor": "admin_test",
            "target": "asset_1",
            "payload": {"reason": "regulatory_compliance"}
        });

        let req = Request::builder()
            .method("POST")
            .uri("/v1/audit/entries")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&create_body).unwrap()))
            .unwrap();

        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let created: AuditEntry = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(created.sequence, 1);
        assert_eq!(created.actor, "admin_test");

        // GET /v1/audit/entries/1
        let req = Request::builder()
            .uri("/v1/audit/entries/1")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let detail: EntryDetailResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(detail.entry.sequence, 1);
        assert!(detail.inclusion_proof.is_some());
        let proof = detail.inclusion_proof.unwrap();
        assert!(proof.verify(&proof.root_hash));

        // GET /v1/audit/entries/999 -> 404
        let req = Request::builder()
            .uri("/v1/audit/entries/999")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_route_publish_anchor_and_list() {
        let (app, _) = test_app();

        // 1. Create an entry so anchor has data
        let create_body = json!({
            "action": {
                "type": "emergency_pause",
                "component": "AMM",
                "reason": "Test"
            },
            "actor": "admin",
            "payload": {}
        });

        let req = Request::builder()
            .method("POST")
            .uri("/v1/audit/entries")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&create_body).unwrap()))
            .unwrap();
        let _ = app.clone().oneshot(req).await.unwrap();

        // 2. Publish anchor
        let anchor_body = json!({
            "ledger_sequence": 123456
        });
        let req = Request::builder()
            .method("POST")
            .uri("/v1/audit/anchor")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&anchor_body).unwrap()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let anchor: AnchorRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(anchor.anchor_id, 1);
        assert_eq!(anchor.ledger_sequence, 123456);

        // 3. List anchors
        let req = Request::builder()
            .uri("/v1/audit/anchors")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let anchors: Vec<AnchorRecord> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].root_hash, anchor.root_hash);

        // 4. Verify with entry_seq
        let req = Request::builder()
            .uri("/v1/audit/verify?entry_seq=1")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let verification: AuditVerificationResult = serde_json::from_slice(&bytes).unwrap();
        assert!(verification.is_valid);
        assert_eq!(verification.anchor_status, "SynchronizedAndAnchored");
        assert!(verification.inclusion_proof.is_some());
    }

    #[tokio::test]
    async fn test_route_list_entries_pagination() {
        let (app, _) = test_app();

        for i in 1..=4 {
            let create_body = json!({
                "action": {
                    "type": "administrative_action",
                    "action": format!("action_{}", i),
                    "details": {}
                },
                "actor": "admin",
                "payload": {}
            });

            let req = Request::builder()
                .method("POST")
                .uri("/v1/audit/entries")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&create_body).unwrap()))
                .unwrap();
            let _ = app.clone().oneshot(req).await.unwrap();
        }

        let req = Request::builder()
            .uri("/v1/audit/entries?offset=1&limit=2")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let paged: PaginatedEntriesResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(paged.total, 4);
        assert_eq!(paged.offset, 1);
        assert_eq!(paged.limit, 2);
        assert_eq!(paged.entries.len(), 2);
        assert_eq!(paged.entries[0].sequence, 2);
        assert_eq!(paged.entries[1].sequence, 3);
    }
}
