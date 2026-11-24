//! Integration tests for the API deprecation middleware.
//!
//! `api` is a binary crate, so there is no library target to import. The
//! middleware is included directly; it is intentionally self-contained (std +
//! axum + chrono + serde_json) so this works without pulling in `AppState`.

#[path = "../src/middleware/deprecation.rs"]
mod deprecation;

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    middleware,
    routing::get,
    Router,
};
use tower::ServiceExt;

use deprecation::{deprecation_middleware, DeprecationMap};

const TEST_MAP: &str = r#"
[versions]
v1 = "active"
v2 = "beta"

[[deprecated]]
path = "/version"
sunset = "2027-06-30"
successor = "/v1/meta/versions"
docs_url = "https://example.test/versions"

[[deprecated]]
path = "/v1/legacy/reports"
sunset = "2025-01-01"
successor = "/v1/reports"
docs_url = "https://example.test/reports"
"#;

fn app() -> Router {
    let map = Arc::new(DeprecationMap::from_toml_str(TEST_MAP).expect("valid test map"));
    Router::new()
        .route("/version", get(|| async { "version payload" }))
        .route("/v1/legacy/reports", get(|| async { "legacy payload" }))
        .route("/health", get(|| async { "ok" }))
        .layer(middleware::from_fn_with_state(map, deprecation_middleware))
}

async fn get_path(app: &Router, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("router responded")
}

#[tokio::test]
async fn past_sunset_returns_410_with_migration_guide() {
    let response = get_path(&app(), "/v1/legacy/reports").await;
    assert_eq!(response.status(), StatusCode::GONE);
    assert_eq!(
        response.headers()["link"],
        "<https://example.test/reports>; rel=\"migration-guide\""
    );

    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON body");
    assert_eq!(body["error"], "gone");
    assert_eq!(body["migration_guide"], "https://example.test/reports");
    assert!(body["message"].as_str().is_some_and(|m| !m.is_empty()));
}

#[tokio::test]
async fn future_sunset_passes_through_with_sunset_and_link_headers() {
    let response = get_path(&app(), "/version").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["sunset"], "2027-06-30");
    assert_eq!(
        response.headers()["link"],
        "</v1/meta/versions>; rel=\"successor-version\""
    );

    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    assert_eq!(&bytes[..], &b"version payload"[..]);
}

#[tokio::test]
async fn non_deprecated_paths_are_untouched() {
    let response = get_path(&app(), "/health").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("sunset").is_none());
    assert!(response.headers().get("link").is_none());
}
