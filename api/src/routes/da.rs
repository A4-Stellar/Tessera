//! `GET /v1/assets/:id/da-proof` — EigenDA retrieval proof (issue #469).
//!
//! The indexer disperses one compressed blob per cycle containing the full
//! state snapshot, keyed under asset id `0`. An asset-specific request returns
//! that proof when no dedicated proof exists; `covers_full_snapshot` tells the
//! client that the blob commits to the whole snapshot (including this asset).

use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::json;

use crate::{indexer::AppState, routes::ApiError};

pub async fn da_proof(
    State(state): State<AppState>,
    Path(asset_id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = state
        .da_store
        .as_ref()
        .ok_or_else(|| ApiError::Unavailable("EigenDA is not configured".into()))?;

    let dedicated = store.latest(asset_id).await?;
    let (proof, covers_full_snapshot) = match dedicated {
        Some(proof) => (proof, false),
        None => (
            store
                .latest(0)
                .await?
                .ok_or_else(|| ApiError::NotFound("no DA proof has been dispersed yet".into()))?,
            true,
        ),
    };
    if proof.quorum_threshold < crate::indexer::eigenda::QUORUM_THRESHOLD_PERCENT {
        return Err(ApiError::Unavailable(
            "DA proof quorum is below the required threshold".into(),
        ));
    }

    // Attach the MMR↔DA anchor when this node has one for the proof's ledger.
    let anchor = state.mmr.read().ok().and_then(|checkpoints| {
        checkpoints
            .checkpoint(proof.ledger_sequence as u32)
            .and_then(|cp| checkpoints.da_anchor_for(cp.leaf_index))
            .cloned()
    });

    Ok(Json(json!({
        "asset_id": asset_id,
        "ledger_sequence": proof.ledger_sequence,
        "batch_header_hash": proof.batch_header_hash,
        "blob_index": proof.blob_index,
        "blob_hash": proof.blob_hash,
        "mmr_root": proof.mmr_root,
        "quorum_threshold": proof.quorum_threshold,
        "status": proof.status,
        "covers_full_snapshot": covers_full_snapshot,
        "proof": proof.proof,
        "mmr_da_anchor": anchor,
        "created_at": proof.created_at,
    })))
}
