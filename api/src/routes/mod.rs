//! HTTP routing and the shared API error type.

pub mod assets;
pub mod audit;
pub mod compliance;
pub mod dividends;
pub mod events;
pub mod holders;
pub mod stats;
pub mod search;
pub mod security;
pub mod simulate;

#[cfg(test)]
mod test_support;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::{MatchedPath, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use crate::middleware::rate_limit::{RateLimitState, MemoryRateLimiter, RedisRateLimiter, rate_limit_middleware};
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, timeout::TimeoutLayer};

use crate::indexer::{AppState, POLL_INTERVAL};
use crate::models::ApiErrorBody;

/// Sustained requests-per-second allowed per client IP, with bursting.
const RATE_LIMIT_PER_SECOND: u64 = 5;
const RATE_LIMIT_BURST: u32 = 20;
const REQUEST_TIMEOUT_SECS: u64 = 30;
const MAX_BODY_BYTES: usize = 1_048_576;
const DEFAULT_CORS_ORIGIN: &str = "http://localhost:3000";

fn env_value<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Errors surfaced to API clients as a JSON body with an appropriate status.
#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    BadRequest(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, message) = match self {
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", msg),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "bad_request", msg),
        };
        (
            status,
            Json(ApiErrorBody {
                error: error.to_string(),
                message,
            }),
        )
            .into_response()
    }
}

/// Build the application router with CORS enabled for the docs/web app.
pub fn router(state: AppState) -> Router {
    let origins = std::env::var("RWA_CORS_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| DEFAULT_CORS_ORIGIN.into())
        .split(',')
        .map(str::trim)
        .map(|origin| origin.parse::<HeaderValue>().expect("valid CORS origin"))
        .collect::<Vec<_>>();
    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);

    let per_second = env_value("RWA_RATE_LIMIT_PER_SECOND", RATE_LIMIT_PER_SECOND);
    let burst = env_value("RWA_RATE_LIMIT_BURST", RATE_LIMIT_BURST);

    // Rate limiter allows switching between memory and Redis
    let limiter: Arc<dyn crate::middleware::rate_limit::RateLimiter> = if let Ok(redis_url) = std::env::var("RWA_RATE_LIMIT_REDIS_URL") {
        Arc::new(RedisRateLimiter::new(&redis_url).expect("failed to connect to Redis"))
    } else {
        Arc::new(MemoryRateLimiter::new(per_second, burst))
    };
    let rate_limit_state = Arc::new(RateLimitState {
        limiter,
        limit: burst as u64,
        window: (burst as u64).max(1) / per_second.max(1), // simple approximation for window if needed by Redis
    });

    // Snapshot-backed endpoints: cacheable and safe to answer with 304 when
    // the client's ETag still matches the last indexed ledger.
    //
    // All data routes are nested under `/v1` so future breaking changes can
    // be introduced as `/v2` without disturbing existing clients.
    let data_routes = Router::new()
        .route("/stats", get(stats::get))
        .route("/events", get(events::list))
        .route("/assets", get(assets::list))
        .route("/assets/:id", get(assets::detail))
        .route("/assets/:id/events", get(assets::events))
        .route("/assets/:id/holders", get(holders::list))
        .route("/assets/:id/compliance", get(compliance::summary))
        .route("/assets/:id/dividends", get(dividends::list))
        .route("/assets/:id/distributions/:did", get(dividends::get_one))
        .route("/holders/:address", get(holders::by_address))
        .route(
            "/holders/:address/compliance",
            get(holders::by_address_compliance),
        )
        .route("/compliance/:address", get(compliance::for_address))
        .route("/security/anomalies", get(security::list))
        .route("/audit/verify", get(audit::verify))
        .route(
            "/audit/entries",
            get(audit::list_entries).post(audit::create_entry),
        )
        .route("/audit/entries/:sequence", get(audit::get_entry))
        .route("/audit/anchors", get(audit::list_anchors))
        .route("/audit/anchor", axum::routing::post(audit::publish_anchor))
        .layer(middleware::from_fn_with_state(state.clone(), cache_headers));

    Router::new()
        .route("/", get(index))
        .route("/version", get(version))
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .nest("/v1", data_routes)
        .route("/v1/ws", get(crate::ws::handler))
        // `route_layer` (rather than `layer`) so the middleware runs after
        // route matching and can read `MatchedPath` from the request
        // extensions for a low-cardinality route label.
        .route_layer(middleware::from_fn(track_request_metrics))
        .with_state(state)
        .layer(TimeoutLayer::new(Duration::from_secs(env_value(
            "RWA_REQUEST_TIMEOUT_SECS",
            REQUEST_TIMEOUT_SECS,
        ))))
        .layer(RequestBodyLimitLayer::new(env_value(
            "RWA_MAX_BODY_BYTES",
            MAX_BODY_BYTES,
        )))
        .layer(middleware::from_fn_with_state(rate_limit_state, rate_limit_middleware))
        .layer(cors)
        // Outermost: scrub PII from every JSON/text response (issue #103).
        .layer(middleware::from_fn(
            crate::middleware::pii_scrubber::pii_scrub_middleware,
        ))
}

/// Issue #7: per-route Prometheus request counts and latencies.
///
/// Records `rwa_http_requests_total{method,route,status}` and
/// `rwa_http_request_duration_seconds{method,route}` for every request. The
/// route label uses the matched Axum path pattern (e.g. `/v1/assets/:id`)
/// rather than the raw URI, so it stays low-cardinality even though asset
/// IDs and addresses vary per request.
async fn track_request_metrics(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());

    let started = Instant::now();
    let response = next.run(req).await;
    let elapsed = started.elapsed();
    let status = response.status().as_u16().to_string();

    metrics::counter!(
        "rwa_http_requests_total",
        "method" => method.to_string(),
        "route" => route.clone(),
        "status" => status,
    )
    .increment(1);
    metrics::histogram!(
        "rwa_http_request_duration_seconds",
        "method" => method.to_string(),
        "route" => route,
    )
    .record(elapsed.as_secs_f64());

    response
}

/// Attach `Cache-Control` and `ETag` to snapshot-backed responses, and answer
/// `If-None-Match` with `304 Not Modified` when the snapshot hasn't advanced.
///
/// The snapshot only changes once per [`POLL_INTERVAL`], so the ETag is
/// derived from `last_indexed_ledger`: two requests against the same indexed
/// ledger are guaranteed to have identical bodies.
async fn cache_headers(State(state): State<AppState>, req: Request<Body>, next: Next) -> Response {
    let ledger = state.last_indexed_ledger();
    let etag = format!("\"ledger-{ledger}\"");

    let fresh = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == etag);

    let mut resp = if fresh {
        Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .expect("static 304 response is well-formed")
    } else {
        next.run(req).await
    };

    insert_cache_headers(resp.headers_mut(), &etag);
    resp
}

fn insert_cache_headers(headers: &mut HeaderMap, etag: &str) {
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_str(&format!("public, max-age={}", POLL_INTERVAL.as_secs()))
            .expect("max-age value is a valid header value"),
    );
    if let Ok(v) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, v);
    }
}

/// Root — a small self-describing index of the available endpoints.
async fn index() -> Json<serde_json::Value> {
    Json(json!({
        "name": "Stellar RWA API",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Read-only index of tokenized real-world asset activity on Stellar.",
        "endpoints": [
            "GET /version",
            "GET /v1/stats",
            "GET /v1/events",
            "GET /v1/assets",
            "GET /v1/assets/:id",
            "GET /v1/assets/:id/events",
            "GET /v1/assets/:id/holders",
            "GET /v1/assets/:id/compliance",
            "GET /v1/assets/:id/dividends",
            "GET /v1/assets/:id/distributions/:did",
            "GET /v1/holders/:address",
            "GET /v1/holders/:address/compliance",
            "GET /v1/compliance/:address",
            "GET /v1/security/anomalies",
            "GET /health",
            "GET /metrics"
        ],
        "docs": "https://github.com/A4-Stellar/Tessera"
    }))
}

/// Machine-readable version endpoint.
///
/// Returns the crate version from `Cargo.toml` and the API release label so
/// clients can negotiate compatibility without parsing the root index body.
async fn version() -> Json<serde_json::Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "release": concat!("v", env!("CARGO_PKG_VERSION")),
    }))
}

/// Liveness probe.
async fn health(State(state): State<AppState>) -> Response {
    let updated = state
        .snapshot()
        .stats
        .last_updated
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok());
    let age = updated.map(|time| {
        (chrono::Utc::now() - time.with_timezone(&chrono::Utc))
            .num_seconds()
            .max(0)
    });
    let max_age = (POLL_INTERVAL * 3).as_secs() as i64;
    let healthy = age.is_some_and(|seconds| seconds <= max_age);
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({
            "status": if healthy { "ok" } else { "degraded" },
            "snapshot_age_seconds": age,
            "max_age_seconds": max_age,
        })),
    )
        .into_response()
}

/// Prometheus scrape endpoint: indexer refresh latency, failure counts, last
/// success timestamp, and per-asset read errors.
async fn metrics(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let expected = std::env::var("RWA_METRICS_TOKEN").ok();
    let authorized = expected
        .as_deref()
        .filter(|token| !token.is_empty())
        .zip(supplied)
        .is_some_and(|(expected, supplied)| expected == supplied);
    if !authorized {
        return (StatusCode::UNAUTHORIZED, "metrics authentication required").into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::{
        body::Body,
        extract::ConnectInfo,
        http::{header, Request, StatusCode},
        Router,
    };
    use tower::ServiceExt as _;

    use crate::indexer::AppState;

    use super::router;

    async fn assert_json_content_type(app: Router, uri: &str, status: StatusCode) {
        // This is the only test that exercises the full `router()`, rate
        // limiter included. The governor keys on the peer IP, which `serve`
        // supplies via into_make_service_with_connect_info; without it here
        // the extractor fails and every route answers 500.
        let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 54321))));

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), status, "unexpected status for {uri}");
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("JSON responses should set a content type");
        assert!(
            content_type
                .to_str()
                .expect("content type is UTF-8")
                .starts_with("application/json"),
            "expected application/json for {uri}, got {content_type:?}"
        );
    }

    #[tokio::test]
    async fn json_responses_set_application_json_content_type() {
        let app = router(AppState::for_test_empty());

        assert_json_content_type(app.clone(), "/", StatusCode::OK).await;
        assert_json_content_type(app.clone(), "/health", StatusCode::SERVICE_UNAVAILABLE).await;
        assert_json_content_type(app.clone(), "/version", StatusCode::OK).await;
        assert_json_content_type(app.clone(), "/v1/stats", StatusCode::OK).await;
        assert_json_content_type(app.clone(), "/v1/assets", StatusCode::OK).await;
        assert_json_content_type(app, "/v1/assets/99999", StatusCode::NOT_FOUND).await;
    }
}
