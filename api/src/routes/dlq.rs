//! `POST /v1/admin/dlq/retry` (issue #163).

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap},
    Json,
};
use serde::Deserialize;

use super::ApiError;
use crate::indexer::{
    dlq::{RetryReport, RETRY_BATCH},
    replay::FileEventStore,
    AppState,
};

#[derive(Debug, Deserialize)]
struct RetryRequest {
    event_ids: Vec<String>,
}

/// Re-process quarantined events after a parser fix; those that now decode
/// are merged into the event store and marked resolved.
///
/// Requires `Authorization: Bearer <RWA_ADMIN_TOKEN>`. The endpoint answers
/// 401 while the token is unset or empty, and 503 without a database.
pub async fn retry(
    headers: HeaderMap,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<RetryReport>, ApiError> {
    let expected = std::env::var("RWA_ADMIN_TOKEN").unwrap_or_default();
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !token_matches(&expected, supplied) {
        return Err(ApiError::Unauthorized("admin token required".into()));
    }

    let dlq = state.dlq.as_ref().ok_or_else(|| {
        ApiError::Unavailable("dead-letter queue requires RWA_DATABASE_URL".into())
    })?;
    let event_ids = parse_retry_ids(&body)?;
    let store = FileEventStore::from_env();
    let report = match event_ids {
        Some(ids) => dlq.retry_selected(store, &ids).await,
        None => dlq.retry(store).await,
    }
    .map_err(|e| {
        tracing::error!(error = %e, "dead-letter retry failed");
        ApiError::Unavailable(e.to_string())
    })?;
    tracing::info!(
        retried = report.retried,
        resolved = report.resolved,
        still_quarantined = report.still_quarantined,
        poisoned = report.poisoned,
        "dead-letter retry complete"
    );
    Ok(Json(report))
}

fn parse_retry_ids(body: &[u8]) -> Result<Option<Vec<String>>, ApiError> {
    if body.is_empty() {
        return Ok(None);
    }
    let request: RetryRequest = serde_json::from_slice(body)
        .map_err(|e| ApiError::BadRequest(format!("invalid retry request: {e}")))?;
    if request.event_ids.is_empty() || request.event_ids.len() > RETRY_BATCH as usize {
        return Err(ApiError::BadRequest(format!(
            "event_ids must contain between 1 and {RETRY_BATCH} IDs"
        )));
    }
    if request.event_ids.iter().any(|id| id.trim().is_empty()) {
        return Err(ApiError::BadRequest(
            "event_ids must not contain empty IDs".into(),
        ));
    }
    Ok(Some(request.event_ids))
}

/// Constant-time comparison, so response timing does not reveal how much of
/// a guessed token is correct. An empty `expected` never matches.
pub(crate) fn token_matches(expected: &str, supplied: Option<&str>) -> bool {
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

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::post,
        Router,
    };
    use tower::ServiceExt as _;

    use super::{parse_retry_ids, token_matches};
    use crate::indexer::AppState;

    #[test]
    fn token_must_be_configured_and_match_exactly() {
        assert!(token_matches("s3cret", Some("s3cret")));
        assert!(!token_matches("s3cret", Some("s3creT")));
        assert!(!token_matches("s3cret", Some("s3cret!")));
        assert!(!token_matches("s3cret", None));
        assert!(!token_matches("", Some("")));
    }

    #[test]
    fn retry_body_is_optional_but_validates_manual_ids() {
        assert_eq!(parse_retry_ids(b"").unwrap(), None);
        assert_eq!(
            parse_retry_ids(br#"{"event_ids":["event-1"]}"#).unwrap(),
            Some(vec!["event-1".to_string()])
        );
        assert!(parse_retry_ids(br#"{"event_ids":[]}"#).is_err());
        assert!(parse_retry_ids(br#"{"event_ids":[" "]}"#).is_err());
        assert!(parse_retry_ids(b"not-json").is_err());
    }

    #[tokio::test]
    async fn retry_without_admin_token_is_unauthorized() {
        let app = Router::new()
            .route("/admin/dlq/retry", post(super::retry))
            .with_state(AppState::for_test_empty());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/dlq/retry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
