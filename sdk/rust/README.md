# Tessera SDK for Rust

An asynchronous, strongly-typed Rust client library for the [Tessera](https://github.com/tessera/tessera) API.

## Features

- **Complete API coverage** — Every REST endpoint with strongly-typed request/response models
- **Built-in retry logic** — Exponential backoff with jitter for transient failures
- **Rate-limit handling** — Respects `Retry-After` headers on HTTP 429 responses
- **Field selection** — Sparse asset responses via the `fields` query parameter
- **Export support** — CSV and Parquet export with automatic filename detection
- **Audit verification** — Cryptographic Merkle proof verification
- **Async/await** — Built on `tokio` and `reqwest`

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
tessera-sdk = "0.1.0"
tokio = { version = "1", features = ["full"] }
```

## Quick Start

```rust
use tessera_sdk::{TesseraClient, ClientConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = TesseraClient::new(ClientConfig {
        base_url: "http://localhost:8080".to_string(),
        ..Default::default()
    })?;

    // Platform stats
    let stats = client.get_stats().await?;
    println!("Total assets: {}", stats.total_assets);
    println!("TVL: ${:.2}", stats.tvl_usd);

    // List assets
    let assets = client.list_assets(Default::default()).await?;
    for asset in &assets {
        println!("{}: {} (${:.2})", asset.symbol, asset.name, asset.valuation_usd);
    }

    // Asset analytics
    let analytics = client.get_asset_analytics(1, Default::default()).await?;
    println!("Volume: {}", analytics.current_window.trading_volume);

    Ok(())
}
```

## Configuration

```rust
use tessera_sdk::ClientConfig;

let config = ClientConfig {
    base_url: "https://api.tessera.example.com".to_string(),
    timeout_secs: 60,
    max_retries: 5,
    initial_backoff_ms: 1000,
    max_backoff_ms: 60_000,
    auth_token: Some("your-token".to_string()),
};
```

## API Reference

### Health & Metadata
- `health()` — Check API health
- `version()` — Get API version

### Stats & Events
- `get_stats()` — Platform-wide statistics
- `list_events()` — Recent contract events

### Assets
- `list_assets(query)` — List all assets
- `list_assets_sparse(query)` — List assets with field selection
- `get_asset(id, query)` — Get asset detail
- `get_asset_sparse(id, query)` — Get asset with field selection
- `get_asset_events(id, query)` — Get asset events
- `get_asset_analytics(id, query)` — Get asset analytics
- `list_holders(asset_id, query)` — List asset holders
- `get_compliance_summary(asset_id)` — Get compliance summary
- `list_dividends(asset_id)` — List dividend distributions
- `get_distribution(asset_id, distribution_id)` — Get specific distribution
- `export_asset(asset_id, query)` — Export asset data (CSV/Parquet)

### Holders & Compliance
- `get_holder(address)` — Get all holdings for an address
- `get_holder_compliance(address)` — Get compliance for an address
- `get_address_compliance(address)` — Get compliance status

### Security
- `list_anomalies(query)` — List security anomalies

### Audit
- `verify_audit(query)` — Verify audit log integrity
- `list_audit_entries(query)` — List audit entries
- `create_audit_entry(request)` — Create audit entry
- `get_audit_entry(sequence)` — Get entry with Merkle proof
- `list_audit_anchors()` — List anchor records
- `publish_anchor(request)` — Publish new anchor

## Error Handling

```rust
use tessera_sdk::TesseraError;

match client.get_asset(99999, Default::default()).await {
    Ok(asset) => println!("Found: {}", asset.name),
    Err(TesseraError::NotFound { resource }) => {
        println!("Not found: {}", resource);
    }
    Err(TesseraError::RateLimited { retry_after, .. }) => {
        println!("Rate limited, retry after: {:?}", retry_after);
    }
    Err(TesseraError::RetryExhausted { attempts, .. }) => {
        println!("Failed after {} attempts", attempts);
    }
    Err(e) => println!("Error: {}", e),
}
```

## License

MIT OR Apache-2.0
