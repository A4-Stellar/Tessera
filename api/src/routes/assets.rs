//! `GET /assets` and `GET /assets/:id`.

use axum::{extract::{Path, State}, Json};

use crate::indexer::AppState;
use crate::models::Asset;
use super::ApiError;

/// All tokenized assets with valuation, supply and holder counts.
pub async fn list(State(state): State<AppState>) -> Json<Vec<Asset>> {
    let snap = state.snapshot().await;
    Json(snap.assets)
}

/// Full detail for a single asset by its registry id.
pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<Json<Asset>, ApiError> {
    let snap = state.snapshot().await;
    snap.asset(id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("no asset with id {id}")))
}
