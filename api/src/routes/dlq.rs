//! Admin routes for the event-parsing dead-letter queue (issue #163).
//!
//! - `GET  /v1/admin/dlq`        — list quarantined events (triage view).
//! - `POST /v1/admin/dlq/retry`  — re-decode quarantined events with the
//!   current parser; those that now decode are merged into the event store
//!   and marked resolved.
//!
//! Both routes require `Authorization: Bearer <RWA_ADMIN_TOKEN>`: they
//! answer 401 while the token is unset or empty, and 503 without an active
//! quarantine backend.

use axum::{
    extract::State,
    http::{header, HeaderMap},
    Json,
};
use serde::Serialize;

use super::ApiError;
use crate::indexer::{dlq::RetryReport, replay::FileEventStore, AppState};

/// Response of `GET /v1/admin/dlq`.
#[derive(Debug, Serialize)]
pub struct DlqListResponse {
    /// Active backend: `postgres` or `file` (no database configured).
    pub backend: &'static str,
    pub total: usize,
    pub entries: Vec<crate::indexer::dlq::QuarantinedEvent>,
}

/// Bearer-token check shared by the DLQ admin endpoints. `RWA_ADMIN_TOKEN`
/// must be set and match exactly, compared in constant time so response
/// timing does not reveal how much of a guessed token is correct. An unset
/// or empty token never matches.
fn require_admin(headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = std::env::var("RWA_ADMIN_TOKEN").unwrap_or_default();
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !token_matches(&expected, supplied) {
        return Err(ApiError::Unauthorized("admin token required".into()));
    }
    Ok(())
}

fn token_matches(expected: &str, supplied: Option<&str>) -> bool {
    let Some(supplied) = supplied else {
        return false;
    };
    !expected.is_empty()
        && expected.len() == supplied.len()
        && expected
            .bytes()
            .zip(supplied.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// `GET /v1/admin/dlq` — list quarantined events for triage, whichever
/// backend is active.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DlqListResponse>, ApiError> {
    require_admin(&headers)?;
    let dlq = state.dlq.as_ref().ok_or_else(|| {
        ApiError::Unavailable("dead-letter queue is not active".into())
    })?;
    let entries = dlq.list().await.map_err(|e| {
        tracing::error!(error = %e, "dead-letter list failed");
        ApiError::Unavailable(e.to_string())
    })?;
    let total = entries.len();
    Ok(Json(DlqListResponse {
        backend: dlq.backend_name(),
        total,
        entries,
    }))
}

/// `POST /v1/admin/dlq/retry` — re-process quarantined events after a parser
/// fix; those that now decode are merged into the event store and marked
/// resolved.
pub async fn retry(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<RetryReport>, ApiError> {
    require_admin(&headers)?;
    let dlq = state.dlq.as_ref().ok_or_else(|| {
        ApiError::Unavailable("dead-letter queue is not active".into())
    })?;
    let report = dlq
        .retry(FileEventStore::from_env())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "dead-letter retry failed");
            ApiError::Unavailable(e.to_string())
        })?;
    tracing::info!(
        retried = report.retried,
        resolved = report.resolved,
        still_quarantined = report.still_quarantined,
        "dead-letter retry complete"
    );
    Ok(Json(report))
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt as _;

    use super::token_matches;
    use crate::indexer::AppState;

    /// Serialize tests that mutate the process-global admin-token env var.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn token_must_be_configured_and_match_exactly() {
        assert!(token_matches("s3cret", Some("s3cret")));
        assert!(!token_matches("s3cret", Some("s3creT")));
        assert!(!token_matches("s3cret", Some("s3cret!")));
        assert!(!token_matches("s3cret", None));
        assert!(!token_matches("", Some("")));
    }

    async fn response_for(uri: &str, auth: Option<&str>) -> StatusCode {
        let app = Router::new()
            .route("/admin/dlq", get(super::list))
            .route("/admin/dlq/retry", post(super::retry))
            .with_state(AppState::for_test_empty());
        let mut builder = Request::builder().uri(uri);
        if let Some(auth) = auth {
            builder = builder.header("authorization", auth);
        }
        app.oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn dlq_routes_are_unauthorized_without_a_valid_token() {
        // SAFETY: test-only env mutation, serialized by ENV_LOCK.
        let _env = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RWA_ADMIN_TOKEN");

        // Unset token: 401 on both routes (never silently open).
        assert_eq!(
            response_for("/admin/dlq", None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            response_for("/admin/dlq/retry", None).await,
            StatusCode::UNAUTHORIZED
        );
        // Wrong token: still 401.
        assert_eq!(
            response_for("/admin/dlq", Some("Bearer nope")).await,
            StatusCode::UNAUTHORIZED
        );

        std::env::set_var("RWA_ADMIN_TOKEN", "s3cret");
        // AppState::for_test_empty() leaves dlq = None, so a correct token
        // gets past auth and answers 503, not 200.
        assert_eq!(
            response_for("/admin/dlq", Some("Bearer s3cret")).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            response_for("/admin/dlq/retry", Some("Bearer s3cret")).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        std::env::remove_var("RWA_ADMIN_TOKEN");
    }
}
