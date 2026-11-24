//! Rust sandbox example — production SDK, sandbox tenant header.
use std::env;

use tessera_sdk::{ClientConfig, TesseraClient};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api = env::var("TESSERA_SANDBOX_URL")
        .unwrap_or_else(|_| "http://localhost:8081".to_owned());
    let tenant = env::var("X_SANDBOX_TENANT_ID").unwrap_or_else(|_| "acme-1".to_owned());

    let client = TesseraClient::new(ClientConfig {
        base_url: api.clone(),
        default_headers: [("X-Sandbox-Tenant-ID".to_owned(), tenant.clone())]
            .into_iter()
            .collect(),
        ..Default::default()
    })?;

    // Identical call shape to production.
    let stats = client.get_stats().await?;
    println!("tenant={tenant} assets={}", stats.total_assets);

    // Sandbox-only reset, same HTTP surface.
    let response = reqwest::Client::new()
        .post(format!("{api}/sandbox/reset"))
        .header("X-Sandbox-Tenant-ID", &tenant)
        .send()
        .await?;
    println!("reset status={}", response.status());
    Ok(())
}
