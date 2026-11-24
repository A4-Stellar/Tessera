//! EDGAR cap-table import and report signing (issue #476). Both routes need
//! `Authorization: Bearer $RWA_ADMIN_TOKEN`.
//!
//! - `POST /v1/admin/cap-table/import-edgar` fetches the issuer's Form 4
//!   filings, reconciles them with the indexed holders and returns a signed
//!   report. Discrepancies above 1 % of total supply go to the compliance
//!   webhook and the audit trail.
//! - `POST /v1/admin/cap-table/reports/sign` signs a report built in the
//!   browser (from an uploaded CSV) so the docs component can export a signed
//!   PDF.

use std::collections::HashMap;

use axum::{extract::State, http::HeaderMap, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::bridge::require_admin;
use super::ApiError;
use crate::audit::AuditAction;
use crate::indexer::AppState;
use crate::services::edgar::{reconcile, OnChainHolder, ReconciliationReport, ReportSignature};

const MAX_FILINGS: usize = 40;

#[derive(Debug, Deserialize)]
pub struct ImportRequest {
    pub asset_id: u64,
    /// Issuer CIK, with or without leading zeros.
    pub cik: String,
    /// Reporting-owner CIK → Stellar address.
    #[serde(default)]
    pub holder_map: HashMap<String, String>,
    /// Tokens per reported share (default 1).
    pub tokens_per_share: Option<f64>,
    /// How many recent Form 4 filings to read (default and max 40).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct SignedReport {
    pub report: ReconciliationReport,
    pub signature: ReportSignature,
}

fn tokens(raw: &str, decimals: u32) -> f64 {
    raw.parse::<f64>().unwrap_or(0.0) / 10f64.powi(decimals as i32)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// `POST /v1/admin/cap-table/import-edgar`
pub async fn import_edgar(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ImportRequest>,
) -> Result<Json<SignedReport>, ApiError> {
    require_admin(&headers)?;
    if !req.cik.chars().any(|c| c.is_ascii_digit()) || req.cik.len() > 10 {
        return Err(ApiError::BadRequest("cik must be up to 10 digits".into()));
    }
    let tokens_per_share = req.tokens_per_share.unwrap_or(1.0);
    if !(tokens_per_share.is_finite() && tokens_per_share > 0.0) {
        return Err(ApiError::BadRequest(
            "tokens_per_share must be positive".into(),
        ));
    }
    let snap = state.snapshot();
    let asset = snap
        .asset(req.asset_id)
        .ok_or_else(|| ApiError::NotFound(format!("no asset with id {}", req.asset_id)))?
        .clone();
    let holders = snap.holders.get(&req.asset_id).cloned().unwrap_or_default();

    let filings = state
        .edgar
        .holdings(
            &req.cik,
            req.limit.unwrap_or(MAX_FILINGS).clamp(1, MAX_FILINGS),
        )
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?;

    let on_chain: Vec<OnChainHolder<'_>> = holders
        .iter()
        .map(|h| OnChainHolder {
            address: &h.address,
            tokens: tokens(&h.balance, asset.decimals),
        })
        .collect();
    let report = reconcile(
        req.asset_id,
        &req.cik,
        &on_chain,
        &filings,
        &req.holder_map,
        tokens_per_share,
        tokens(&asset.total_supply, asset.decimals),
        now(),
    );

    for row in report.rows.iter().filter(|r| r.compliance_alert) {
        state.compliance_webhooks.discrepancy(&report, row).await;
        let _ = state
            .audit
            .append(
                AuditAction::AdministrativeAction {
                    action: "cap_table.edgar_discrepancy".into(),
                    details: serde_json::to_value(row).unwrap_or_default(),
                },
                "edgar-import".into(),
                Some(asset.token_contract.clone()),
                json!({ "asset_id": req.asset_id, "issuer_cik": report.issuer_cik }),
            )
            .await;
    }

    let signature = state.report_signer.sign(&report, now());
    Ok(Json(SignedReport { report, signature }))
}

#[derive(Debug, Deserialize)]
pub struct SignRequest {
    /// Any JSON report. It is signed exactly as sent (canonicalized).
    pub report: Value,
}

/// `POST /v1/admin/cap-table/reports/sign`
pub async fn sign_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SignRequest>,
) -> Result<Json<ReportSignature>, ApiError> {
    require_admin(&headers)?;
    if !req.report.is_object() {
        return Err(ApiError::BadRequest("report must be a JSON object".into()));
    }
    let signature = state.report_signer.sign(&req.report, now());
    let _ = state
        .audit
        .append(
            AuditAction::AdministrativeAction {
                action: "cap_table.report_signed".into(),
                details: json!({ "digest": signature.digest }),
            },
            "cap-table-report".into(),
            None,
            Value::Null,
        )
        .await;
    Ok(Json(signature))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::{body::Body, http::Request, http::StatusCode, Router};
    use tower::ServiceExt as _;

    use super::*;
    use crate::indexer::Snapshot;
    use crate::models::Holder;
    use crate::routes::test_support::{asset, state_with};
    use crate::services::edgar::tests::fixture_holdings;
    use crate::services::edgar::{
        verify_report, ComplianceAlerts, EdgarError, EdgarSource, Form4Holding, ReconciliationRow,
    };

    struct Fixtures;
    #[async_trait]
    impl EdgarSource for Fixtures {
        async fn holdings(
            &self,
            _cik: &str,
            _limit: usize,
        ) -> Result<Vec<Form4Holding>, EdgarError> {
            Ok(fixture_holdings())
        }
    }

    #[derive(Default)]
    struct Alerts(Mutex<Vec<String>>);
    #[async_trait]
    impl ComplianceAlerts for Alerts {
        async fn discrepancy(&self, _r: &ReconciliationReport, row: &ReconciliationRow) {
            self.0
                .lock()
                .unwrap()
                .push(row.address.clone().unwrap_or_default());
        }
    }

    fn app(alerts: Arc<Alerts>) -> (Router, AppState) {
        let mut a = asset(1);
        a.decimals = 0;
        a.total_supply = "1000000".into();
        let holders = vec![
            Holder {
                address: "GDOE".into(),
                balance: "115000".into(),
                share_percent: 11.5,
            },
            Holder {
                address: "GROE".into(),
                balance: "47000".into(),
                share_percent: 4.7,
            },
        ];
        let mut state = state_with(Snapshot {
            assets: vec![a],
            holders: [(1, holders)].into(),
            ..Snapshot::default()
        });
        state.edgar = Arc::new(Fixtures);
        state.compliance_webhooks = alerts;
        let router = Router::new()
            .route(
                "/v1/admin/cap-table/import-edgar",
                axum::routing::post(import_edgar),
            )
            .route(
                "/v1/admin/cap-table/reports/sign",
                axum::routing::post(sign_report),
            )
            .with_state(state.clone());
        (router, state)
    }

    fn post(uri: &str, body: Value, token: Option<&str>) -> Request<Body> {
        let mut b = Request::post(uri).header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn import_reconciles_signs_and_alerts_compliance() {
        std::env::set_var("RWA_ADMIN_TOKEN", "test-admin-token");
        let alerts = Arc::new(Alerts::default());
        let (app, state) = app(alerts.clone());
        let body = json!({
            "asset_id": 1,
            "cik": "9990001",
            "holder_map": { "0009990101": "GDOE", "0009990102": "GROE", "0009990103": "GSMITH" }
        });

        let unauth = app
            .clone()
            .oneshot(post("/v1/admin/cap-table/import-edgar", body.clone(), None))
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        let resp = app
            .oneshot(post(
                "/v1/admin/cap-table/import-edgar",
                body,
                Some("test-admin-token"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let report: ReconciliationReport = serde_json::from_value(v["report"].clone()).unwrap();
        let signature: ReportSignature = serde_json::from_value(v["signature"].clone()).unwrap();
        assert!(verify_report(&report, &signature));

        // ROE: 47 000 on chain vs 40 000 filed = +17.5 %, red, 0.7 % of supply.
        let roe = report
            .rows
            .iter()
            .find(|r| r.address.as_deref() == Some("GROE"))
            .unwrap();
        assert_eq!(serde_json::to_value(roe.highlight).unwrap(), "red");
        assert!(!roe.compliance_alert);
        // SMITH: filed 60 000, nothing on chain = 6 % of supply.
        assert_eq!(alerts.0.lock().unwrap().as_slice(), ["GSMITH"]);
        let (entries, _) = state.audit.list_entries(0, 10).await;
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn sign_endpoint_signs_browser_reports() {
        std::env::set_var("RWA_ADMIN_TOKEN", "test-admin-token");
        let (app, _) = app(Arc::default());
        let report = json!({ "rows": [{ "address": "GA", "kind": "new" }], "source": "csv" });
        let resp = app
            .oneshot(post(
                "/v1/admin/cap-table/reports/sign",
                json!({ "report": report.clone() }),
                Some("test-admin-token"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let sig: ReportSignature = serde_json::from_slice(&bytes).unwrap();
        assert!(verify_report(&report, &sig));
    }
}
