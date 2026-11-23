//! HashiCorp Vault dynamic secrets for PostgreSQL and Redis with Kubernetes
//! ServiceAccount JWT authentication and background lease renewal (issue #423).
//!
//! * **Startup Authentication (`POST /v1/auth/kubernetes/login`)**: reads the
//!   pod's projected Kubernetes ServiceAccount JWT from disk (default
//!   `/var/run/secrets/kubernetes.io/serviceaccount/token`) and exchanges it
//!   for a short-lived Vault client token.
//! * **Dynamic Credentials (`GET /v1/database/creds/<role>`)**: fetches 1h-TTL
//!   (`3600s`) least-privilege PostgreSQL and Redis AUTH credentials in memory.
//!   Credentials never touch Kubernetes `Secret` objects or `etcd`.
//! * **Background Lease Renewal (`PUT /v1/sys/leases/renew`)**: renews both
//!   PostgreSQL and Redis leases before expiry.
//! * **Expiry & Network-Partition Recovery**: if lease renewal fails or a lease
//!   expires while Vault is unreachable, the active `sqlx::PgPool` is explicitly
//!   closed (`pool.close().await`), the expired Redis client is dropped, and the
//!   manager re-authenticates with Vault using the Kubernetes ServiceAccount JWT
//!   to provision a fresh `sqlx::PgPool` and `redis::Client`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use tokio::sync::{watch, Mutex};

/// Default 1-hour (`3600s`) TTL for dynamic PostgreSQL and Redis credentials.
pub const DEFAULT_DYNAMIC_SECRET_TTL: Duration = Duration::from_secs(3600);
/// Default Kubernetes ServiceAccount JWT path mounted by the kubelet.
pub const DEFAULT_K8S_JWT_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";
/// Default Vault Kubernetes auth mount path.
pub const DEFAULT_K8S_AUTH_MOUNT: &str = "kubernetes";
/// Default Vault Kubernetes auth role for the API workload.
pub const DEFAULT_K8S_AUTH_ROLE: &str = "tessera-api";
/// Default Vault database secrets engine mount path.
pub const DEFAULT_DATABASE_MOUNT: &str = "database";
/// Default Vault role for 1h-TTL least-privilege PostgreSQL credentials.
pub const DEFAULT_POSTGRES_ROLE: &str = "tessera-api-postgres";
/// Default Vault role for 1h-TTL dynamic Redis AUTH tokens.
pub const DEFAULT_REDIS_ROLE: &str = "tessera-api-redis";
/// Default fraction of lease duration elapsed before triggering `PUT /v1/sys/leases/renew`.
pub const DEFAULT_RENEW_THRESHOLD_RATIO: f64 = 0.75;
/// Default background lease check cadence.
pub const DEFAULT_RENEW_POLL_INTERVAL: Duration = Duration::from_secs(30);

static MANAGER: OnceLock<Arc<VaultSecretManager>> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("cannot read Kubernetes ServiceAccount JWT at {path:?}: {source}")]
    JwtIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Kubernetes ServiceAccount JWT at {path:?} is empty")]
    EmptyJwt { path: PathBuf },
    #[error("Vault HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Vault API returned HTTP {status} on {endpoint}: {body}")]
    ApiStatus {
        endpoint: String,
        status: StatusCode,
        body: String,
    },
    #[error("Vault response missing client_token during Kubernetes login")]
    MissingClientToken,
    #[error("failed to construct Redis client from dynamic credentials: {0}")]
    RedisConfig(String),
    #[error("Vault lease expired and renewal could not complete before deadline")]
    LeaseExpired,
}

#[derive(Clone, Debug)]
pub struct VaultConfig {
    pub vault_addr: String,
    pub k8s_mount: String,
    pub k8s_role: String,
    pub jwt_path: PathBuf,
    pub database_mount: String,
    pub postgres_role: String,
    pub redis_mount: String,
    pub redis_role: String,
    pub pg_host: String,
    pub pg_port: u16,
    pub pg_database: String,
    pub redis_host: String,
    pub redis_port: u16,
    pub default_ttl: Duration,
    pub renew_threshold_ratio: f64,
    pub poll_interval: Duration,
}

impl VaultConfig {
    /// Load Vault configuration from environment variables. Returns `None` when
    /// `VAULT_ADDR` is not set.
    pub fn from_env() -> Option<Self> {
        let vault_addr = std::env::var("VAULT_ADDR").ok()?;
        if vault_addr.trim().is_empty() {
            return None;
        }
        Some(Self {
            vault_addr: vault_addr.trim_end_matches('/').to_string(),
            k8s_mount: std::env::var("VAULT_K8S_MOUNT")
                .unwrap_or_else(|_| DEFAULT_K8S_AUTH_MOUNT.to_string()),
            k8s_role: std::env::var("VAULT_K8S_ROLE")
                .unwrap_or_else(|_| DEFAULT_K8S_AUTH_ROLE.to_string()),
            jwt_path: PathBuf::from(
                std::env::var("VAULT_K8S_JWT_PATH")
                    .unwrap_or_else(|_| DEFAULT_K8S_JWT_PATH.to_string()),
            ),
            database_mount: std::env::var("VAULT_DB_MOUNT")
                .unwrap_or_else(|_| DEFAULT_DATABASE_MOUNT.to_string()),
            postgres_role: std::env::var("VAULT_PG_ROLE")
                .unwrap_or_else(|_| DEFAULT_POSTGRES_ROLE.to_string()),
            redis_mount: std::env::var("VAULT_REDIS_MOUNT")
                .unwrap_or_else(|_| DEFAULT_DATABASE_MOUNT.to_string()),
            redis_role: std::env::var("VAULT_REDIS_ROLE")
                .unwrap_or_else(|_| DEFAULT_REDIS_ROLE.to_string()),
            pg_host: std::env::var("DB_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            pg_port: std::env::var("DB_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(5432),
            pg_database: std::env::var("DB_NAME").unwrap_or_else(|_| "tessera".to_string()),
            redis_host: std::env::var("REDIS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            redis_port: std::env::var("REDIS_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(6379),
            default_ttl: DEFAULT_DYNAMIC_SECRET_TTL,
            renew_threshold_ratio: DEFAULT_RENEW_THRESHOLD_RATIO,
            poll_interval: DEFAULT_RENEW_POLL_INTERVAL,
        })
    }
}

/// Active lease for a dynamic PostgreSQL user or dynamic Redis AUTH token.
#[derive(Clone, Debug)]
pub struct DynamicSecretLease {
    pub username: String,
    pub password: String,
    pub lease_id: String,
    pub lease_duration: Duration,
    pub renewable: bool,
    pub obtained_at: Instant,
}

impl DynamicSecretLease {
    /// Returns `true` when the lease has reached its renewal window
    /// (e.g. 75% of `lease_duration` elapsed).
    pub fn should_renew(&self, now: Instant, threshold_ratio: f64) -> bool {
        let elapsed = now.saturating_duration_since(self.obtained_at);
        elapsed.as_secs_f64() >= self.lease_duration.as_secs_f64() * threshold_ratio
    }

    /// Returns `true` when the lease TTL has fully elapsed.
    pub fn is_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.obtained_at) >= self.lease_duration
    }
}

#[derive(Serialize)]
struct K8sLoginRequest<'a> {
    role: &'a str,
    jwt: &'a str,
}

#[derive(Deserialize)]
struct VaultAuthBlock {
    client_token: String,
}

#[derive(Deserialize)]
struct K8sLoginResponse {
    auth: Option<VaultAuthBlock>,
}

#[derive(Deserialize)]
struct DynamicCredsData {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct DynamicCredsResponse {
    lease_id: String,
    lease_duration: u64,
    renewable: bool,
    data: DynamicCredsData,
}

#[derive(Serialize)]
struct LeaseRenewRequest<'a> {
    lease_id: &'a str,
    increment: u64,
}

#[derive(Deserialize)]
struct LeaseRenewResponse {
    lease_id: String,
    lease_duration: u64,
    renewable: bool,
}

/// Low-level HTTP client for HashiCorp Vault's Kubernetes auth, database
/// secrets engine, and `PUT /v1/sys/leases/renew` APIs.
#[derive(Clone)]
pub struct VaultClient {
    http: Client,
    base_url: String,
}

impl VaultClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let http = crate::security::mtls::http_builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    /// Authenticate with Vault using the Kubernetes ServiceAccount JWT
    /// (`POST /v1/auth/{mount}/login`).
    pub async fn authenticate_k8s(
        &self,
        mount: &str,
        role: &str,
        jwt: &str,
    ) -> Result<String, VaultError> {
        let endpoint = format!("{}/v1/auth/{}/login", self.base_url, mount);
        let resp = self
            .http
            .post(&endpoint)
            .json(&K8sLoginRequest { role, jwt })
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(VaultError::ApiStatus {
                endpoint,
                status,
                body,
            });
        }

        let parsed: K8sLoginResponse = resp.json().await?;
        let token = parsed
            .auth
            .map(|a| a.client_token)
            .filter(|t| !t.is_empty())
            .ok_or(VaultError::MissingClientToken)?;
        Ok(token)
    }

    /// Fetch short-lived dynamic credentials from `GET /v1/{mount}/creds/{role}`.
    pub async fn fetch_dynamic_credentials(
        &self,
        token: &str,
        mount: &str,
        role: &str,
        now: Instant,
    ) -> Result<DynamicSecretLease, VaultError> {
        let endpoint = format!("{}/v1/{}/creds/{}", self.base_url, mount, role);
        let resp = self
            .http
            .get(&endpoint)
            .header("X-Vault-Token", token)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(VaultError::ApiStatus {
                endpoint,
                status,
                body,
            });
        }

        let parsed: DynamicCredsResponse = resp.json().await?;
        let ttl_secs = if parsed.lease_duration > 0 {
            parsed.lease_duration
        } else {
            DEFAULT_DYNAMIC_SECRET_TTL.as_secs()
        };

        Ok(DynamicSecretLease {
            username: parsed.data.username,
            password: parsed.data.password,
            lease_id: parsed.lease_id,
            lease_duration: Duration::from_secs(ttl_secs),
            renewable: parsed.renewable,
            obtained_at: now,
        })
    }

    /// Renew an active lease before expiry via `PUT /v1/sys/leases/renew`.
    pub async fn renew_lease(
        &self,
        token: &str,
        lease_id: &str,
        increment_secs: u64,
    ) -> Result<Duration, VaultError> {
        let endpoint = format!("{}/v1/sys/leases/renew", self.base_url);
        let resp = self
            .http
            .put(&endpoint)
            .header("X-Vault-Token", token)
            .json(&LeaseRenewRequest {
                lease_id,
                increment: increment_secs,
            })
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(VaultError::ApiStatus {
                endpoint,
                status,
                body,
            });
        }

        let parsed: LeaseRenewResponse = resp.json().await?;
        let ttl_secs = if parsed.lease_duration > 0 {
            parsed.lease_duration
        } else {
            increment_secs
        };
        Ok(Duration::from_secs(ttl_secs))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseMaintenanceOutcome {
    /// Leases are healthy and not yet due for renewal.
    NoActionNeeded,
    /// Active leases were renewed in place via `PUT /v1/sys/leases/renew`.
    RenewedInPlace,
    /// Renewal failed and lease expired; SQLx pool was closed and awaits Vault recovery.
    PoolClosedOnExpiry,
    /// Vault recovered after expiry/failure; manager re-authenticated via K8s JWT and rotated pools.
    ReauthenticatedAndRotated,
}

#[derive(Default)]
struct LeaseState {
    vault_token: Option<String>,
    postgres_lease: Option<DynamicSecretLease>,
    redis_lease: Option<DynamicSecretLease>,
}

/// Coordinates Kubernetes ServiceAccount JWT authentication, 1h-TTL dynamic
/// PostgreSQL and Redis credentials, `PUT /v1/sys/leases/renew` background
/// renewal, and SQLx pool teardown + re-authentication on lease expiry.
pub struct VaultSecretManager {
    config: VaultConfig,
    client: VaultClient,
    state: Mutex<LeaseState>,
    pg_pool: ArcSwap<Option<PgPool>>,
    redis_client: ArcSwap<Option<redis::Client>>,
    pub renewals_succeeded: AtomicU64,
    pub renewals_failed: AtomicU64,
    pub reauthentications: AtomicU64,
    pub pool_closures: AtomicU64,
}

impl VaultSecretManager {
    pub fn new(config: VaultConfig) -> Self {
        let client = VaultClient::new(config.vault_addr.clone());
        Self {
            config,
            client,
            state: Mutex::new(LeaseState::default()),
            pg_pool: ArcSwap::from_pointee(None),
            redis_client: ArcSwap::from_pointee(None),
            renewals_succeeded: AtomicU64::new(0),
            renewals_failed: AtomicU64::new(0),
            reauthentications: AtomicU64::new(0),
            pool_closures: AtomicU64::new(0),
        }
    }

    pub fn config(&self) -> &VaultConfig {
        &self.config
    }

    /// Returns the currently active `sqlx::PgPool` backed by dynamic Vault credentials.
    pub fn pg_pool(&self) -> Option<PgPool> {
        self.pg_pool.load_full().as_ref().clone()
    }

    /// Returns the currently active `redis::Client` configured with a dynamic Redis AUTH token.
    pub fn redis_client(&self) -> Option<redis::Client> {
        self.redis_client.load_full().as_ref().clone()
    }

    /// Returns a snapshot of the active PostgreSQL and Redis leases.
    pub async fn active_leases(&self) -> (Option<DynamicSecretLease>, Option<DynamicSecretLease>) {
        let guard = self.state.lock().await;
        (guard.postgres_lease.clone(), guard.redis_lease.clone())
    }

    /// Startup sequence: read the Kubernetes ServiceAccount JWT from disk,
    /// authenticate with Vault (`POST /v1/auth/kubernetes/login`), fetch
    /// dynamic 1h-TTL Postgres and Redis credentials, and initialize the
    /// SQLx connection pool and Redis client.
    pub async fn initialize_on_startup(&self) -> Result<(), VaultError> {
        self.authenticate_and_provision(Instant::now()).await
    }

    async fn read_k8s_jwt(&self) -> Result<String, VaultError> {
        let raw = tokio::fs::read_to_string(&self.config.jwt_path)
            .await
            .map_err(|source| VaultError::JwtIo {
                path: self.config.jwt_path.clone(),
                source,
            })?;
        let trimmed = raw.trim().to_string();
        if trimmed.is_empty() {
            return Err(VaultError::EmptyJwt {
                path: self.config.jwt_path.clone(),
            });
        }
        Ok(trimmed)
    }

    fn build_pg_pool(&self, lease: &DynamicSecretLease) -> PgPool {
        let opts = PgConnectOptions::new()
            .host(&self.config.pg_host)
            .port(self.config.pg_port)
            .database(&self.config.pg_database)
            .username(&lease.username)
            .password(&lease.password);
        let opts = crate::security::mtls::apply_pg_tls(opts);
        PgPoolOptions::new()
            .max_connections(10)
            .connect_lazy_with(opts)
    }

    fn build_redis_client(&self, lease: &DynamicSecretLease) -> Result<redis::Client, VaultError> {
        let redis_url = format!(
            "redis://{}:{}@{}:{}/",
            lease.username, lease.password, self.config.redis_host, self.config.redis_port
        );
        redis::Client::open(redis_url).map_err(|e| VaultError::RedisConfig(e.to_string()))
    }

    /// Closes the active `sqlx::PgPool` (`pool.close().await`) and clears both
    /// the PostgreSQL pool and Redis client handles so expired credentials are
    /// never used for new queries.
    pub async fn close_active_pools(&self) {
        if let Some(existing_pool) = self.pg_pool.swap(Arc::new(None)).as_ref().clone() {
            existing_pool.close().await;
            self.pool_closures.fetch_add(1, Ordering::SeqCst);
            metrics::counter!("vault_sqlx_pool_closures_total").increment(1);
            tracing::warn!("closed active sqlx::PgPool due to Vault lease expiry or renewal failure");
        }
        self.redis_client.store(Arc::new(None));
    }

    async fn authenticate_and_provision(&self, now: Instant) -> Result<(), VaultError> {
        let jwt = self.read_k8s_jwt().await?;
        let token = self
            .client
            .authenticate_k8s(&self.config.k8s_mount, &self.config.k8s_role, &jwt)
            .await?;

        let pg_lease = self
            .client
            .fetch_dynamic_credentials(
                &token,
                &self.config.database_mount,
                &self.config.postgres_role,
                now,
            )
            .await?;

        let redis_lease = self
            .client
            .fetch_dynamic_credentials(
                &token,
                &self.config.redis_mount,
                &self.config.redis_role,
                now,
            )
            .await?;

        let new_pg_pool = self.build_pg_pool(&pg_lease);
        let new_redis_client = self.build_redis_client(&redis_lease)?;

        // Close any previous PgPool before installing the new one
        if let Some(old_pool) = self
            .pg_pool
            .swap(Arc::new(Some(new_pg_pool)))
            .as_ref()
            .clone()
        {
            old_pool.close().await;
        }
        self.redis_client.store(Arc::new(Some(new_redis_client)));

        let mut guard = self.state.lock().await;
        guard.vault_token = Some(token);
        guard.postgres_lease = Some(pg_lease);
        guard.redis_lease = Some(redis_lease);

        metrics::gauge!("vault_dynamic_secrets_active").set(1.0);
        Ok(())
    }

    /// Single maintenance cycle for the background renewal task:
    /// - If leases have not yet reached their renewal threshold, does nothing.
    /// - If leases are due for renewal and not yet expired, calls `PUT /v1/sys/leases/renew`.
    /// - If renewal fails or a lease has expired:
    ///   1. Immediately closes the active `sqlx::PgPool` (`pool.close().await`) and drops the Redis client.
    ///   2. Attempts to re-authenticate with Vault via the Kubernetes ServiceAccount JWT
    ///      and provision fresh dynamic Postgres and Redis credentials.
    pub async fn maintain_leases_once(
        &self,
        now: Instant,
    ) -> Result<LeaseMaintenanceOutcome, VaultError> {
        let (token_opt, pg_opt, redis_opt) = {
            let guard = self.state.lock().await;
            (
                guard.vault_token.clone(),
                guard.postgres_lease.clone(),
                guard.redis_lease.clone(),
            )
        };

        let (Some(token), Some(pg_lease), Some(redis_lease)) = (token_opt, pg_opt, redis_opt)
        else {
            // No active leases (or previously closed on expiry): attempt full K8s JWT re-authentication
            self.authenticate_and_provision(now).await?;
            self.reauthentications.fetch_add(1, Ordering::SeqCst);
            return Ok(LeaseMaintenanceOutcome::ReauthenticatedAndRotated);
        };

        let expired = pg_lease.is_expired(now) || redis_lease.is_expired(now);
        if expired {
            tracing::warn!(
                pg_lease_id = %pg_lease.lease_id,
                redis_lease_id = %redis_lease.lease_id,
                "Vault dynamic secret lease expired; closing SQLx pool and re-authenticating"
            );
            self.close_active_pools().await;
            {
                let mut guard = self.state.lock().await;
                guard.postgres_lease = None;
                guard.redis_lease = None;
            }
            match self.authenticate_and_provision(now).await {
                Ok(()) => {
                    self.reauthentications.fetch_add(1, Ordering::SeqCst);
                    return Ok(LeaseMaintenanceOutcome::ReauthenticatedAndRotated);
                }
                Err(err) => {
                    tracing::error!(error = %err, "Vault unreachable after lease expiry; SQLx pool remains closed");
                    return Ok(LeaseMaintenanceOutcome::PoolClosedOnExpiry);
                }
            }
        }

        let ratio = self.config.renew_threshold_ratio;
        let pg_due = pg_lease.should_renew(now, ratio);
        let redis_due = redis_lease.should_renew(now, ratio);

        if !pg_due && !redis_due {
            return Ok(LeaseMaintenanceOutcome::NoActionNeeded);
        }

        let increment_secs = self.config.default_ttl.as_secs();
        let pg_renew = if pg_due && pg_lease.renewable {
            self.client
                .renew_lease(&token, &pg_lease.lease_id, increment_secs)
                .await
        } else {
            Ok(pg_lease.lease_duration)
        };

        let redis_renew = if redis_due && redis_lease.renewable {
            self.client
                .renew_lease(&token, &redis_lease.lease_id, increment_secs)
                .await
        } else {
            Ok(redis_lease.lease_duration)
        };

        match (pg_renew, redis_renew) {
            (Ok(new_pg_ttl), Ok(new_redis_ttl)) => {
                let mut guard = self.state.lock().await;
                if let Some(pg) = guard.postgres_lease.as_mut() {
                    pg.lease_duration = new_pg_ttl;
                    pg.obtained_at = now;
                }
                if let Some(rd) = guard.redis_lease.as_mut() {
                    rd.lease_duration = new_redis_ttl;
                    rd.obtained_at = now;
                }
                self.renewals_succeeded.fetch_add(1, Ordering::SeqCst);
                metrics::counter!("vault_lease_renewals_total", "status" => "success").increment(1);
                tracing::info!(
                    pg_lease_id = %pg_lease.lease_id,
                    redis_lease_id = %redis_lease.lease_id,
                    "renewed Vault Postgres and Redis dynamic secret leases via PUT /v1/sys/leases/renew"
                );
                Ok(LeaseMaintenanceOutcome::RenewedInPlace)
            }
            (Err(err), _) | (_, Err(err)) => {
                self.renewals_failed.fetch_add(1, Ordering::SeqCst);
                metrics::counter!("vault_lease_renewals_total", "status" => "error").increment(1);
                tracing::warn!(
                    error = %err,
                    "PUT /v1/sys/leases/renew failed; closing SQLx pool and attempting K8s JWT re-authentication"
                );
                self.close_active_pools().await;
                {
                    let mut guard = self.state.lock().await;
                    guard.postgres_lease = None;
                    guard.redis_lease = None;
                }
                match self.authenticate_and_provision(now).await {
                    Ok(()) => {
                        self.reauthentications.fetch_add(1, Ordering::SeqCst);
                        Ok(LeaseMaintenanceOutcome::ReauthenticatedAndRotated)
                    }
                    Err(_reauth_err) => Ok(LeaseMaintenanceOutcome::PoolClosedOnExpiry),
                }
            }
        }
    }

    /// Background Tokio loop that periodically renews active Postgres and Redis
    /// leases via `PUT /v1/sys/leases/renew` and recovers from Vault outages.
    pub async fn run_renewal_loop(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(self.config.poll_interval) => {
                    if let Err(error) = self.maintain_leases_once(Instant::now()).await {
                        tracing::warn!(%error, "Vault lease maintenance cycle encountered error");
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow_and_update() {
                        tracing::info!("stopping Vault dynamic secret lease renewal task");
                        self.close_active_pools().await;
                        break;
                    }
                }
            }
        }
    }
}

/// Initialize the process-wide [`VaultSecretManager`] from environment variables
/// when `VAULT_ADDR` is configured.
pub fn init_from_env() -> Option<Arc<VaultSecretManager>> {
    let config = VaultConfig::from_env()?;
    let manager = Arc::new(VaultSecretManager::new(config));
    let _ = MANAGER.set(Arc::clone(&manager));
    MANAGER.get().cloned()
}

pub fn manager() -> Option<Arc<VaultSecretManager>> {
    MANAGER.get().cloned()
}

/// Spawn the background lease renewal task (`PUT /v1/sys/leases/renew`).
pub fn spawn_lease_renewal(
    manager: Arc<VaultSecretManager>,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        manager.run_renewal_loop(shutdown).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path, State},
        http::StatusCode as AxumStatus,
        routing::{get, post, put},
        Json, Router,
    };
    use std::sync::atomic::AtomicBool;

    struct MockVaultState {
        blocked: AtomicBool,
        login_calls: AtomicU64,
        pg_creds_calls: AtomicU64,
        redis_creds_calls: AtomicU64,
        renew_calls: AtomicU64,
        lease_ttl_secs: u64,
    }

    impl MockVaultState {
        fn new(lease_ttl_secs: u64) -> Arc<Self> {
            Arc::new(Self {
                blocked: AtomicBool::new(false),
                login_calls: AtomicU64::new(0),
                pg_creds_calls: AtomicU64::new(0),
                redis_creds_calls: AtomicU64::new(0),
                renew_calls: AtomicU64::new(0),
                lease_ttl_secs,
            })
        }
    }

    async fn mock_k8s_login(
        State(state): State<Arc<MockVaultState>>,
        Path(_mount): Path<String>,
        Json(payload): Json<serde_json::Value>,
    ) -> (AxumStatus, Json<serde_json::Value>) {
        if state.blocked.load(Ordering::SeqCst) {
            return (
                AxumStatus::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "errors": ["vault sealed or network blocked"] })),
            );
        }
        let jwt = payload
            .get("jwt")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if jwt.is_empty() {
            return (
                AxumStatus::FORBIDDEN,
                Json(serde_json::json!({ "errors": ["missing service account jwt"] })),
            );
        }
        let gen = state.login_calls.fetch_add(1, Ordering::SeqCst) + 1;
        (
            AxumStatus::OK,
            Json(serde_json::json!({
                "auth": {
                    "client_token": format!("s.vault-k8s-token-v{gen}"),
                    "lease_duration": state.lease_ttl_secs,
                    "renewable": true
                }
            })),
        )
    }

    async fn mock_database_creds(
        State(state): State<Arc<MockVaultState>>,
        Path((_mount, role)): Path<(String, String)>,
    ) -> (AxumStatus, Json<serde_json::Value>) {
        if state.blocked.load(Ordering::SeqCst) {
            return (
                AxumStatus::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "errors": ["vault blocked mid-flight"] })),
            );
        }

        if role.contains("postgres") {
            let gen = state.pg_creds_calls.fetch_add(1, Ordering::SeqCst) + 1;
            (
                AxumStatus::OK,
                Json(serde_json::json!({
                    "lease_id": format!("database/creds/{role}/pg-lease-v{gen}"),
                    "lease_duration": state.lease_ttl_secs,
                    "renewable": true,
                    "data": {
                        "username": format!("v_k8s_pg_user_v{gen}"),
                        "password": format!("pg_secret_pass_v{gen}")
                    }
                })),
            )
        } else {
            let gen = state.redis_creds_calls.fetch_add(1, Ordering::SeqCst) + 1;
            (
                AxumStatus::OK,
                Json(serde_json::json!({
                    "lease_id": format!("database/creds/{role}/redis-lease-v{gen}"),
                    "lease_duration": state.lease_ttl_secs,
                    "renewable": true,
                    "data": {
                        "username": format!("v_k8s_redis_user_v{gen}"),
                        "password": format!("redis_auth_token_v{gen}")
                    }
                })),
            )
        }
    }

    async fn mock_renew_lease(
        State(state): State<Arc<MockVaultState>>,
        Json(payload): Json<serde_json::Value>,
    ) -> (AxumStatus, Json<serde_json::Value>) {
        if state.blocked.load(Ordering::SeqCst) {
            return (
                AxumStatus::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "errors": ["vault network partition during renewal"] })),
            );
        }
        state.renew_calls.fetch_add(1, Ordering::SeqCst);
        let lease_id = payload
            .get("lease_id")
            .and_then(|v| v.as_str())
            .unwrap_or("lease");
        (
            AxumStatus::OK,
            Json(serde_json::json!({
                "lease_id": lease_id,
                "lease_duration": state.lease_ttl_secs,
                "renewable": true
            })),
        )
    }

    async fn spawn_mock_vault_server(
        state: Arc<MockVaultState>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/v1/auth/:mount/login", post(mock_k8s_login))
            .route("/v1/:mount/creds/:role", get(mock_database_creds))
            .route("/v1/sys/leases/renew", put(mock_renew_lease))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock vault listener");
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), handle)
    }

    fn write_temp_jwt_file(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("tessera-vault-jwt-{name}-{}.token", std::process::id()));
        std::fs::write(&path, "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.k8s-sa-payload.sig")
            .expect("write temp k8s jwt");
        path
    }

    fn test_vault_config(vault_addr: String, jwt_path: PathBuf, ttl_secs: u64) -> VaultConfig {
        VaultConfig {
            vault_addr,
            k8s_mount: DEFAULT_K8S_AUTH_MOUNT.to_string(),
            k8s_role: DEFAULT_K8S_AUTH_ROLE.to_string(),
            jwt_path,
            database_mount: DEFAULT_DATABASE_MOUNT.to_string(),
            postgres_role: DEFAULT_POSTGRES_ROLE.to_string(),
            redis_mount: DEFAULT_DATABASE_MOUNT.to_string(),
            redis_role: DEFAULT_REDIS_ROLE.to_string(),
            pg_host: "127.0.0.1".to_string(),
            pg_port: 5432,
            pg_database: "tessera".to_string(),
            redis_host: "127.0.0.1".to_string(),
            redis_port: 6379,
            default_ttl: Duration::from_secs(ttl_secs),
            renew_threshold_ratio: 0.75,
            poll_interval: Duration::from_millis(50),
        }
    }

    #[tokio::test]
    async fn test_k8s_jwt_startup_fetches_postgres_and_redis_dynamic_secrets() {
        let state = MockVaultState::new(3600);
        let (vault_url, server_handle) = spawn_mock_vault_server(Arc::clone(&state)).await;
        let jwt_path = write_temp_jwt_file("startup");

        let manager = VaultSecretManager::new(test_vault_config(vault_url, jwt_path.clone(), 3600));
        manager
            .initialize_on_startup()
            .await
            .expect("startup K8s JWT login and dynamic credential fetch should succeed");

        assert_eq!(state.login_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.pg_creds_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.redis_creds_calls.load(Ordering::SeqCst), 1);

        let (pg_lease, redis_lease) = manager.active_leases().await;
        let pg_lease = pg_lease.expect("postgres lease present");
        let redis_lease = redis_lease.expect("redis lease present");

        assert_eq!(pg_lease.username, "v_k8s_pg_user_v1");
        assert_eq!(pg_lease.lease_duration, Duration::from_secs(3600));
        assert_eq!(redis_lease.username, "v_k8s_redis_user_v1");
        assert_eq!(redis_lease.password, "redis_auth_token_v1");
        assert_eq!(redis_lease.lease_duration, Duration::from_secs(3600));

        let pool = manager.pg_pool().expect("PgPool initialized");
        assert!(!pool.is_closed());
        assert!(manager.redis_client().is_some());

        let _ = std::fs::remove_file(jwt_path);
        server_handle.abort();
    }

    #[tokio::test]
    async fn test_background_lease_renewal_via_put_sys_leases_renew() {
        let state = MockVaultState::new(3600);
        let (vault_url, server_handle) = spawn_mock_vault_server(Arc::clone(&state)).await;
        let jwt_path = write_temp_jwt_file("renew");

        let manager = VaultSecretManager::new(test_vault_config(vault_url, jwt_path.clone(), 3600));
        manager.initialize_on_startup().await.unwrap();

        let (pg_before, _) = manager.active_leases().await;
        let t0 = pg_before.unwrap().obtained_at;

        // Before 75% of 1h TTL (e.g. 10 minutes elapsed) -> no renewal needed
        let outcome_early = manager
            .maintain_leases_once(t0 + Duration::from_secs(600))
            .await
            .unwrap();
        assert_eq!(outcome_early, LeaseMaintenanceOutcome::NoActionNeeded);
        assert_eq!(state.renew_calls.load(Ordering::SeqCst), 0);

        // At 80% of 1h TTL (2880s elapsed) -> renews both Postgres and Redis leases via PUT /v1/sys/leases/renew
        let outcome_renew = manager
            .maintain_leases_once(t0 + Duration::from_secs(2880))
            .await
            .unwrap();
        assert_eq!(outcome_renew, LeaseMaintenanceOutcome::RenewedInPlace);
        assert_eq!(state.renew_calls.load(Ordering::SeqCst), 2);
        assert_eq!(manager.renewals_succeeded.load(Ordering::SeqCst), 1);

        let _ = std::fs::remove_file(jwt_path);
        server_handle.abort();
    }

    #[tokio::test]
    async fn test_chaos_vault_blocked_mid_flight_closes_sqlx_pool_and_reauthenticates() {
        let state = MockVaultState::new(3600);
        let (vault_url, server_handle) = spawn_mock_vault_server(Arc::clone(&state)).await;
        let jwt_path = write_temp_jwt_file("chaos");

        let manager = VaultSecretManager::new(test_vault_config(vault_url, jwt_path.clone(), 3600));
        manager.initialize_on_startup().await.unwrap();

        let initial_pool = manager.pg_pool().expect("initial PgPool active");
        assert!(!initial_pool.is_closed());
        let (pg_v1, _) = manager.active_leases().await;
        let t0 = pg_v1.unwrap().obtained_at;

        // 1. Block Vault access mid-flight during the renewal window
        state.blocked.store(true, Ordering::SeqCst);

        let outcome_during_outage = manager
            .maintain_leases_once(t0 + Duration::from_secs(3000))
            .await
            .unwrap();
        assert_eq!(
            outcome_during_outage,
            LeaseMaintenanceOutcome::PoolClosedOnExpiry
        );

        // Active SQLx PgPool MUST be closed and dropped, and Redis client cleared
        assert!(
            initial_pool.is_closed(),
            "initial sqlx::PgPool must be closed when renewal fails and Vault is unreachable"
        );
        assert!(manager.pg_pool().is_none());
        assert!(manager.redis_client().is_none());
        assert_eq!(manager.pool_closures.load(Ordering::SeqCst), 1);

        // 2. Restore Vault access; next maintenance cycle must re-authenticate via K8s ServiceAccount JWT
        state.blocked.store(false, Ordering::SeqCst);

        let outcome_recovery = manager
            .maintain_leases_once(t0 + Duration::from_secs(3700))
            .await
            .unwrap();
        assert_eq!(
            outcome_recovery,
            LeaseMaintenanceOutcome::ReauthenticatedAndRotated
        );
        assert_eq!(manager.reauthentications.load(Ordering::SeqCst), 1);
        assert_eq!(state.login_calls.load(Ordering::SeqCst), 2);

        // Fresh v2 credentials and a new open sqlx::PgPool must now be installed
        let (pg_v2, redis_v2) = manager.active_leases().await;
        assert_eq!(pg_v2.unwrap().username, "v_k8s_pg_user_v2");
        assert_eq!(redis_v2.unwrap().username, "v_k8s_redis_user_v2");

        let recovered_pool = manager.pg_pool().expect("recovered PgPool installed");
        assert!(!recovered_pool.is_closed());
        assert!(manager.redis_client().is_some());

        let _ = std::fs::remove_file(jwt_path);
        server_handle.abort();
    }
}
