//! `GET /v1/security/anomalies` - active risk flags for maintainer review
//! (issue #101). See [`crate::services::anomaly_detector`].

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::indexer::AppState;
use crate::services::anomaly_detector::{RiskFlag, Z_THRESHOLD};

#[derive(Debug, Deserialize, Default)]
pub struct AnomalyQuery {
    /// Only flags for this account.
    pub account: Option<String>,
    /// Only flags whose peak Z-score is at least this.
    pub min_z: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct AnomaliesResponse {
    pub threshold: f64,
    pub count: usize,
    pub flags: Vec<RiskFlag>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<AnomalyQuery>,
) -> Json<AnomaliesResponse> {
    let now = chrono::Utc::now().timestamp().max(0) as u64;
    let flags: Vec<RiskFlag> = state
        .anomalies
        .active_flags(now)
        .into_iter()
        .filter(|f| q.account.as_deref().is_none_or(|a| a == f.account))
        .filter(|f| q.min_z.is_none_or(|z| f.peak_z_score >= z))
        .collect();
    Json(AnomaliesResponse {
        threshold: Z_THRESHOLD,
        count: flags.len(),
        flags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::anomaly_detector::{Observation, ObservationKind};

    #[tokio::test]
    async fn lists_active_flags_and_filters() {
        let state = AppState::for_test_empty();
        let now = chrono::Utc::now().timestamp() as u64;
        let base = now - 30 * 60;
        for m in 0..20u64 {
            state.anomalies.observe(&Observation {
                account: "GA".into(),
                timestamp: base + m * 60,
                kind: ObservationKind::Transfer {
                    amount: 100.0 + (m % 3) as f64,
                },
            });
        }
        state.anomalies.observe(&Observation {
            account: "GA".into(),
            timestamp: base + 20 * 60,
            kind: ObservationKind::Transfer { amount: 1e7 },
        });
        let Json(all) = list(State(state.clone()), Query(AnomalyQuery::default())).await;
        assert!(all.count >= 1);
        assert_eq!(all.threshold, 3.5);
        let Json(none) = list(
            State(state.clone()),
            Query(AnomalyQuery {
                account: Some("GZ".into()),
                min_z: None,
            }),
        )
        .await;
        assert_eq!(none.count, 0);
        let Json(high) = list(
            State(state),
            Query(AnomalyQuery {
                account: None,
                min_z: Some(1e9),
            }),
        )
        .await;
        assert_eq!(high.count, 0);
    }
}
