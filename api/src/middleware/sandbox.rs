//! Sandbox tenant identity and Postgres RLS session binding.
//!
//! A sandbox client sends `X-Sandbox-Tenant-ID: <id>` on every request. The
//! value is validated once (same rules as `X-Tenant-ID`), stored as an extension
//! and used to pin the `app.sandbox_tenant_id` GUC on a per-request
//! transaction. Postgres row-level security then makes cross-tenant reads and
//! writes structurally impossible — there is no `WHERE tenant_id = ...` in the
//! handlers that a bug could drop.

use axum::{
    extract::Request,
    http::{header::HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};

pub const SANDBOX_TENANT_HEADER: &str = "x-sandbox-tenant-id";

/// A validated sandbox tenant identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SandboxTenant(String);

impl SandboxTenant {
    /// Validate a raw header value. `None` means "no sandbox header supplied".
    pub fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, String> {
        match headers.get(SANDBOX_TENANT_HEADER) {
            None => Ok(None),
            Some(value) => {
                let raw = value
                    .to_str()
                    .map_err(|_| "X-Sandbox-Tenant-ID must be valid ASCII".to_owned())?;
                let tenant =
                    crate::middleware::tenant::tenant_id_from_header(raw).ok_or_else(|| {
                        "X-Sandbox-Tenant-ID must be 1-128 ASCII letters, digits, '-' or '_'"
                            .to_owned()
                    })?;
                Ok(Some(Self(tenant)))
            }
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SandboxTenant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reject malformed sandbox tenant headers before any handler runs, and make a
/// validated tenant available to handlers via `Request::extensions()`.
pub async fn sandbox_tenant_middleware(req: Request, next: Next) -> Response {
    let tenant = match SandboxTenant::from_headers(req.headers()) {
        Ok(tenant) => tenant,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_sandbox_tenant_id", "message": message })),
            )
                .into_response();
        }
    };
    let mut req = req;
    if let Some(tenant) = tenant {
        req.extensions_mut().insert(tenant);
    }
    next.run(req).await
}

/// Start a transaction pinned to one tenant. Every query issued through the
/// returned transaction sees only that tenant's rows. The GUC is `SET LOCAL`,
/// so it cannot leak into another request that reuses the pooled connection.
pub async fn begin_tenant_transaction(
    pool: &PgPool,
    tenant: &SandboxTenant,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Safe to interpolate: SandboxTenant can only contain [A-Za-z0-9_-].
    let statement = format!("SET LOCAL app.sandbox_tenant_id = '{}'", tenant.as_str());
    sqlx::query(&statement).execute(&mut *tx).await?;
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn accepts_valid_tenant_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(SANDBOX_TENANT_HEADER, HeaderValue::from_static("Acme-01"));
        assert_eq!(
            SandboxTenant::from_headers(&headers).unwrap(),
            Some(SandboxTenant("acme-01".to_owned()))
        );
    }

    #[test]
    fn rejects_headers_that_could_break_out_of_the_guc() {
        for bad in ["", "a:b", "tenant'; DROP TABLE sandbox_assets; --", "x y"] {
            let mut headers = HeaderMap::new();
            headers.insert(
                SANDBOX_TENANT_HEADER,
                HeaderValue::from_str(bad).unwrap_or_else(|_| HeaderValue::from_static("x y")),
            );
            assert!(
                SandboxTenant::from_headers(&headers).is_err(),
                "must reject {bad:?}"
            );
        }
    }

    #[test]
    fn absent_header_is_not_an_error() {
        assert_eq!(
            SandboxTenant::from_headers(&HeaderMap::new()).unwrap(),
            None
        );
    }
}
