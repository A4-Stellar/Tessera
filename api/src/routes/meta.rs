//! API version metadata (`GET /v1/meta/versions`).
//!
//! The deprecation map is embedded at compile time so the version endpoint has
//! no runtime filesystem dependency (the API runs from a minimal container).
//! Parse failures are configuration errors and panic once, on first use, with
//! the offending line reported by the parser.

use std::sync::OnceLock;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

use crate::middleware::deprecation::DeprecationMap;

static VERSION_MAP: OnceLock<DeprecationMap> = OnceLock::new();

/// The process-wide, lazily parsed deprecation/version map.
pub fn versions_map() -> &'static DeprecationMap {
    VERSION_MAP.get_or_init(|| {
        DeprecationMap::from_toml_str(include_str!("../../deprecation_map.toml"))
            .expect("api/deprecation_map.toml is valid TOML in the restricted deprecation format")
    })
}

/// `GET /v1/meta/versions`: lifecycle status and sunset schedule for every API
/// version, including per-path deprecations.
pub async fn versions() -> Response {
    (StatusCode::OK, Json(versions_map().versions_response())).into_response()
}

/// Nested under `/v1/meta` by the router, yielding `/v1/meta/versions`.
pub fn router() -> Router {
    Router::new().route("/versions", get(versions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_map_has_both_versions() {
        let map = versions_map();
        assert_eq!(map.versions.get("v1").map(String::as_str), Some("active"));
        assert_eq!(map.versions.get("v2").map(String::as_str), Some("beta"));
        assert!(map.lookup("/version").is_some());
        assert!(map.lookup("/health").is_none());
    }
}
