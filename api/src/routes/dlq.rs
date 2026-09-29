//! Admin routes for the event-parsing dead-letter queue (issue #163).
//!
//! - `GET  /v1/admin/dlq`        — list quarantined payloads (triage view).
//! - `POST /v1/admin/dlq/retry`  — re-decode quarantined payloads with the
//!   current parser and merge recovered events into the event store; pass
//!   `{"id": <event_id>}` in the body to retry a single entry.
//!
//! Both routes are gated behind `RWA_DLQ_ADMIN_TOKEN` (Bearer). When unset
//! the endpoints answer `503` — they can mutate the event store, so they
//! must never be silently open.

use axum::{extract::State, http::HeaderMap, Json};
use serde::{Deserialize, Serialize};

use crate::indexer::dlq::{self, DlqStore};
use crate::indexer::AppState;

use super::ApiError;

/// Bearer token guarding the DLQ admin endpoints. Unset or empty disables
/// the endpoints entirely (they answer `503`).
const ADMIN_TOKEN_ENV: &str = "RWA_DLQ_ADMIN_TOKEN";

#[derive(Debug, Serialize)]
pub struct DlqListResponse {
    pub total: usize,
    pub entries: Vec<dlq::QuarantinedEvent>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RetryRequest {
    /// Optional single entry to retry; omitted retries the whole queue.
    pub id: Option<u64>,
}

/// Check the `Authorization: Bearer <token>` header against
/// `RWA_DLQ_ADMIN_TOKEN`. Returns `Err` with the response to send when the
/// endpoint must not be served.
fn require_admin(headers: &HeaderMap) -> Result<(), ApiError> {
    let Ok(expected) = std::env::var(ADMIN_TOKEN_ENV) else {
        return Err(ApiError::ServiceUnavailable(format!(
            "DLQ admin endpoints are disabled; set {ADMIN_TOKEN_ENV}"
        )));
    };
    if expected.is_empty() {
        return Err(ApiError::ServiceUnavailable(format!(
            "DLQ admin endpoints are disabled; set {ADMIN_TOKEN_ENV}"
        )));
    }
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match supplied {
        Some(supplied) if constant_time_eq(supplied, &expected) => Ok(()),
        _ => Err(ApiError::Unauthorized(
            "DLQ admin endpoints require a valid bearer token".into(),
        )),
    }
}

/// Length-independent comparison so token checks do not leak length timing.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `GET /v1/admin/dlq` — list the quarantine queue for triage.
pub async fn list(headers: HeaderMap) -> Result<Json<DlqListResponse>, ApiError> {
    require_admin(&headers)?;
    let store = DlqStore::from_env();
    let entries = store
        .load()
        .map_err(|e| ApiError::ServiceUnavailable(e.to_string()))?;
    let total = entries.len();
    Ok(Json(DlqListResponse { total, entries }))
}

/// `POST /v1/admin/dlq/retry` — re-decode quarantined payloads with the
/// current parser and merge recovered events into the event store.
pub async fn retry(
    State(_state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<RetryRequest>>,
) -> Result<Json<dlq::RetryReport>, ApiError> {
    require_admin(&headers)?;
    let id = body.and_then(|Json(body)| body.id);
    let store = DlqStore::from_env();
    let report = dlq::retry(&store, id)
        .await
        .map_err(|e| ApiError::ServiceUnavailable(e.to_string()))?;
    Ok(Json(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize tests that mutate the process-global admin-token env var.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn constant_time_eq_matches_and_mismatch() {
        assert!(constant_time_eq("token", "token"));
        assert!(!constant_time_eq("token", "tokem"));
        assert!(!constant_time_eq("token", "token2"));
        assert!(!constant_time_eq("", "x"));
    }

    #[test]
    fn endpoints_are_disabled_without_token() {
        let _env = ENV_LOCK.lock().unwrap();
        std::env::remove_var(ADMIN_TOKEN_ENV);
        let headers = HeaderMap::new();
        assert!(matches!(
            require_admin(&headers),
            Err(ApiError::ServiceUnavailable(_))
        ));
    }

    #[test]
    fn endpoints_reject_wrong_token() {
        // SAFETY: test-only env mutation, serialized by ENV_LOCK.
        let _env = ENV_LOCK.lock().unwrap();
        std::env::set_var(ADMIN_TOKEN_ENV, "secret");
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer wrong".parse().unwrap(),
        );
        assert!(matches!(require_admin(&headers), Err(ApiError::Unauthorized(_))));

        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer secret".parse().unwrap(),
        );
        assert!(require_admin(&headers).is_ok());

        // Acceptance without the "Bearer " prefix is rejected too.
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "secret".parse().unwrap(),
        );
        assert!(matches!(require_admin(&headers), Err(ApiError::Unauthorized(_))));

        std::env::remove_var(ADMIN_TOKEN_ENV);
    }
}
