//! # Tessera SDK
//!
//! An asynchronous, strongly-typed Rust client library for the Tessera API.
//!
//! ## Features
//!
//! - Complete coverage of all REST API endpoints
//! - Strongly-typed request/response models
//! - Built-in retry logic with exponential backoff and jitter
//! - Rate-limit handling with `Retry-After` header support
//! - Field selection support for sparse asset responses
//! - Export functionality (CSV/Parquet)
//! - Audit log verification
//!
//! ## Quick Start
//!
//! ```no_run
//! use tessera_sdk::{TesseraClient, ClientConfig};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = TesseraClient::new(ClientConfig {
//!     base_url: "http://localhost:8080".to_string(),
//!     ..Default::default()
//! })?;
//!
//! // Get platform stats
//! let stats = client.get_stats().await?;
//! println!("Total assets: {}", stats.total_assets);
//! println!("TVL: ${}", stats.tvl_usd);
//!
//! // List all assets
//! let assets = client.list_assets(Default::default()).await?;
//! for asset in &assets {
//!     println!("{}: {} (${})", asset.symbol, asset.name, asset.valuation_usd);
//! }
//!
//! // Get asset detail with analytics
//! let analytics = client
//!     .get_asset_analytics(1, Default::default())
//!     .await?;
//! println!("Current window volume: {}", analytics.current_window.trading_volume);
//!
//! # Ok(())
//! # }
//! ```
//!
//! ## Configuration
//!
//! ```no_run
//! use tessera_sdk::ClientConfig;
//! use std::time::Duration;
//!
//! let config = ClientConfig {
//!     base_url: "https://api.tessera.example.com".to_string(),
//!     timeout_secs: 60,
//!     max_retries: 5,
//!     initial_backoff_ms: 1000,
//!     max_backoff_ms: 60_000,
//!     auth_token: Some("your-token".to_string()),
//! };
//! ```
//!
//! ## Error Handling
//!
//! ```no_run
//! use tessera_sdk::{TesseraClient, TesseraError};
//!
//! # async fn example() {
//! let client = TesseraClient::default().unwrap();
//!
//! match client.get_asset(99999, Default::default()).await {
//!     Ok(asset) => println!("Found: {}", asset.name),
//!     Err(TesseraError::NotFound { resource }) => {
//!         println!("Not found: {}", resource);
//!     }
//!     Err(TesseraError::RateLimited { retry_after, .. }) => {
//!         println!("Rate limited, retry after: {:?}", retry_after);
//!     }
//!     Err(e) => println!("Error: {}", e),
//! }
//! # }
//! ```

pub mod client;
pub mod error;
pub mod models;

// Re-exports for convenience
pub use client::{ClientConfig, TesseraClient};
pub use error::{ApiErrorDetail, Result, TesseraError};
pub use models::*;

/// Library version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
