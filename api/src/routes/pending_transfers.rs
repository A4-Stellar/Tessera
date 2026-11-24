//! Board approval of large transfers (issue #478).
//!
//! - `GET  /v1/assets/:id/pending-transfers[?signer=G...]` lists transfers
//!   waiting for board signatures, with the policy and who still has to sign.
//! - `POST /v1/assets/:id/pending-transfers/:transfer_id/sign` relays a board
//!   member's signed `co_sign(signer, transfer_id)` transaction to Soroban RPC.
//!
//! The API never holds board keys: the member signs in their wallet (the docs
//! site uses Freighter) and posts the signed envelope. Every relayed signature
//! is recorded in the audit trail. The contract emits `ms_sign`/`ms_exec`
//! events, which the indexer records too.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::ApiError;
use crate::audit::AuditAction;
use crate::indexer::AppState;
use crate::services::multisig::{check_co_sign_envelope, BoardPolicy, PendingTransferView};

#[derive(Debug, Default, Deserialize)]
pub struct PendingQuery {
    /// When set, each transfer reports whether this address can still sign.
    pub signer: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PendingTransferItem {
    #[serde(flatten)]
    pub transfer: PendingTransferView,
    pub signatures: usize,
    /// Ledgers left before the request expires.
    pub expires_in_ledgers: u32,
    /// Board members who have not signed yet.
    pub awaiting: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_sign: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct PendingTransfersResponse {
    pub asset_id: u64,
    pub compliance_contract: String,
    pub ledger: u32,
    pub policy: Option<BoardPolicy>,
    pub pending: Vec<PendingTransferItem>,
}

fn item(
    t: PendingTransferView,
    policy: Option<&BoardPolicy>,
    ledger: u32,
    signer: Option<&str>,
) -> PendingTransferItem {
    let awaiting: Vec<String> = policy
        .map(|p| {
            p.board
                .iter()
                .filter(|m| !t.signers.contains(m))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let current = policy.is_some_and(|p| p.version == t.policy_version);
    let can_sign = signer.map(|s| current && awaiting.iter().any(|m| m == s));
    PendingTransferItem {
        signatures: t.signers.len(),
        expires_in_ledgers: t.expires_at_ledger.saturating_sub(ledger),
        awaiting,
        can_sign,
        transfer: t,
    }
}

fn compliance_contract(state: &AppState, id: u64) -> Result<String, ApiError> {
    state
        .snapshot()
        .asset(id)
        .map(|a| a.compliance_contract.clone())
        .ok_or_else(|| ApiError::NotFound(format!("no asset with id {id}")))
}

/// `GET /v1/assets/:id/pending-transfers`
pub async fn list(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(query): Query<PendingQuery>,
) -> Result<Json<PendingTransfersResponse>, ApiError> {
    let compliance = compliance_contract(&state, id)?;
    let ledger = state.last_indexed_ledger();
    let policy = state.multisig.policy(&compliance);
    let pending = state
        .multisig
        .pending(&compliance, ledger)
        .into_iter()
        .map(|t| item(t, policy.as_ref(), ledger, query.signer.as_deref()))
        .collect();
    Ok(Json(PendingTransfersResponse {
        asset_id: id,
        compliance_contract: compliance,
        ledger,
        policy,
        pending,
    }))
}

#[derive(Debug, Deserialize)]
pub struct SignRequest {
    /// The board member's address; must match the `co_sign` argument.
    pub signer: String,
    /// Base64 `TransactionEnvelope` invoking `co_sign(signer, transfer_id)`,
    /// signed by the board member.
    pub signed_transaction_xdr: String,
}

#[derive(Debug, Serialize)]
pub struct SignResponse {
    pub transfer_id: u64,
    pub signer: String,
    pub transaction_hash: String,
    pub rpc_status: String,
    /// Signatures once this one lands, and how many are needed.
    pub signatures: usize,
    pub required_signatures: u32,
    /// `true` when this signature reaches the threshold, so the contract
    /// executes the transfer in the same transaction.
    pub executes_transfer: bool,
}

/// `POST /v1/assets/:id/pending-transfers/:transfer_id/sign`
pub async fn sign(
    State(state): State<AppState>,
    Path((id, transfer_id)): Path<(u64, u64)>,
    Json(req): Json<SignRequest>,
) -> Result<(StatusCode, Json<SignResponse>), ApiError> {
    let compliance = compliance_contract(&state, id)?;
    let ledger = state.last_indexed_ledger();
    let transfer = state
        .multisig
        .get(&compliance, transfer_id)
        .ok_or_else(|| {
            ApiError::NotFound(format!("no pending transfer {transfer_id} for asset {id}"))
        })?;
    if transfer.status != crate::services::multisig::PendingStatus::Pending {
        return Err(ApiError::Conflict(format!(
            "transfer {transfer_id} was already executed"
        )));
    }
    if ledger > transfer.expires_at_ledger {
        return Err(ApiError::Gone(format!(
            "transfer {transfer_id} expired at ledger {}",
            transfer.expires_at_ledger
        )));
    }
    let policy = state
        .multisig
        .policy(&compliance)
        .ok_or_else(|| ApiError::Unavailable("board policy not indexed yet".into()))?;
    if policy.version != transfer.policy_version {
        return Err(ApiError::Conflict(
            "the board policy changed after this transfer was requested; request it again".into(),
        ));
    }
    if !policy.board.contains(&req.signer) {
        return Err(ApiError::Forbidden(format!(
            "{} is not a board signer",
            req.signer
        )));
    }
    if transfer.signers.contains(&req.signer) {
        return Err(ApiError::Conflict(format!(
            "{} already signed transfer {transfer_id}",
            req.signer
        )));
    }
    check_co_sign_envelope(
        &req.signed_transaction_xdr,
        &compliance,
        &req.signer,
        transfer_id,
    )
    .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let submission = state
        .tx_submitter
        .submit(&req.signed_transaction_xdr)
        .await
        .map_err(ApiError::Unavailable)?;
    match submission.status.as_str() {
        "PENDING" | "DUPLICATE" => {}
        "TRY_AGAIN_LATER" => {
            return Err(ApiError::Unavailable(
                "Soroban RPC asked to retry later".into(),
            ))
        }
        other => {
            return Err(ApiError::BadRequest(format!(
                "transaction rejected ({other}){}",
                submission
                    .error_result_xdr
                    .as_deref()
                    .map(|x| format!(": {x}"))
                    .unwrap_or_default()
            )))
        }
    }

    let signatures = transfer.signers.len() + 1;
    let executes_transfer = signatures as u32 >= policy.required_signatures;
    let _ = state
        .audit
        .append(
            AuditAction::AdministrativeAction {
                action: "multisig.co_sign_submitted".into(),
                details: json!({
                    "asset_id": id,
                    "transfer_id": transfer_id,
                    "signer": req.signer,
                    "transaction_hash": submission.hash,
                    "signatures": signatures,
                    "required_signatures": policy.required_signatures,
                }),
            },
            req.signer.clone(),
            Some(compliance),
            json!({ "ledger": ledger }),
        )
        .await;

    Ok((
        StatusCode::ACCEPTED,
        Json(SignResponse {
            transfer_id,
            signer: req.signer,
            transaction_hash: submission.hash,
            rpc_status: submission.status,
            signatures,
            required_signatures: policy.required_signatures,
            executes_transfer,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::{body::Body, http::Request, routing::get, Router};
    use tower::ServiceExt as _;

    use super::*;
    use crate::indexer::Snapshot;
    use crate::routes::test_support::{asset, state_with};
    use crate::services::multisig::tests::{
        account, co_sign_args, envelope, seeded_book, COMPLIANCE,
    };
    use crate::services::multisig::{Submission, TransactionSubmitter};

    #[derive(Default)]
    struct FakeRpc(Mutex<Vec<String>>);

    #[async_trait]
    impl TransactionSubmitter for FakeRpc {
        async fn submit(&self, envelope_b64: &str) -> Result<Submission, String> {
            self.0.lock().unwrap().push(envelope_b64.to_string());
            Ok(Submission {
                hash: "ab".repeat(32),
                status: "PENDING".into(),
                error_result_xdr: None,
            })
        }
    }

    fn app() -> (Router, AppState, Arc<FakeRpc>) {
        let mut a = asset(1);
        a.compliance_contract = COMPLIANCE.into();
        let mut state = state_with(Snapshot {
            assets: vec![a],
            ..Snapshot::default()
        });
        state.multisig = Arc::new(seeded_book());
        let rpc = Arc::new(FakeRpc::default());
        state.tx_submitter = rpc.clone();
        let router = Router::new()
            .route("/v1/assets/:id/pending-transfers", get(list))
            .route(
                "/v1/assets/:id/pending-transfers/:tid/sign",
                axum::routing::post(sign),
            )
            .with_state(state.clone());
        (router, state, rpc)
    }

    async fn call(app: Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    fn post_sign(signer: &str, xdr: String) -> Request<Body> {
        Request::post("/v1/assets/1/pending-transfers/1/sign")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "signer": signer, "signed_transaction_xdr": xdr }).to_string(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn lists_pending_transfers_with_remaining_signers() {
        let (app, _, _) = app();
        let uri = format!("/v1/assets/1/pending-transfers?signer={}", account(2));
        let (status, body) = call(app, Request::get(uri).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["policy"]["required_signatures"], 3);
        let t = &body["pending"][0];
        assert_eq!(t["id"], 1);
        assert_eq!(t["signatures"], 1);
        assert_eq!(t["awaiting"].as_array().unwrap().len(), 4);
        assert_eq!(t["can_sign"], true);
    }

    #[tokio::test]
    async fn relays_a_valid_signature_and_audits_it() {
        let (app, state, rpc) = app();
        let signer = account(2);
        let xdr = envelope(COMPLIANCE, "co_sign", co_sign_args(&signer, 1), true);
        let (status, body) = call(app, post_sign(&signer, xdr.clone())).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["signatures"], 2);
        assert_eq!(body["executes_transfer"], false);
        assert_eq!(rpc.0.lock().unwrap().as_slice(), [xdr]);
        let (entries, _) = state.audit.list_entries(0, 10).await;
        assert!(entries.iter().any(|e| matches!(&e.action,
            AuditAction::AdministrativeAction { action, .. } if action == "multisig.co_sign_submitted")));
    }

    #[tokio::test]
    async fn refuses_non_members_repeat_signers_and_wrong_transactions() {
        let (app, _, rpc) = app();

        let outsider = account(30);
        let xdr = envelope(COMPLIANCE, "co_sign", co_sign_args(&outsider, 1), true);
        assert_eq!(
            call(app.clone(), post_sign(&outsider, xdr)).await.0,
            StatusCode::FORBIDDEN
        );

        let already = account(1);
        let xdr = envelope(COMPLIANCE, "co_sign", co_sign_args(&already, 1), true);
        assert_eq!(
            call(app.clone(), post_sign(&already, xdr)).await.0,
            StatusCode::CONFLICT
        );

        let signer = account(2);
        let xdr = envelope(
            COMPLIANCE,
            "request_transfer",
            co_sign_args(&signer, 1),
            true,
        );
        assert_eq!(
            call(app.clone(), post_sign(&signer, xdr)).await.0,
            StatusCode::BAD_REQUEST
        );

        let uri = "/v1/assets/1/pending-transfers/99/sign";
        let req = Request::post(uri)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "signer": signer, "signed_transaction_xdr": "x" }).to_string(),
            ))
            .unwrap();
        assert_eq!(call(app, req).await.0, StatusCode::NOT_FOUND);

        assert!(
            rpc.0.lock().unwrap().is_empty(),
            "nothing invalid reaches the RPC"
        );
    }
}
