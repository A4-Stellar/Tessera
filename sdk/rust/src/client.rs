//! Asynchronous Tessera API client with built-in retry logic.
//!
//! The client provides strongly-typed methods for every REST endpoint,
//! with automatic retry on transient failures using exponential backoff
//! and jitter. Rate-limit responses (HTTP 429) are handled by respecting
//! the `Retry-After` header.

use std::time::Duration;

use bytes::Bytes;
use reqwest::{Client, Method, RequestBuilder, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::{debug, instrument, warn};

use crate::error::{ApiErrorDetail, Result, TesseraError};
use crate::models::*;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the Tessera API client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Base URL for the API (e.g., `http://localhost:8080`).
    pub base_url: String,
    /// Request timeout in seconds.
    pub timeout_secs: u64,
    /// Maximum number of retry attempts for transient failures.
    pub max_retries: u32,
    /// Initial backoff duration in milliseconds.
    pub initial_backoff_ms: u64,
    /// Maximum backoff duration in milliseconds.
    pub max_backoff_ms: u64,
    /// Optional bearer token for authenticated endpoints.
    pub auth_token: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8080".to_string(),
            timeout_secs: 30,
            max_retries: 3,
            initial_backoff_ms: 500,
            max_backoff_ms: 30_000,
            auth_token: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Asynchronous client for the Tessera API.
///
/// Provides methods for every REST endpoint with automatic retry,
/// rate-limit handling, and strongly-typed responses.
///
/// # Example
///
/// ```no_run
/// use tessera_sdk::{TesseraClient, ClientConfig};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let config = ClientConfig {
///     base_url: "http://localhost:8080".to_string(),
///     ..Default::default()
/// };
/// let client = TesseraClient::new(config)?;
///
/// let stats = client.get_stats().await?;
/// println!("Total assets: {}", stats.total_assets);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct TesseraClient {
    http: Client,
    config: ClientConfig,
    base_url: Url,
}

impl TesseraClient {
    /// Create a new client with the given configuration.
    pub fn new(config: ClientConfig) -> Result<Self> {
        let base_url = Url::parse(&config.base_url).map_err(|_| TesseraError::InvalidUrl {
            url: config.base_url.clone(),
        })?;

        let http = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| TesseraError::Connection {
                message: format!("Failed to build HTTP client: {}", e),
                retryable: false,
            })?;

        Ok(Self {
            http,
            config,
            base_url,
        })
    }

    /// Create a new client with default configuration.
    pub fn default() -> Result<Self> {
        Self::new(ClientConfig::default())
    }

    /// Get a reference to the client configuration.
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    // -----------------------------------------------------------------------
    // Internal request methods
    // -----------------------------------------------------------------------

    /// Build a full URL from a path.
    fn url(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .map_err(|_| TesseraError::InvalidUrl {
                url: format!("{}{}", self.config.base_url, path),
            })
    }

    /// Build a request with common headers.
    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let url = self.url(path)?;
        let mut req = self.http.request(method, url);

        if let Some(ref token) = self.config.auth_token {
            req = req.bearer_auth(token);
        }

        Ok(req)
    }

    /// Execute a request with retry logic and return the deserialized response.
    #[instrument(skip(self), fields(method, path))]
    async fn execute<T>(&self, method: Method, path: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let body = self.execute_raw(method, path).await?;
        serde_json::from_slice(&body).map_err(|e| TesseraError::Serialization {
            message: e.to_string(),
            body: String::from_utf8_lossy(&body).to_string(),
        })
    }

    /// Execute a request with retry logic and return the raw bytes.
    #[instrument(skip(self), fields(method, path))]
    async fn execute_raw(&self, method: Method, path: &str) -> Result<Bytes> {
        let mut last_error: Option<TesseraError> = None;

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let delay = self.backoff_delay(attempt);
                debug!(attempt, delay_ms = delay.as_millis(), "Retrying request");
                tokio::time::sleep(delay).await;
            }

            match self.try_request(&method, path).await {
                Ok(bytes) => return Ok(bytes),
                Err(e) => {
                    if !e.is_retryable() {
                        return Err(e);
                    }

                    // Check for rate-limit with Retry-After
                    if let TesseraError::RateLimited { retry_after, .. } = &e {
                        if let Some(duration) = retry_after {
                            debug!(
                                attempt,
                                retry_after_secs = duration.as_secs(),
                                "Rate limited, waiting for Retry-After"
                            );
                            tokio::time::sleep(*duration).await;
                            // Don't count rate-limit waits against retry attempts
                            continue;
                        }
                    }

                    warn!(attempt, error = %e, "Request failed");
                    last_error = Some(e);
                }
            }
        }

        Err(TesseraError::RetryExhausted {
            attempts: self.config.max_retries + 1,
            last_error: Box::new(last_error.unwrap_or(TesseraError::Connection {
                message: "Unknown error".to_string(),
                retryable: false,
            })),
        })
    }

    /// Execute a single request attempt (no retry logic).
    async fn try_request(&self, method: &Method, path: &str) -> Result<Bytes> {
        let req = self.request(method.clone(), path)?;
        let response = req.send().await?;

        let status = response.status();

        if status.is_success() {
            return response.bytes().await.map_err(TesseraError::from);
        }

        // Handle error responses — extract headers before consuming body
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);

        let body = response.bytes().await.unwrap_or_default();
        let body_str = String::from_utf8_lossy(&body).to_string();

        // Try to parse structured error
        let error_detail: Option<ApiErrorBody> =
            serde_json::from_slice(&body).ok();

        match status {
            StatusCode::TOO_MANY_REQUESTS => {

                Err(TesseraError::RateLimited {
                    retry_after,
                    message: error_detail
                        .map(|d| d.message)
                        .unwrap_or_else(|| "Rate limited".to_string()),
                })
            }
            StatusCode::NOT_FOUND => Err(TesseraError::NotFound {
                resource: path.to_string(),
            }),
            StatusCode::BAD_REQUEST => Err(TesseraError::BadRequest {
                message: error_detail
                    .map(|d| d.message)
                    .unwrap_or_else(|| "Bad request".to_string()),
            }),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(TesseraError::Auth {
                message: error_detail
                    .map(|d| d.message)
                    .unwrap_or_else(|| "Authentication failed".to_string()),
            }),
            _ => {
                let detail = error_detail.unwrap_or(ApiErrorBody {
                    error: format!("http_{}", status.as_u16()),
                    message: body_str.clone(),
                });
                Err(TesseraError::Api(ApiErrorDetail {
                    status: status.as_u16(),
                    error: detail.error,
                    message: detail.message,
                }))
            }
        }
    }

    /// Calculate backoff delay with exponential backoff and jitter.
    fn backoff_delay(&self, attempt: u32) -> Duration {
        let base = self.config.initial_backoff_ms;
        let max = self.config.max_backoff_ms;

        // Exponential: base * 2^(attempt-1)
        let exp = base.saturating_mul(2_u64.saturating_pow(attempt.saturating_sub(1)));
        let exp = exp.min(max);

        // Add jitter: random value between 0 and 25% of exp
        let jitter = (rand::random::<u64>() % (exp / 4 + 1)).min(max);

        Duration::from_millis(exp + jitter)
    }

    // -----------------------------------------------------------------------
    // Health & metadata endpoints
    // -----------------------------------------------------------------------

    /// `GET /health` — Check API health status.
    pub async fn health(&self) -> Result<HealthResponse> {
        self.execute(Method::GET, "/health").await
    }

    /// `GET /version` — Get API version information.
    pub async fn version(&self) -> Result<VersionResponse> {
        self.execute(Method::GET, "/version").await
    }

    // -----------------------------------------------------------------------
    // Stats & events
    // -----------------------------------------------------------------------

    /// `GET /v1/stats` — Get platform-wide statistics.
    pub async fn get_stats(&self) -> Result<Stats> {
        self.execute(Method::GET, "/v1/stats").await
    }

    /// `GET /v1/events` — List recent contract events.
    pub async fn list_events(&self) -> Result<Vec<Event>> {
        self.execute(Method::GET, "/v1/events").await
    }

    // -----------------------------------------------------------------------
    // Asset endpoints
    // -----------------------------------------------------------------------

    /// `GET /v1/assets` — List all tokenized assets.
    pub async fn list_assets(&self, query: ListAssetsQuery) -> Result<Vec<Asset>> {
        let mut path = String::from("/v1/assets");
        let mut params = Vec::new();

        if let Some(ref t) = query.asset_type {
            params.push(format!("asset_type={}", urlencode(t)));
        }
        if let Some(a) = query.active {
            params.push(format!("active={}", a));
        }
        if let Some(o) = query.offset {
            params.push(format!("offset={}", o));
        }
        if let Some(l) = query.limit {
            params.push(format!("limit={}", l));
        }
        if let Some(ref f) = query.fields {
            params.push(format!("fields={}", urlencode(f)));
        }

        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }

        self.execute(Method::GET, &path).await
    }

    /// `GET /v1/assets` with field selection — Returns sparse assets.
    pub async fn list_assets_sparse(
        &self,
        query: ListAssetsQuery,
    ) -> Result<Vec<SparseAssetItem>> {
        let mut path = String::from("/v1/assets");
        let mut params = Vec::new();

        if let Some(ref t) = query.asset_type {
            params.push(format!("asset_type={}", urlencode(t)));
        }
        if let Some(a) = query.active {
            params.push(format!("active={}", a));
        }
        if let Some(o) = query.offset {
            params.push(format!("offset={}", o));
        }
        if let Some(l) = query.limit {
            params.push(format!("limit={}", l));
        }
        if let Some(ref f) = query.fields {
            params.push(format!("fields={}", urlencode(f)));
        }

        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }

        self.execute(Method::GET, &path).await
    }

    /// `GET /v1/assets/:id` — Get full asset detail.
    pub async fn get_asset(&self, id: u64, query: GetAssetQuery) -> Result<Asset> {
        let path = match query.fields {
            Some(ref f) => format!("/v1/assets/{}?fields={}", id, urlencode(f)),
            None => format!("/v1/assets/{}", id),
        };
        self.execute(Method::GET, &path).await
    }

    /// `GET /v1/assets/:id` with field selection — Returns sparse asset.
    pub async fn get_asset_sparse(
        &self,
        id: u64,
        query: GetAssetQuery,
    ) -> Result<SparseAsset> {
        let path = match query.fields {
            Some(ref f) => format!("/v1/assets/{}?fields={}", id, urlencode(f)),
            None => format!("/v1/assets/{}", id),
        };
        self.execute(Method::GET, &path).await
    }

    /// `GET /v1/assets/:id/events` — Get events for a specific asset.
    pub async fn get_asset_events(
        &self,
        id: u64,
        query: AssetEventsQuery,
    ) -> Result<AssetEventsResponse> {
        let mut path = format!("/v1/assets/{}/events", id);
        if let Some(d) = query.include_diagnostics {
            path.push_str(&format!("?include_diagnostics={}", d));
        }
        self.execute(Method::GET, &path).await
    }

    /// `GET /v1/assets/:id/metrics/analytics` — Get analytics for an asset.
    pub async fn get_asset_analytics(
        &self,
        id: u64,
        query: AssetAnalyticsQuery,
    ) -> Result<AssetAnalyticsResponse> {
        let mut path = format!("/v1/assets/{}/metrics/analytics", id);
        let mut params = Vec::new();

        if let Some(ref w) = query.window {
            params.push(format!("window={}", urlencode(w)));
        }
        if let Some(ref wt) = query.window_type {
            params.push(format!("window_type={}", urlencode(wt)));
        }
        if let Some(l) = query.limit {
            params.push(format!("limit={}", l));
        }

        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }

        self.execute(Method::GET, &path).await
    }

    /// `GET /v1/assets/:id/holders` — List holders of an asset.
    pub async fn list_holders(
        &self,
        asset_id: u64,
        query: ListHoldersQuery,
    ) -> Result<Vec<Holder>> {
        let mut path = format!("/v1/assets/{}/holders", asset_id);
        let mut params = Vec::new();

        if let Some(o) = query.offset {
            params.push(format!("offset={}", o));
        }
        if let Some(l) = query.limit {
            params.push(format!("limit={}", l));
        }

        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }

        self.execute(Method::GET, &path).await
    }

    /// `GET /v1/assets/:id/compliance` — Get compliance summary for an asset.
    pub async fn get_compliance_summary(&self, asset_id: u64) -> Result<ComplianceSummary> {
        self.execute(Method::GET, &format!("/v1/assets/{}/compliance", asset_id))
            .await
    }

    /// `GET /v1/assets/:id/dividends` — List dividend distributions for an asset.
    pub async fn list_dividends(&self, asset_id: u64) -> Result<Vec<Distribution>> {
        self.execute(Method::GET, &format!("/v1/assets/{}/dividends", asset_id))
            .await
    }

    /// `GET /v1/assets/:id/distributions/:did` — Get a specific distribution.
    pub async fn get_distribution(
        &self,
        asset_id: u64,
        distribution_id: u64,
    ) -> Result<Distribution> {
        self.execute(
            Method::GET,
            &format!("/v1/assets/{}/distributions/{}", asset_id, distribution_id),
        )
        .await
    }

    /// `GET /v1/assets/:id/export` — Export asset data as CSV or Parquet.
    ///
    /// Returns the raw bytes of the exported file.
    pub async fn export_asset(
        &self,
        asset_id: u64,
        query: ExportQuery,
    ) -> Result<ExportResponse> {
        let mut path = format!("/v1/assets/{}/export", asset_id);
        let mut params = Vec::new();

        if let Some(ref f) = query.format {
            params.push(format!("format={}", urlencode(f)));
        }
        if let Some(l) = query.ledger {
            params.push(format!("ledger={}", l));
        }

        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }

        let bytes = self.execute_raw(Method::GET, &path).await?;
        let content_disposition = self
            .request(Method::GET, &path)?
            .send()
            .await?
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        Ok(ExportResponse {
            bytes,
            content_disposition,
        })
    }

    // -----------------------------------------------------------------------
    // Holder endpoints
    // -----------------------------------------------------------------------

    /// `GET /v1/holders/:address` — Get all holdings for an address.
    pub async fn get_holder(&self, address: &str) -> Result<Vec<AddressHolding>> {
        self.execute(Method::GET, &format!("/v1/holders/{}", urlencode(address)))
            .await
    }

    /// `GET /v1/holders/:address/compliance` — Get compliance for an address.
    pub async fn get_holder_compliance(
        &self,
        address: &str,
    ) -> Result<Vec<AddressCompliance>> {
        self.execute(
            Method::GET,
            &format!("/v1/holders/{}/compliance", urlencode(address)),
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Compliance endpoints
    // -----------------------------------------------------------------------

    /// `GET /v1/compliance/:address` — Get compliance status for an address.
    pub async fn get_address_compliance(
        &self,
        address: &str,
    ) -> Result<Vec<AddressCompliance>> {
        self.execute(
            Method::GET,
            &format!("/v1/compliance/{}", urlencode(address)),
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Security endpoints
    // -----------------------------------------------------------------------

    /// `GET /v1/security/anomalies` — List security anomalies.
    pub async fn list_anomalies(&self, query: AnomalyQuery) -> Result<AnomaliesResponse> {
        let mut path = String::from("/v1/security/anomalies");
        let mut params = Vec::new();

        if let Some(ref a) = query.account {
            params.push(format!("account={}", urlencode(a)));
        }
        if let Some(z) = query.min_z {
            params.push(format!("min_z={}", z));
        }

        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }

        self.execute(Method::GET, &path).await
    }

    // -----------------------------------------------------------------------
    // Audit endpoints
    // -----------------------------------------------------------------------

    /// `GET /v1/audit/verify` — Verify audit log integrity.
    pub async fn verify_audit(&self, query: VerifyQuery) -> Result<AuditVerificationResult> {
        let mut path = String::from("/v1/audit/verify");
        if let Some(seq) = query.entry_seq {
            path.push_str(&format!("?entry_seq={}", seq));
        }
        self.execute(Method::GET, &path).await
    }

    /// `GET /v1/audit/entries` — List audit entries with pagination.
    pub async fn list_audit_entries(
        &self,
        query: ListAuditEntriesQuery,
    ) -> Result<PaginatedEntriesResponse> {
        let mut path = String::from("/v1/audit/entries");
        let mut params = Vec::new();

        if let Some(o) = query.offset {
            params.push(format!("offset={}", o));
        }
        if let Some(l) = query.limit {
            params.push(format!("limit={}", l));
        }

        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }

        self.execute(Method::GET, &path).await
    }

    /// `POST /v1/audit/entries` — Create a new audit entry.
    pub async fn create_audit_entry(
        &self,
        request: &CreateAuditEntryRequest,
    ) -> Result<AuditEntry> {
        let req = self.request(Method::POST, "/v1/audit/entries")?.json(request);
        let response = req.send().await?;
        let status = response.status();

        if status.is_success() {
            let bytes = response.bytes().await?;
            serde_json::from_slice(&bytes).map_err(|e| TesseraError::Serialization {
                message: e.to_string(),
                body: String::from_utf8_lossy(&bytes).to_string(),
            })
        } else {
            self.handle_error_response(response).await
        }
    }

    /// `GET /v1/audit/entries/:sequence` — Get a specific audit entry with proof.
    pub async fn get_audit_entry(&self, sequence: u64) -> Result<EntryDetailResponse> {
        self.execute(Method::GET, &format!("/v1/audit/entries/{}", sequence))
            .await
    }

    /// `GET /v1/audit/anchors` — List all anchor records.
    pub async fn list_audit_anchors(&self) -> Result<Vec<AnchorRecord>> {
        self.execute(Method::GET, "/v1/audit/anchors").await
    }

    /// `POST /v1/audit/anchor` — Publish a new anchor.
    pub async fn publish_anchor(
        &self,
        request: &PublishAnchorRequest,
    ) -> Result<AnchorRecord> {
        let req = self.request(Method::POST, "/v1/audit/anchor")?.json(request);
        let response = req.send().await?;
        let status = response.status();

        if status.is_success() {
            let bytes = response.bytes().await?;
            serde_json::from_slice(&bytes).map_err(|e| TesseraError::Serialization {
                message: e.to_string(),
                body: String::from_utf8_lossy(&bytes).to_string(),
            })
        } else {
            self.handle_error_response(response).await
        }
    }

    // -----------------------------------------------------------------------
    // Helper methods
    // -----------------------------------------------------------------------

    /// Handle an error response from a POST request.
    async fn handle_error_response<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T> {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        let body_str = String::from_utf8_lossy(&body).to_string();
        let error_detail: Option<ApiErrorBody> = serde_json::from_slice(&body).ok();

        match status {
            StatusCode::NOT_FOUND => Err(TesseraError::NotFound {
                resource: "audit entry".to_string(),
            }),
            StatusCode::BAD_REQUEST => Err(TesseraError::BadRequest {
                message: error_detail
                    .map(|d| d.message)
                    .unwrap_or_else(|| "Bad request".to_string()),
            }),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(TesseraError::Auth {
                message: error_detail
                    .map(|d| d.message)
                    .unwrap_or_else(|| "Authentication failed".to_string()),
            }),
            _ => {
                let detail = error_detail.unwrap_or(ApiErrorBody {
                    error: format!("http_{}", status.as_u16()),
                    message: body_str.clone(),
                });
                Err(TesseraError::Api(ApiErrorDetail {
                    status: status.as_u16(),
                    error: detail.error,
                    message: detail.message,
                }))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Additional response types
// ---------------------------------------------------------------------------

/// Health check response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub snapshot_age_seconds: i64,
    pub max_age_seconds: i64,
}

/// Version information response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionResponse {
    pub version: String,
    pub release: String,
}

/// Asset events response (includes optional diagnostics).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetEventsResponse {
    pub asset_id: u64,
    #[serde(default)]
    pub diagnostics: Vec<DiagnosticEventRecord>,
}

/// Export response with raw bytes and metadata.
#[derive(Debug, Clone)]
pub struct ExportResponse {
    /// Raw file bytes (CSV or Parquet).
    pub bytes: Bytes,
    /// Content-Disposition header value (filename).
    pub content_disposition: Option<String>,
}

impl ExportResponse {
    /// Save the exported data to a file.
    pub fn save_to(&self, path: &str) -> std::io::Result<()> {
        std::fs::write(path, &self.bytes)
    }

    /// Get the filename from the Content-Disposition header.
    pub fn filename(&self) -> Option<String> {
        self.content_disposition.as_ref().and_then(|cd| {
            cd.split("filename=")
                .nth(1)
                .map(|s| s.trim_matches('"').to_string())
        })
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// URL-encode a string.
fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}


