//! Differential-privacy plumbing for HTTP handlers (issue #26):
//!
//! * the `429 Too Many Requests` answer for an exhausted privacy budget,
//! * the headers describing a charged query, and
//! * `GET /v1/admin/privacy/budget`, the operator view of consumption.

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use super::dlq::token_matches;
use super::ApiError;
use crate::analytics::differential_privacy::{
    key_id_for, BudgetUsage, PrivacyRejection, PrivateQuery, ANONYMOUS,
};
use crate::indexer::AppState;
use crate::models::ApiErrorBody;

/// The caller's `X-API-Key`, if any.
pub(crate) fn api_key(headers: &HeaderMap) -> Option<&str> {
    headers.get("X-API-Key").and_then(|v| v.to_str().ok())
}

impl IntoResponse for PrivacyRejection {
    fn into_response(self) -> Response {
        match self {
            PrivacyRejection::BudgetExhausted {
                spent_epsilon,
                limit_epsilon,
                retry_after_secs,
                ..
            } => {
                let mut response = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(ApiErrorBody {
                        error: "privacy_budget_exhausted".into(),
                        message: format!(
                            "privacy budget exhausted ({spent_epsilon:.3} of {limit_epsilon} epsilon spent); \
                             it refreshes in {retry_after_secs} s"
                        ),
                    }),
                )
                    .into_response();
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from(retry_after_secs));
                response
            }
            PrivacyRejection::StoreUnavailable(error) => {
                tracing::error!(error = %error, "privacy budget store unavailable; refusing private query");
                ApiError::Unavailable("privacy budget store unavailable".into()).into_response()
            }
        }
    }
}

/// Headers describing the privacy cost of a charged query.
pub(crate) fn privacy_headers(query: &PrivateQuery) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let mut put = |name: &'static str, value: String| {
        if let Ok(value) = HeaderValue::from_str(&value) {
            headers.insert(name, value);
        }
    };
    put("X-Privacy-Epsilon", query.params.epsilon.to_string());
    put("X-Privacy-Delta", query.params.delta_or_zero().to_string());
    put(
        "X-Privacy-Budget-Remaining",
        format!("{:.6}", query.remaining_epsilon()),
    );
    put(
        "X-Privacy-Budget-Reset",
        query.refresh_in_secs.to_string(),
    );
    headers
}

#[derive(Debug, Deserialize)]
pub struct BudgetQuery {
    /// Show only this API key's consumption (the key is hashed before
    /// lookup). `anonymous` selects the shared anonymous budget.
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BudgetEndpointConfig {
    pub epsilon: f64,
    pub delta: f64,
}

#[derive(Debug, Serialize)]
pub struct BudgetReport {
    pub enabled: bool,
    pub limit_epsilon: f64,
    pub limit_delta: f64,
    pub window_secs: u64,
    pub endpoints: std::collections::BTreeMap<&'static str, BudgetEndpointConfig>,
    pub keys: Vec<BudgetUsage>,
}

/// `GET /v1/admin/privacy/budget` — per-key privacy-budget consumption.
///
/// Requires `Authorization: Bearer <RWA_ADMIN_TOKEN>`.
pub async fn budget(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(query): Query<BudgetQuery>,
) -> Result<Json<BudgetReport>, ApiError> {
    let expected = std::env::var("RWA_ADMIN_TOKEN").unwrap_or_default();
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !token_matches(&expected, supplied) {
        return Err(ApiError::Unauthorized("admin token required".into()));
    }

    let engine = &state.privacy;
    let mut keys = engine.usage().await.map_err(|e| {
        tracing::error!(error = %e, "privacy budget store unavailable");
        ApiError::Unavailable("privacy budget store unavailable".into())
    })?;
    if let Some(api_key) = query.api_key.as_deref() {
        let wanted = if api_key == ANONYMOUS {
            ANONYMOUS.to_owned()
        } else {
            key_id_for(api_key)
        };
        keys.retain(|usage| usage.key_id == wanted);
    }

    let config = &engine.config;
    let endpoint = |params: crate::analytics::differential_privacy::PrivacyParams| {
        BudgetEndpointConfig {
            epsilon: params.epsilon,
            delta: params.delta_or_zero(),
        }
    };
    Ok(Json(BudgetReport {
        enabled: config.enabled,
        limit_epsilon: config.budget_epsilon,
        limit_delta: config.budget_delta,
        window_secs: config.budget_window.as_secs(),
        endpoints: [("stats", endpoint(config.stats)), ("holders", endpoint(config.holders))]
            .into_iter()
            .collect(),
        keys,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{body::Body, http::Request, routing::get, Router};
    use tower::ServiceExt as _;

    use super::*;
    use crate::analytics::differential_privacy::{
        MemoryBudgetStore, PrivacyConfig, PrivacyEngine,
    };
    use crate::indexer::Snapshot;
    use crate::routes::test_support::{asset, state_with};

    pub(crate) fn private_state(budget_epsilon: f64) -> AppState {
        let mut snap = Snapshot::default();
        snap.assets.push(asset(1));
        snap.holders.insert(
            1,
            vec![crate::models::Holder {
                address: "GA".into(),
                balance: "10".into(),
                share_percent: 100.0,
            }],
        );
        let mut state = state_with(snap);
        state.privacy = Arc::new(PrivacyEngine::new(
            PrivacyConfig {
                budget_epsilon,
                ..PrivacyConfig::default()
            },
            Box::<MemoryBudgetStore>::default(),
        ));
        state
    }

    fn app(state: AppState) -> Router {
        Router::new()
            .route("/v1/stats", get(crate::routes::stats::get))
            .route(
                "/v1/assets/:id/holders",
                get(crate::routes::holders::list_or_aggregate),
            )
            .route("/v1/admin/privacy/budget", get(budget))
            .with_state(state)
    }

    async fn call(app: &Router, uri: &str, auth: Option<&str>) -> Response {
        let mut request = Request::builder().uri(uri);
        if let Some(token) = auth {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        app.clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn exhausted_budget_returns_429_with_retry_after() {
        // Default stats epsilon is 0.5: two queries fit in a 1.0 budget.
        let app = app(private_state(1.0));
        for _ in 0..2 {
            let response = call(&app, "/v1/stats", None).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.headers().contains_key("X-Privacy-Budget-Remaining"));
        }
        let response = call(&app, "/v1/stats", None).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after: u64 = response.headers()[header::RETRY_AFTER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(retry_after > 0);

        // The holders aggregate view draws on the same budget.
        let response = call(&app, "/v1/assets/1/holders?view=aggregate", None).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        // The row-level holder list is not an aggregate and is not metered.
        let response = call(&app, "/v1/assets/1/holders", None).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn admin_endpoint_reports_consumption() {
        std::env::set_var("RWA_ADMIN_TOKEN", "privacy-admin-test");
        let app = app(private_state(10.0));
        call(&app, "/v1/stats", None).await;
        call(&app, "/v1/assets/1/holders?view=aggregate", None).await;

        let unauthorized = call(&app, "/v1/admin/privacy/budget", None).await;
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let response = call(
            &app,
            "/v1/admin/privacy/budget?api_key=anonymous",
            Some("privacy-admin-test"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["enabled"], true);
        assert_eq!(body["keys"][0]["key_id"], "anonymous");
        assert_eq!(body["keys"][0]["queries"], 2);
        assert!((body["keys"][0]["spent_epsilon"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(body["endpoints"]["stats"]["epsilon"], 0.5);
    }
}
