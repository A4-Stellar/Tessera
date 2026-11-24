//! Sandbox lifecycle endpoints.
//!
//! `POST /sandbox/reset` wipes and re-seeds only the calling tenant's data.
//! Every statement runs inside a transaction whose `app.sandbox_tenant_id`
//! GUC is pinned to the validated `X-Sandbox-Tenant-ID` header, so the
//! Postgres RLS policies — not the SQL text — enforce the isolation.

use std::time::Instant;

use axum::{extract::State, http::HeaderMap, Json};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;

use crate::{
    indexer::AppState,
    middleware::sandbox::{begin_tenant_transaction, SandboxTenant},
    routes::ApiError,
};

/// The ordered wipe-and-reseed plan. Kept as data so it can be unit-tested
/// without a database (and audited: no statement interpolates a tenant id).
pub const RESET_STATEMENTS: &[&str] = &[
    // Children before parents: FK-free tables, but the order still keeps the
    // operation deterministic for the audit row's counts.
    "DELETE FROM sandbox_holders WHERE tenant_id = $1",
    "DELETE FROM sandbox_dividends WHERE tenant_id = $1",
    "DELETE FROM sandbox_assets WHERE tenant_id = $1",
    "DELETE FROM sandbox_resets WHERE tenant_id = $1",
    "INSERT INTO sandbox_assets (asset_id, tenant_id, name, issuer, asset_type, jurisdiction, price_cents) \
         SELECT asset_id, $1, name, issuer, asset_type, jurisdiction, price_cents FROM sandbox_template_assets",
    "INSERT INTO sandbox_holders (holder_id, tenant_id, asset_id, address, balance, jurisdiction, joined_day) \
         SELECT holder_id, $1, asset_id, address, balance, jurisdiction, joined_day FROM sandbox_template_holders",
    "INSERT INTO sandbox_dividends (dividend_id, tenant_id, asset_id, amount_cents, interval_days) \
         SELECT dividend_id, $1, asset_id, amount_cents, interval_days FROM sandbox_template_dividends",
    "INSERT INTO sandbox_resets (tenant_id, duration_ms, rows_removed) VALUES ($1, $2, $3)",
];

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ResetSummary {
    pub tenant_id: String,
    pub status: &'static str,
    pub duration_ms: u64,
    pub assets: u64,
    pub holders: u64,
    pub dividends: u64,
    pub reset_at: String,
}

/// Resolve the caller's tenant and require a tenant header. Sandbox mode is
/// opt-in per request; an ordinary production client never touches these rows.
fn caller_tenant(headers: &HeaderMap) -> Result<SandboxTenant, ApiError> {
    SandboxTenant::from_headers(headers)
        .map_err(ApiError::BadRequest)?
        .ok_or_else(|| {
            ApiError::BadRequest("X-Sandbox-Tenant-ID is required for sandbox operations".into())
        })
}

/// Wipe and re-seed the tenant's data from the template tables. The tenant
/// header is mandatory: RLS scopes the operation, so a caller can only ever
/// reset its own rows even if it supplies somebody else's id in the body.
pub async fn reset(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ResetSummary>, ApiError> {
    let tenant = caller_tenant(&headers)?;
    let pool = state
        .sandbox_db
        .as_ref()
        .ok_or_else(|| ApiError::Unavailable("sandbox database is not configured".into()))?;

    let started = Instant::now();
    let mut tx = begin_tenant_transaction(pool, &tenant)
        .await
        .map_err(|error| ApiError::Unavailable(format!("sandbox transaction failed: {error}")))?;

    let removed_holders = sqlx::query(RESET_STATEMENTS[0])
        .bind(tenant.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?
        .rows_affected();
    let removed_dividends = sqlx::query(RESET_STATEMENTS[1])
        .bind(tenant.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?
        .rows_affected();
    let removed_assets = sqlx::query(RESET_STATEMENTS[2])
        .bind(tenant.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?
        .rows_affected();
    sqlx::query(RESET_STATEMENTS[3])
        .bind(tenant.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?;

    for statement in &RESET_STATEMENTS[4..7] {
        sqlx::query(statement)
            .bind(tenant.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::Unavailable(e.to_string()))?;
    }

    let duration_ms = started.elapsed().as_millis() as i64;
    let rows_removed = (removed_assets + removed_dividends + removed_holders) as i64;
    sqlx::query(RESET_STATEMENTS[7])
        .bind(tenant.as_str())
        .bind(duration_ms)
        .bind(rows_removed)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?;

    Ok(Json(ResetSummary {
        tenant_id: tenant.as_str().to_owned(),
        status: "reset",
        duration_ms: duration_ms as u64,
        assets: removed_assets,
        holders: removed_holders,
        dividends: removed_dividends,
        reset_at: Utc::now().to_rfc3339(),
    }))
}

/// Cheap tenant-scoped status probe, useful for asserting isolation in E2E.
pub async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tenant = caller_tenant(&headers)?;
    let pool = state
        .sandbox_db
        .as_ref()
        .ok_or_else(|| ApiError::Unavailable("sandbox database is not configured".into()))?;
    let mut tx = begin_tenant_transaction(pool, &tenant)
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?;
    let assets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sandbox_assets")
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?;
    let holders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sandbox_holders")
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?;
    let dividends: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sandbox_dividends")
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::Unavailable(e.to_string()))?;
    Ok(Json(json!({
        "tenant_id": tenant.as_str(),
        "mode": "sandbox",
        "assets": assets,
        "holders": holders,
        "dividends": dividends,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::sandbox::SANDBOX_TENANT_HEADER;

    #[test]
    fn reset_plan_never_interpolates_the_tenant_id() {
        assert!(RESET_STATEMENTS.iter().all(|sql| !sql.contains("{}")));
        assert!(RESET_STATEMENTS.iter().all(|sql| !sql.contains("format!")));
        // Every mutating statement is parameterised with $1.
        for sql in RESET_STATEMENTS {
            assert!(sql.contains("$1"), "statement is not tenant-bound: {sql}");
        }
    }

    #[test]
    fn reset_plan_deletes_children_before_parents() {
        let deletes = RESET_STATEMENTS
            .iter()
            .take(4)
            .map(|s| s.split_whitespace().nth(2).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            deletes,
            vec![
                "sandbox_holders",
                "sandbox_dividends",
                "sandbox_assets",
                "sandbox_resets"
            ]
        );
    }

    #[test]
    fn requires_a_tenant_header() {
        assert!(caller_tenant(&HeaderMap::new()).is_err());
        let mut headers = HeaderMap::new();
        headers.insert(SANDBOX_TENANT_HEADER, "Tenant-1".parse().unwrap());
        assert_eq!(caller_tenant(&headers).unwrap().as_str(), "tenant-1");
    }
}
