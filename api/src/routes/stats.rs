//! `GET /stats`.
//!
//! With differential privacy enabled (issue #26) the holder count and TVL
//! are released through the Laplace mechanism and each request is charged
//! against the caller's privacy budget; see
//! [`crate::analytics::differential_privacy`].

use axum::{extract::State, http::HeaderMap, Json};

use super::privacy::{api_key, privacy_headers};
use crate::analytics::differential_privacy::{
    clipped_tvl_cents, distinct_holders, Aggregate, PrivacyRejection, PrivateEndpoint,
};
use crate::indexer::AppState;
use crate::models::Stats;

/// Aggregates `/stats` releases under differential privacy.
const PRIVATE_AGGREGATES: usize = 2;

/// Platform-wide statistics: asset count, TVL, holders, distributions.
pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<Stats>), PrivacyRejection> {
    let snap = state.snapshot();
    let Some(query) = state
        .privacy
        .begin(PrivateEndpoint::Stats, api_key(&headers), PRIVATE_AGGREGATES)
        .await?
    else {
        return Ok((HeaderMap::new(), Json(snap.stats)));
    };

    let mut stats = snap.stats;
    let mut rng = rand::rng();
    let bounds = state.privacy.config.bounds;
    let tvl = clipped_tvl_cents(&snap.assets, &snap.holders, bounds.tvl_clip_cents);
    let noisy_tvl_cents = query.release(Aggregate::TvlSum, tvl, &mut rng).round();
    stats.tvl_cents = format!("{noisy_tvl_cents:.0}");
    stats.tvl_usd = noisy_tvl_cents / 100.0;
    stats.total_holders =
        query.release_count(Aggregate::HolderCount, distinct_holders(&snap.holders), &mut rng);
    Ok((privacy_headers(&query), Json(stats)))
}

#[cfg(test)]
mod tests {
    use axum::{extract::State, http::HeaderMap};
    use serde_json::json;

    use super::get;
    use crate::indexer::Snapshot;
    use crate::models::Stats;
    use crate::routes::test_support::state_with;

    #[tokio::test]
    async fn stats_json_shape() {
        let snap = Snapshot {
            stats: Stats {
                total_assets: 3,
                active_assets: 2,
                tvl_cents: "123456789".to_string(),
                tvl_usd: 1_234_567.89,
                total_holders: 10,
                total_distributions: 4,
                last_indexed_ledger: 555,
                last_updated: Some("2024-01-01T00:00:00Z".to_string()),
            },
            ..Snapshot::default()
        };
        let state = state_with(snap);

        let body = get(State(state), HeaderMap::new()).await.unwrap().1 .0;
        let value = serde_json::to_value(&body).expect("serialize stats");

        assert!(value["tvl_cents"].is_string());
        assert_eq!(value["tvl_cents"], json!("123456789"));
        assert!(value["tvl_usd"].is_number());
        assert_eq!(value["tvl_usd"], json!(1_234_567.89));
        assert_eq!(value["last_updated"], json!("2024-01-01T00:00:00Z"));
    }

    #[tokio::test]
    async fn stats_empty_snapshot_reports_zeroed_defaults() {
        let state = state_with(Snapshot::default());

        let body = get(State(state), HeaderMap::new()).await.unwrap().1 .0;
        let value = serde_json::to_value(&body).expect("serialize stats");

        assert!(
            value.is_object(),
            "empty snapshot should still serialize as JSON object"
        );
        assert_eq!(value["total_assets"], 0);
        assert_eq!(value["active_assets"], 0);
        assert_eq!(value["tvl_cents"], "");
        assert_eq!(value["tvl_usd"], 0.0);
        assert_eq!(value["total_holders"], 0);
        assert_eq!(value["total_distributions"], 0);
        assert_eq!(value["last_indexed_ledger"], 0);
        assert!(value["last_updated"].is_null());
    }

    /// The serialized key set must exactly match the OpenAPI schema's
    /// `required` list (docs/public/openapi.json → components.schemas.Stats),
    /// so a model/spec drift fails CI instead of shipping (see #187).
    #[tokio::test]
    async fn stats_response_key_set_matches_openapi_schema() {
        let snap = Snapshot {
            stats: Stats {
                total_assets: 3,
                active_assets: 2,
                tvl_cents: "123456789".to_string(),
                tvl_usd: 1_234_567.89,
                total_holders: 10,
                total_distributions: 4,
                last_indexed_ledger: 555,
                last_updated: Some("2024-01-01T00:00:00Z".to_string()),
            },
            ..Snapshot::default()
        };
        let state = state_with(snap);

        let body = get(State(state), HeaderMap::new()).await.unwrap().1 .0;
        let value = serde_json::to_value(&body).expect("serialize stats");

        let openapi: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/public/openapi.json"))
                .expect("docs/public/openapi.json is valid JSON");
        let required = openapi["components"]["schemas"]["Stats"]["required"]
            .as_array()
            .expect("Stats schema has a required list")
            .iter()
            .map(|v| v.as_str().expect("required entries are strings"))
            .collect::<Vec<_>>();

        let mut actual = value
            .as_object()
            .expect("serialized stats is a JSON object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut expected = required;
        actual.sort_unstable();
        expected.sort_unstable();

        assert_eq!(
            actual, expected,
            "serialized /stats keys must exactly match the OpenAPI schema's required list"
        );
    }
}
