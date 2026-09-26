use axum::{response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct SimulateRequest {
    pub tx: String,
}

#[derive(Serialize)]
pub struct SimulateResponse {
    pub fee: u64,
}

pub async fn simulate_handler(Json(req): Json<SimulateRequest>) -> impl IntoResponse {
    Json(SimulateResponse { fee: 100 })
}
