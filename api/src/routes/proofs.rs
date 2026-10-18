//! Light-client MMR proofs (issue #318).
//!
//! - `GET /v1/proofs/mmr` — the current MMR root and leaf count.
//! - `GET /v1/proofs/mmr/:ledger_seq?account=:address` — the checkpoint for
//!   `ledger_seq` with its MMR inclusion path, plus (when `account` is given)
//!   every cap-table entry of that account at that ledger with its path to
//!   the checkpoint's `contract_state_root`.
//!
//! See `crate::indexer::mmr_checkpoint` for the hashing rules, and
//! `sdk/typescript/src/mmr.ts` for the client-side verifier.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;

use super::ApiError;
use crate::indexer::mmr_checkpoint::{
    hex32, mmr_leaf_hash, AccountProof, MmrProofResponse, MmrRootResponse, StateEntryProof,
};
use crate::indexer::AppState;

#[derive(Debug, Deserialize)]
pub struct ProofQuery {
    pub account: Option<String>,
}

pub async fn root(State(state): State<AppState>) -> Json<MmrRootResponse> {
    let mmr = state.mmr.read().expect("mmr lock");
    Json(MmrRootResponse {
        leaf_count: mmr.leaf_count(),
        root: hex32(&mmr.root()),
        latest_ledger_sequence: mmr.latest().map(|cp| cp.ledger_sequence),
    })
}

pub async fn proof(
    State(state): State<AppState>,
    Path(ledger_seq): Path<u32>,
    Query(query): Query<ProofQuery>,
) -> Result<Json<MmrProofResponse>, ApiError> {
    let mmr = state.mmr.read().expect("mmr lock");
    let (cp, proof) = mmr
        .proof(ledger_seq)
        .ok_or_else(|| ApiError::NotFound(format!("no MMR checkpoint for ledger {ledger_seq}")))?;

    let account = match query.account {
        None => None,
        Some(address) => {
            if address.is_empty() || address.len() > 128 {
                return Err(ApiError::BadRequest("invalid account".into()));
            }
            let tree = mmr.state(ledger_seq).ok_or_else(|| {
                ApiError::NotFound(format!(
                    "cap-table state for ledger {ledger_seq} is past the retention window; \
                     only the MMR inclusion proof is available"
                ))
            })?;
            let entries = tree
                .proofs_for(&address)
                .into_iter()
                .map(|(entry, leaf_index, path)| StateEntryProof {
                    asset_id: entry.asset_id,
                    address: entry.address,
                    balance: entry.balance.to_string(),
                    leaf_index,
                    path: path.iter().map(hex32).collect(),
                })
                .collect();
            Some(AccountProof {
                address,
                state_leaf_count: tree.len() as u64,
                entries,
            })
        }
    };

    Ok(Json(MmrProofResponse {
        ledger_sequence: cp.ledger_sequence,
        ledger_hash: hex32(&cp.ledger_hash),
        contract_state_root: hex32(&cp.state_root),
        leaf_hash: hex32(&mmr_leaf_hash(
            cp.ledger_sequence,
            &cp.ledger_hash,
            &cp.state_root,
        )),
        leaf_index: proof.leaf_index,
        leaf_count: proof.leaf_count,
        path: proof.path.iter().map(hex32).collect(),
        peaks: proof.peaks.iter().map(hex32).collect(),
        root: hex32(&mmr.root()),
        account,
    }))
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use crate::indexer::mmr_checkpoint::{StateEntry, StateTree};
    use crate::indexer::Snapshot;
    use crate::routes::{router, test_support::state_with};

    async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let res = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    #[tokio::test]
    async fn serves_checkpoint_and_account_proofs() {
        let state = state_with(Snapshot::default());
        {
            let mut mmr = state.mmr.write().unwrap();
            for seq in 1..=5u32 {
                let tree = StateTree::new(vec![
                    StateEntry {
                        asset_id: 1,
                        address: "GALICE".into(),
                        balance: 10 * seq as i128,
                    },
                    StateEntry {
                        asset_id: 1,
                        address: "GBOB".into(),
                        balance: 3,
                    },
                ]);
                mmr.record(seq, [seq as u8; 32], tree).unwrap();
            }
        }
        let app = router(state);

        let (status, root) = get_json(app.clone(), "/v1/proofs/mmr").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(root["leaf_count"], 5);
        assert_eq!(root["latest_ledger_sequence"], 5);

        let (status, body) = get_json(app.clone(), "/v1/proofs/mmr/3?account=GALICE").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ledger_sequence"], 3);
        assert_eq!(body["leaf_index"], 2);
        assert_eq!(body["root"], root["root"]);
        assert_eq!(body["account"]["entries"][0]["balance"], "30");
        assert_eq!(body["account"]["state_leaf_count"], 2);

        let (status, body) = get_json(app.clone(), "/v1/proofs/mmr/3").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.get("account").is_none());

        let (status, _) = get_json(app, "/v1/proofs/mmr/99").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
