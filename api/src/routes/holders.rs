//! `GET /assets/:id/holders`.

use axum::{extract::{Path, State}, Json};

use crate::indexer::AppState;
use crate::models::Holder;
use super::ApiError;

/// Holder list for an asset, sorted by balance descending.
pub async fn list(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<Json<Vec<Holder>>, ApiError> {
    let snap = state.snapshot().await;
    if snap.asset(id).is_none() {
        return Err(ApiError::NotFound(format!("no asset with id {id}")));
    }
    Ok(Json(snap.holders.get(&id).cloned().unwrap_or_default()))
}
