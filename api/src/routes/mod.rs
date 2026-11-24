//! HTTP routing and the shared API error type.

pub mod assets;
pub mod audit;
pub mod bridge;
pub mod cap_table_import;
pub mod compliance;
pub mod da;
pub mod dividends;
pub mod dlq;
pub mod events;
pub mod export;
pub mod forensics;
pub mod holders;
pub mod meta;
pub mod portfolio;
pub mod privacy;
pub mod proofs;
/// Regulatory sandbox lifecycle (issue #468).
pub mod sandbox;
#[expect(
    dead_code,
    reason = "The search placeholder is not part of the active API router."
)]
pub mod search;
pub mod security;
pub mod sep24;
pub mod sep30;
#[expect(
    dead_code,
    reason = "The simulation placeholder is not part of the active API router."
)]
pub mod simulate;
pub mod slo;
pub mod stats;
pub mod time_travel;

#[cfg(test)]
mod test_support;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::middleware::token_bucket::{
    token_bucket_middleware, BucketStore, MemoryBucketStore, RedisBucketStore, Tier,
    TokenBucketState,
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
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, timeout::TimeoutLayer};

use crate::cluster::leader_election::NodeRole;
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
    Unauthorized(String),
    Forbidden(String),
    Conflict(String),
    Gone(String),
    Unavailable(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, message) = match self {
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", msg),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "bad_request", msg),
            ApiError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "unauthorized", msg),
            ApiError::Forbidden(msg) => (StatusCode::FORBIDDEN, "forbidden", msg),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, "conflict", msg),
            ApiError::Gone(msg) => (StatusCode::GONE, "gone", msg),
            ApiError::Unavailable(msg) => {
                (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", msg)
            }
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
        .allow_headers([
            header::CONTENT_TYPE,
            axum::http::HeaderName::from_static("x-tenant-id"),
            axum::http::HeaderName::from_static("x-sandbox-tenant-id"),
            axum::http::HeaderName::from_static("x-api-key"),
        ]);

    let per_second = env_value("RWA_RATE_LIMIT_PER_SECOND", RATE_LIMIT_PER_SECOND);
    let burst = env_value("RWA_RATE_LIMIT_BURST", RATE_LIMIT_BURST);

    // Token bucket: `burst` is the capacity, `per_second` the refill rate.
    // Specific API keys can get their own tier via `RWA_API_KEY_TIERS`
    // (`key=capacity:refill_per_second,...`). Redis-backed when configured.
    let store: Arc<dyn BucketStore> =
        if let Ok(redis_url) = std::env::var("RWA_RATE_LIMIT_REDIS_URL") {
            Arc::new(RedisBucketStore::new(&redis_url).expect("failed to connect to Redis"))
        } else {
            Arc::new(MemoryBucketStore::default())
        };
    let sandbox_mode = env_value("RWA_SANDBOX_MODE", false);
    let token_bucket_state = Arc::new(
        TokenBucketState::new(
            store,
            Tier {
                capacity: burst,
                refill_per_second: per_second as f64,
            },
            TokenBucketState::parse_api_key_tiers(
                &std::env::var("RWA_API_KEY_TIERS").unwrap_or_default(),
            ),
        )
        .with_sandbox_mode(sandbox_mode),
    );

    // Snapshot-backed endpoints: cacheable and safe to answer with 304 when
    // the client's ETag still matches the last indexed ledger.
    //
    // All data routes are nested under `/v1` so future breaking changes can
    // be introduced as `/v2` without disturbing existing clients.
    let data_routes = Router::new()
        .route("/stats", get(stats::get))
        .route("/events", get(events::list))
        .route("/rsocket", get(crate::rsocket::handler))
        .route("/assets", get(assets::list))
        .route("/assets/:id", get(assets::detail))
        .route("/assets/:id/da-proof", get(da::da_proof))
        .route("/assets/:id/events", get(assets::events))
        .route("/assets/:id/metrics/analytics", get(assets::analytics))
        .route(
            "/assets/:id/analytics",
            get(assets::concentration_analytics),
        )
        .route("/assets/:id/holders", get(holders::list_or_aggregate))
        .route("/assets/:id/compliance", get(compliance::summary))
        // Issue #478: board-approved transfers awaiting co-signers.
        .route(
            "/assets/:id/pending-transfers",
            get(pending_transfers::list),
        )
        .route("/assets/:id/dividends", get(dividends::list))
        .route("/assets/:id/distributions/:did", get(dividends::get_one))
        .route("/assets/:id/prospectus", get(prospectus::get_prospectus))
        .route("/assets/:id/prospectus/history", get(prospectus::get_prospectus_history))
        .route("/assets/:id/export", get(export::export))
        .route("/holders/:address", get(holders::by_address))
        .route(
            "/holders/:address/compliance",
            get(holders::by_address_compliance),
        )
        .route("/holders/:address/portfolio", get(portfolio::get_portfolio))
        .route("/compliance/:address", get(compliance::for_address))
        .route("/security/anomalies", get(security::list))
        // Issue #475: bridge packet monitoring.
        .route("/bridge/packets", get(bridge::packets))
        .route("/audit/verify", get(audit::verify))
        .route(
            "/audit/entries",
            get(audit::list_entries).post(audit::create_entry),
        )
        .route("/audit/entries/:sequence", get(audit::get_entry))
        .route("/audit/mmr", get(audit::mmr_root))
        .route("/audit/:entry_id/proof", get(audit::mmr_proof))
        .route("/audit/anchors", get(audit::list_anchors))
        // Issue #318: light-client MMR checkpoint proofs.
        .route("/proofs/mmr", get(proofs::root))
        .route("/proofs/mmr/:ledger_seq", get(proofs::proof))
        .route("/audit/anchor", axum::routing::post(audit::publish_anchor))
        .layer(middleware::from_fn_with_state(state.clone(), cache_headers));

    Router::new()
        .route("/", get(index))
        .route("/version", get(version))
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        // SEP-24 (Hosted Deposit and Withdrawal) lives at the API root so an
        // anchor's `stellar.toml` can advertise standard paths. These routes
        // use the module-local volatile store and require no `AppState`.
        // NOTE: the global anonymizer middleware pseudonymises KYC fields
        // (email, first_name, last_name, dob, tax_id, …) for non-admin callers;
        // SEP-24 status/amount/XDR fields are unaffected.
        .route("/.well-known/stellar.toml", get(sep24::stellar_toml))
        .route(
            "/sep24/transactions/deposit/interactive",
            axum::routing::post(sep24::interactive_deposit),
        )
        .route("/sep24/transaction", get(sep24::get_transaction))
        .route("/sep24/transactions", get(sep24::list_transactions))
        .route(
            "/sep24/transactions/:id/confirm_external",
            axum::routing::post(sep24::confirm_external),
        )
        .route(
            "/sep24/transactions/:id/complete",
            axum::routing::post(sep24::complete),
        )
        .route(
            "/sep24/transactions/:id/kyc",
            axum::routing::post(sep24::submit_kyc),
        )
        .route(
            "/sep24/admin/reverify",
            axum::routing::post(sep24::reverify),
        )
        .route(
            "/sep24/admin/reverify/due",
            get(sep24::reverify_due),
        )
        .route(
            "/v1/assets/:id/proofs/verify",
            axum::routing::post(proofs::verify),
        )
        .nest("/v1", data_routes)
        // Issue #478: relay a board member's signed `co_sign` transaction.
        .route(
            "/v1/assets/:id/pending-transfers/:transfer_id/sign",
            axum::routing::post(pending_transfers::sign),
        )
        .route("/v1/ws", get(crate::ws::handler))
        // Versioned aliases for the protocol-standard root endpoints. The
        // root paths stay available for anchors/well-known clients, but the
        // deprecation map marks them with a successor and sunset date.
        .route("/v1/health", get(health))
        .route("/v1/metrics", get(metrics))
        .route("/v1/version", get(version))
        .route(
            "/v1/sep24/transactions/deposit/interactive",
            axum::routing::post(sep24::interactive_deposit),
        )
        .route("/v1/sep24/transaction", get(sep24::get_transaction))
        .route("/v1/sep24/transactions", get(sep24::list_transactions))
        .route(
            "/v1/sep24/transactions/:id/confirm_external",
            axum::routing::post(sep24::confirm_external),
        )
        .route(
            "/v1/sep24/transactions/:id/complete",
            axum::routing::post(sep24::complete),
        )
        .route(
            "/v1/sep24/transactions/:id/kyc",
            axum::routing::post(sep24::submit_kyc),
        )
        .route("/v1/admin/dlq/retry", axum::routing::post(dlq::retry))
        // Issue #475: other-chain watchers report packet sends/receipts.
        .route(
            "/v1/admin/bridge/observations",
            axum::routing::post(bridge::observe),
        )
        // Issue #476: EDGAR Form 4 import and signed cap-table reports.
        .route(
            "/v1/admin/cap-table/import-edgar",
            axum::routing::post(cap_table_import::import_edgar),
        )
        .route(
            "/v1/admin/cap-table/reports/sign",
            axum::routing::post(cap_table_import::sign_report),
        )
        // Issue #26: per-key differential-privacy budget consumption.
        .route("/v1/admin/privacy/budget", get(privacy::budget))
        // Issue #471: SLO alerting with multi-window burn rate and error budget report.
        .route(
            "/v1/admin/slo/report",
            axum::routing::post(slo::slo_report).get(slo::slo_report),
        )
        // Shadow-indexer health: independent-node lag and last verification.
        .route("/v1/admin/health/shadow", get(shadow_health))
        // Machine-readable API version/deprecation catalogue.
        .route("/v1/meta/versions", get(meta::versions))
        // `route_layer` (rather than `layer`) so the middleware runs after
        // route matching and can read `MatchedPath` from the request
        // extensions for a low-cardinality route label.
        .route_layer(middleware::from_fn(track_request_metrics))
        .with_state(state)
        // GraphQL lives at the API root so Apollo Router can use the standard
        // `/graphql` service endpoint (including Federation `_service`). It is
        // already stateful, so it merges after `with_state`.
        .merge(
            crate::graphql::create_graphql_router()
                .route_layer(middleware::from_fn(track_request_metrics)),
        )
        // Issue #50: Stellar SEP-30 Account Recovery Server with Social Recovery Multisig
        .merge({
            let sep30_state = Arc::new(sep30::Sep30ServerState::new(Arc::new(std::sync::RwLock::new(
                crate::audit::chain_verifier::AuditTrailVerifier::new(),
            ))));
            Router::new()
                .route(
                    "/sep30/accounts/:address",
                    axum::routing::post(sep30::register_account).get(sep30::get_account),
                )
                .route(
                    "/sep30/accounts/:address/recover",
                    axum::routing::put(sep30::recover_account),
                )
                .route(
                    "/sep30/accounts/:address/sign",
                    axum::routing::post(sep30::recover_account),
                )
                .route(
                    "/sep30/accounts/:address/sign/:signing_address",
                    axum::routing::post(sep30::recover_account),
                )
                .route(
                    "/v1/sep30/accounts/:address",
                    axum::routing::post(sep30::register_account).get(sep30::get_account),
                )
                .route(
                    "/v1/sep30/accounts/:address/recover",
                    axum::routing::put(sep30::recover_account),
                )
                .route(
                    "/v1/sep30/accounts/:address/sign",
                    axum::routing::post(sep30::recover_account),
                )
                .route(
                    "/v1/sep30/accounts/:address/sign/:signing_address",
                    axum::routing::post(sep30::recover_account),
                )
                .with_state(sep30_state)
        })
        .layer(TimeoutLayer::new(Duration::from_secs(env_value(
            "RWA_REQUEST_TIMEOUT_SECS",
            REQUEST_TIMEOUT_SECS,
        ))))
        .layer(RequestBodyLimitLayer::new(env_value(
            "RWA_MAX_BODY_BYTES",
            MAX_BODY_BYTES,
        )))
        .layer(middleware::from_fn_with_state(
            token_bucket_state,
            token_bucket_middleware,
        ))
        // API versioning: annotate deprecated paths with Sunset/Link and
        // return 410 Gone once the sunset date has passed. The map is shared
        // with GET /v1/meta/versions so both surfaces agree.
        .layer(middleware::from_fn_with_state(
            Arc::new(meta::versions_map().clone()),
            crate::middleware::deprecation::deprecation_middleware,
        ))
        .layer(cors)
        // Drop named personal-data fields for non-admin callers (issue #284).
        .layer(middleware::from_fn(
            crate::middleware::anonymizer::anonymizer_middleware,
        ))
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
            "POST /v1/assets/:id/proofs/verify",
            "GET /v1/assets/:id/da-proof",
            "GET /v1/assets/:id/events",
            "GET /v1/assets/:id/metrics/analytics",
            "GET /v1/assets/:id/analytics",
            "GET /v1/assets/:id/holders",
            "GET /v1/assets/:id/holders?view=aggregate",
            "GET /v1/assets/:id/compliance",
            "GET /v1/assets/:id/pending-transfers",
            "POST /v1/assets/:id/pending-transfers/:transfer_id/sign",
            "GET /v1/assets/:id/dividends",
            "GET /v1/assets/:id/distributions/:did",
            "GET /v1/assets/:id/export",
            "GET /v1/holders/:address",
            "GET /v1/holders/:address/compliance",
            "GET /v1/holders/:address/portfolio",
            "GET /v1/compliance/:address",
            "GET /v1/security/anomalies",
            "GET /v1/bridge/packets?status=pending|completed|timeout",
            "POST /v1/admin/bridge/observations",
            "POST /v1/admin/cap-table/import-edgar",
            "POST /v1/admin/cap-table/reports/sign",
            "GET /v1/audit/:entry_id/proof",
            "GET /v1/audit/mmr",
            "GET /v1/proofs/mmr",
            "GET /v1/proofs/mmr/:ledger_seq?account=:address",
            "GET /v1/admin/privacy/budget",
            "POST /v1/admin/slo/report",
            "GET /v1/admin/slo/report",
            "POST /sandbox/reset",
            "GET /sandbox/status",
            "GET /.well-known/stellar.toml",
            "POST /sep24/transactions/deposit/interactive",
            "GET /sep24/transaction",
            "GET /sep24/transactions",
            "POST /sep24/transactions/:id/kyc",
            "POST /sep24/admin/reverify",
            "GET /sep24/admin/reverify/due",
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

/// Shadow-indexer health: reports the lag of the independent snapshot and
/// the time of the last primary-vs-shadow verification (issue: silent XDR
/// parsing bugs / missed events).
async fn shadow_health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let status = state.shadow.status();
    Json(serde_json::to_value(status).unwrap_or_else(|error| {
        json!({ "status": "error", "message": error.to_string() })
    }))
}

/// Liveness probe, reporting this node's leader-election role (issue #159).
async fn health(State(state): State<AppState>) -> Response {
    let role = *state.node_role.borrow();
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
    // Standby nodes do not poll, so their snapshot is expected to be stale:
    // they are healthy while waiting to take over.
    let healthy = role == NodeRole::Follower || age.is_some_and(|seconds| seconds <= max_age);
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({
            "status": if healthy { "ok" } else { "degraded" },
            "role": role,
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

    #[tokio::test]
    async fn health_reports_node_role() {
        use crate::cluster::leader_election::NodeRole;

        async fn health(state: AppState) -> (StatusCode, serde_json::Value) {
            let response = axum::Router::new()
                .route("/health", axum::routing::get(super::health))
                .with_state(state)
                .oneshot(
                    Request::builder()
                        .uri("/health")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }

        // A leader with no indexed snapshot yet is degraded.
        let (status, body) = health(AppState::for_test_empty()).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["role"], "leader");

        // A standby does not poll, so its empty snapshot is expected.
        let mut state = AppState::for_test_empty();
        state.node_role = tokio::sync::watch::channel(NodeRole::Follower).1;
        let (status, body) = health(state).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["role"], "follower");
        assert_eq!(body["status"], "ok");
    }
}
