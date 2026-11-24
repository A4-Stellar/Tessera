//! Bridge packet monitoring (issue #475).
//!
//! - `GET  /v1/bridge/packets?status=pending|completed|timeout&limit=N`
//! - `POST /v1/admin/bridge/observations`: a watcher of the other chain
//!   reports a `send`/`recv` it saw. Requires `Authorization: Bearer
//!   $RWA_ADMIN_TOKEN`.

use std::collections::BTreeMap;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::ApiError;
use crate::indexer::AppState;
use crate::services::bridge_monitor::{BridgeAlert, Packet, PacketStatus, RemotePacketEvent};

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

#[derive(Debug, Default, Deserialize)]
pub struct PacketsQuery {
    pub status: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct PacketsResponse {
    pub ledger: u32,
    pub expected_relay_ledgers: u32,
    pub counts: BTreeMap<PacketStatus, usize>,
    /// Highest anomaly score (basis points) seen per destination chain.
    pub anomaly_scores: BTreeMap<String, u32>,
    pub packets: Vec<Packet>,
}

/// `GET /v1/bridge/packets`
pub async fn packets(
    State(state): State<AppState>,
    Query(query): Query<PacketsQuery>,
) -> Result<Json<PacketsResponse>, ApiError> {
    let status = query
        .status
        .as_deref()
        .map(str::parse::<PacketStatus>)
        .transpose()
        .map_err(ApiError::BadRequest)?;
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let monitor = &state.bridge;
    Ok(Json(PacketsResponse {
        ledger: state.last_indexed_ledger(),
        expected_relay_ledgers: monitor.expected_relay_ledgers(),
        counts: monitor.counts(),
        anomaly_scores: monitor.anomaly_scores(),
        packets: monitor.packets(status, limit),
    }))
}

/// `Authorization: Bearer <RWA_ADMIN_TOKEN>`, compared by digest.
pub(crate) fn require_admin(headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = std::env::var("RWA_ADMIN_TOKEN").unwrap_or_default();
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if expected.is_empty()
        || Sha256::digest(expected.as_bytes()) != Sha256::digest(supplied.as_bytes())
    {
        return Err(ApiError::Unauthorized("admin token required".into()));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct ObservationResponse {
    pub alerts: Vec<BridgeAlert>,
}

/// `POST /v1/admin/bridge/observations`
pub async fn observe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(event): Json<RemotePacketEvent>,
) -> Result<Json<ObservationResponse>, ApiError> {
    require_admin(&headers)?;
    if event.key.trim().is_empty() || event.chain.trim().is_empty() {
        return Err(ApiError::BadRequest("chain and key are required".into()));
    }
    let alerts = state
        .bridge
        .observe_remote(event, state.last_indexed_ledger());
    Ok(Json(ObservationResponse { alerts }))
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request, http::StatusCode, routing::get, Router};
    use serde_json::json;
    use tower::ServiceExt as _;

    use super::*;
    use crate::indexer::Snapshot;
    use crate::models::Event;
    use crate::routes::test_support::state_with;

    fn send_event(seq: u64, ledger: u32) -> Event {
        Event {
            id: seq,
            contract: "CBRIDGE".into(),
            event_type: "SendPacket".into(),
            ledger,
            timestamp: None,
            data: json!({ "topics": ["SendPacket", "GA"], "value": [seq, 0, "100", "657468", "0x", ledger] }),
        }
    }

    async fn get_json(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    #[tokio::test]
    async fn filters_packets_by_status() {
        let state = state_with(Snapshot::default());
        state
            .bridge
            .observe_events(&[send_event(1, 100), send_event(2, 400)], 400);
        let app = Router::new()
            .route("/v1/bridge/packets", get(packets))
            .with_state(state);

        let (status, body) = get_json(app.clone(), "/v1/bridge/packets?status=timeout").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["packets"].as_array().unwrap().len(), 1);
        assert_eq!(body["packets"][0]["key"], "stellar:1");
        assert_eq!(body["counts"]["pending"], 1);
        assert_eq!(body["counts"]["timeout"], 1);

        let (_, body) = get_json(app.clone(), "/v1/bridge/packets?status=pending").await;
        assert_eq!(body["packets"][0]["key"], "stellar:2");

        let (status, _) = get_json(app, "/v1/bridge/packets?status=lost").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn observations_need_the_admin_token() {
        let state = state_with(Snapshot::default());
        let app = Router::new()
            .route(
                "/v1/admin/bridge/observations",
                axum::routing::post(observe),
            )
            .with_state(state);
        let body = json!({ "kind": "recv", "chain": "ethereum", "key": "stellar:1" }).to_string();
        let req = Request::post("/v1/admin/bridge/observations")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        assert_eq!(
            app.oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
