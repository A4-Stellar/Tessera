//! `POST /v1/admin/dlq/retry` (issue #163).

use axum::{
    extract::State,
    http::{header, HeaderMap},
    Json,
};

use super::ApiError;
use crate::indexer::{dlq::RetryReport, replay::FileEventStore, AppState};

/// Re-process quarantined events after a parser fix; those that now decode
/// are merged into the event store and marked resolved.
///
/// Requires `Authorization: Bearer <RWA_ADMIN_TOKEN>`. The endpoint answers
/// 401 while the token is unset or empty, and 503 without a database.
pub async fn retry(
    headers: HeaderMap,
    State(state): State<AppState>,
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
    let report = dlq.retry(FileEventStore::from_env()).await.map_err(|e| {
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

/// Constant-time comparison, so response timing does not reveal how much of
/// a guessed token is correct. An empty `expected` never matches.
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

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::post,
        Router,
    };
    use tower::ServiceExt as _;

    use super::token_matches;
    use crate::indexer::AppState;

    #[test]
    fn token_must_be_configured_and_match_exactly() {
        assert!(token_matches("s3cret", Some("s3cret")));
        assert!(!token_matches("s3cret", Some("s3creT")));
        assert!(!token_matches("s3cret", Some("s3cret!")));
        assert!(!token_matches("s3cret", None));
        assert!(!token_matches("", Some("")));
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
