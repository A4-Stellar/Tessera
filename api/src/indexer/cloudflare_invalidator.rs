//! Cloudflare KV edge cache invalidation client (Issue #41).
//!
//! On each Soroban polling cycle, pushes the list of invalidated
//! contract IDs to Cloudflare KV so edge workers bypass cache for affected assets.

use std::time::Duration;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct CloudflareKvConfig {
    pub account_id: String,
    pub namespace_id: String,
    pub api_token: String,
    pub base_url: String,
}

impl CloudflareKvConfig {
    pub fn from_env() -> Option<Self> {
        let account_id = std::env::var("CF_ACCOUNT_ID").ok()?;
        let namespace_id = std::env::var("CF_KV_NAMESPACE_ID").ok()?;
        let api_token = std::env::var("CF_API_TOKEN").ok()?;
        if account_id.is_empty() || namespace_id.is_empty() || api_token.is_empty() {
            return None;
        }
        let base_url = std::env::var("CF_API_BASE_URL")
            .unwrap_or_else(|_| "https://api.cloudflare.com/client/v4".to_string());
        Some(Self {
            account_id,
            namespace_id,
            api_token,
            base_url,
        })
    }
}

pub struct CloudflareKvInvalidator {
    config: CloudflareKvConfig,
    client: reqwest::Client,
}

impl CloudflareKvInvalidator {
    pub fn new(config: CloudflareKvConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self { config, client }
    }

    /// Push list of invalidated contract IDs to Cloudflare KV with a 60-second TTL.
    pub async fn push_invalidations(&self, contract_ids: &[String]) -> Result<(), String> {
        if contract_ids.is_empty() {
            return Ok(());
        }

        let url = format!(
            "{}/accounts/{}/storage/kv/namespaces/{}/values/invalidated_contracts",
            self.config.base_url.trim_end_matches('/'),
            self.config.account_id,
            self.config.namespace_id
        );

        let json_body = match serde_json::to_string(contract_ids) {
            Ok(j) => j,
            Err(e) => return Err(format!("failed to serialize invalidations: {e}")),
        };

        let req = self
            .client
            .put(&url)
            .query(&[("expiration_ttl", "60")])
            .header("Authorization", format!("Bearer {}", self.config.api_token))
            .header("Content-Type", "application/json")
            .body(json_body);

        match req.send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    debug!(count = contract_ids.len(), "pushed contract invalidations to Cloudflare KV");
                    Ok(())
                } else {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    warn!(status = %status, response = %text, "failed to push invalidations to Cloudflare KV");
                    Err(format!("Cloudflare KV responded with status {status}: {text}"))
                }
            }
            Err(e) => {
                warn!(error = %e, "network error pushing invalidations to Cloudflare KV");
                Err(format!("Cloudflare KV request error: {e}"))
            }
        }
    }
}
