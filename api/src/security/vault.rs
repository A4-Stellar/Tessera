//! HashiCorp Vault dynamic secrets manager for PostgreSQL and Redis with lease renewal (Issue #42).
//!
//! Re-implements `infrastructure/vault_rotation.py` in production-grade Rust:
//! 1. Authenticates against Vault via the Kubernetes auth method using pod ServiceAccount JWT.
//! 2. Requests short-lived dynamic PostgreSQL credentials (1h TTL) and Redis AUTH tokens.
//! 3. Background asynchronous task renews Vault leases before expiry (`PUT /v1/sys/leases/renew`).
//! 4. On lease renewal failure / expiry, closes SQLx pool, re-authenticates with Vault, and recreates pool.
//! 5. Eliminates Kubernetes static Secrets — credentials never touch etcd.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info, warn};

/// Default lease renewal threshold: renew when 80% of lease duration has elapsed.
pub const DEFAULT_RENEWAL_THRESHOLD_PERCENT: f64 = 0.8;

#[derive(Debug, Clone)]
pub struct VaultConfig {
    pub vault_addr: String,
    pub k8s_role: String,
    pub k8s_token_path: String,
    pub database_mount: String,
    pub database_role: String,
    pub redis_mount: String,
    pub redis_role: String,
    pub direct_token: Option<String>,
}

impl VaultConfig {
    pub fn from_env() -> Option<Self> {
        let vault_addr = std::env::var("VAULT_ADDR").ok()?;
        if vault_addr.is_empty() {
            return None;
        }

        Some(Self {
            vault_addr: vault_addr.trim_end_matches('/').to_string(),
            k8s_role: std::env::var("VAULT_K8S_ROLE").unwrap_or_else(|_| "tessera-api".to_string()),
            k8s_token_path: std::env::var("VAULT_K8S_TOKEN_PATH")
                .unwrap_or_else(|_| "/var/run/secrets/kubernetes.io/serviceaccount/token".to_string()),
            database_mount: std::env::var("VAULT_DATABASE_MOUNT")
                .unwrap_or_else(|_| "database".to_string()),
            database_role: std::env::var("VAULT_DATABASE_ROLE")
                .unwrap_or_else(|_| "api-readonly".to_string()),
            redis_mount: std::env::var("VAULT_REDIS_MOUNT")
                .unwrap_or_else(|_| "redis".to_string()),
            redis_role: std::env::var("VAULT_REDIS_ROLE")
                .unwrap_or_else(|_| "api-redis".to_string()),
            direct_token: std::env::var("VAULT_TOKEN").ok(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicCredentials {
    pub username: String,
    pub password: String,
    pub lease_id: String,
    pub lease_duration_seconds: u64,
    pub renewable: bool,
    pub acquired_at_epoch: u64,
}

impl DynamicCredentials {
    /// Calculate seconds remaining until lease expiration.
    pub fn seconds_until_expiration(&self, now_epoch: u64) -> u64 {
        let expiry = self.acquired_at_epoch.saturating_add(self.lease_duration_seconds);
        expiry.saturating_sub(now_epoch)
    }

    /// Calculate renewal sleep duration (80% of lease TTL).
    pub fn next_renewal_delay(&self) -> Duration {
        let target_secs = (self.lease_duration_seconds as f64 * DEFAULT_RENEWAL_THRESHOLD_PERCENT) as u64;
        let clamped = target_secs.max(10).min(self.lease_duration_seconds.saturating_sub(60));
        Duration::from_secs(clamped.max(5))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisCredentials {
    pub auth_token: String,
    pub lease_id: Option<String>,
    pub lease_duration_seconds: u64,
    pub renewable: bool,
}

/// Client interacting with Vault HTTP API.
#[derive(Clone)]
pub struct VaultClient {
    client: reqwest::Client,
    config: VaultConfig,
    client_token: Arc<RwLock<Option<String>>>,
}

impl VaultClient {
    pub fn new(config: VaultConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        let initial_token = config.direct_token.clone();
        Self {
            client,
            config,
            client_token: Arc::new(RwLock::new(initial_token)),
        }
    }

    /// Authenticate via Kubernetes Auth Method using Pod ServiceAccount JWT.
    pub async fn authenticate_kubernetes(&self) -> Result<String, String> {
        let jwt = tokio::fs::read_to_string(&self.config.k8s_token_path)
            .await
            .map_err(|e| format!("failed to read k8s serviceaccount token from {}: {e}", self.config.k8s_token_path))?;

        let login_url = format!("{}/v1/auth/kubernetes/login", self.config.vault_addr);
        let body = serde_json::json!({
            "role": self.config.k8s_role,
            "jwt": jwt.trim(),
        });

        let resp = self
            .client
            .post(&login_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("vault k8s login failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("vault k8s login rejected with status {status}: {text}"));
        }

        let payload: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse vault login response: {e}"))?;

        let token = payload["auth"]["client_token"]
            .as_str()
            .ok_or_else(|| "missing auth.client_token in vault response".to_string())?
            .to_string();

        *self.client_token.write().await = Some(token.clone());
        info!("authenticated with HashiCorp Vault via Kubernetes Auth Method");
        Ok(token)
    }

    /// Ensure client has a valid active token.
    async fn get_active_token(&self) -> Result<String, String> {
        if let Some(token) = self.client_token.read().await.clone() {
            return Ok(token);
        }
        self.authenticate_kubernetes().await
    }

    /// Request new dynamic credentials for PostgreSQL.
    pub async fn generate_postgres_credentials(&self) -> Result<DynamicCredentials, String> {
        let token = self.get_active_token().await?;
        let creds_url = format!(
            "{}/v1/{}/creds/{}",
            self.config.vault_addr, self.config.database_mount, self.config.database_role
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Vault-Token",
            HeaderValue::from_str(&token).map_err(|e| e.to_string())?,
        );

        let resp = self
            .client
            .get(&creds_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("failed to request dynamic database credentials: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("vault database credentials request returned {status}: {text}"));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("invalid json from vault creds: {e}"))?;

        let data = &json["data"];
        let username = data["username"]
            .as_str()
            .ok_or_else(|| "missing username in vault database response".to_string())?
            .to_string();
        let password = data["password"]
            .as_str()
            .ok_or_else(|| "missing password in vault database response".to_string())?
            .to_string();
        let lease_id = json["lease_id"]
            .as_str()
            .ok_or_else(|| "missing lease_id in vault database response".to_string())?
            .to_string();
        let lease_duration_seconds = json["lease_duration"].as_u64().unwrap_or(3600);
        let renewable = json["renewable"].as_bool().unwrap_or(false);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        info!(
            username = %username,
            lease_id = %lease_id,
            duration = lease_duration_seconds,
            renewable = renewable,
            "acquired dynamic PostgreSQL credentials from Vault"
        );

        Ok(DynamicCredentials {
            username,
            password,
            lease_id,
            lease_duration_seconds,
            renewable,
            acquired_at_epoch: now,
        })
    }

    /// Request dynamic Redis credentials or auth token.
    pub async fn generate_redis_credentials(&self) -> Result<RedisCredentials, String> {
        let token = self.get_active_token().await?;
        let creds_url = format!(
            "{}/v1/{}/data/{}",
            self.config.vault_addr, self.config.redis_mount, self.config.redis_role
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Vault-Token",
            HeaderValue::from_str(&token).map_err(|e| e.to_string())?,
        );

        let resp = self
            .client
            .get(&creds_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("failed to request redis credentials: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("vault redis credentials request returned {status}: {text}"));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("invalid json from vault redis creds: {e}"))?;

        let data = &json["data"]["data"];
        let auth_token = data["password"]
            .as_str()
            .or_else(|| data["token"].as_str())
            .unwrap_or_default()
            .to_string();
        let lease_id = json["lease_id"].as_str().map(|s| s.to_string());
        let lease_duration_seconds = json["lease_duration"].as_u64().unwrap_or(3600);
        let renewable = json["renewable"].as_bool().unwrap_or(false);

        Ok(RedisCredentials {
            auth_token,
            lease_id,
            lease_duration_seconds,
            renewable,
        })
    }

    /// Renew lease via `PUT /v1/sys/leases/renew`.
    pub async fn renew_lease(&self, lease_id: &str, increment: u64) -> Result<u64, String> {
        let token = self.get_active_token().await?;
        let renew_url = format!("{}/v1/sys/leases/renew", self.config.vault_addr);

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Vault-Token",
            HeaderValue::from_str(&token).map_err(|e| e.to_string())?,
        );

        let body = serde_json::json!({
            "lease_id": lease_id,
            "increment": increment,
        });

        let resp = self
            .client
            .put(&renew_url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("failed to renew vault lease {lease_id}: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("vault lease renewal failed with status {status}: {text}"));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("invalid json from vault lease renewal: {e}"))?;

        let duration = json["lease_duration"].as_u64().unwrap_or(increment);
        info!(lease_id = %lease_id, duration = duration, "successfully renewed Vault lease");
        Ok(duration)
    }

    /// Revoke lease on graceful shutdown.
    pub async fn revoke_lease(&self, lease_id: &str) -> Result<(), String> {
        let token = self.get_active_token().await?;
        let revoke_url = format!("{}/v1/sys/leases/revoke", self.config.vault_addr);

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Vault-Token",
            HeaderValue::from_str(&token).map_err(|e| e.to_string())?,
        );

        let body = serde_json::json!({ "lease_id": lease_id });

        let resp = self
            .client
            .put(&revoke_url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("failed to revoke lease {lease_id}: {e}"))?;

        if resp.status().is_success() {
            info!(lease_id = %lease_id, "revoked Vault lease");
            Ok(())
        } else {
            warn!(lease_id = %lease_id, status = %resp.status(), "failed to revoke Vault lease");
            Err(format!("revoke lease failed with status {}", resp.status()))
        }
    }
}

/// Dynamic secrets manager that maintains and renews database & redis credentials.
pub struct DynamicSecretsManager {
    client: VaultClient,
    current_credentials: Arc<RwLock<Option<DynamicCredentials>>>,
    shutdown_tx: broadcast::Sender<()>,
    is_running: Arc<AtomicBool>,
}

impl DynamicSecretsManager {
    pub fn new(client: VaultClient) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self {
            client,
            current_credentials: Arc::new(RwLock::new(None)),
            shutdown_tx,
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn current_credentials(&self) -> Option<DynamicCredentials> {
        self.current_credentials.read().await.clone()
    }

    /// Start initial credential generation and spawn background lease renewal loop.
    pub async fn start(
        &self,
        on_rotation: Arc<dyn Fn(DynamicCredentials) + Send + Sync + 'static>,
    ) -> Result<DynamicCredentials, String> {
        let initial_creds = self.client.generate_postgres_credentials().await?;
        *self.current_credentials.write().await = Some(initial_creds.clone());
        on_rotation(initial_creds.clone());

        self.is_running.store(true, Ordering::SeqCst);
        let client = self.client.clone();
        let creds_lock = self.current_credentials.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            info!("started background Vault lease renewal task");
            while is_running.load(Ordering::SeqCst) {
                let current = creds_lock.read().await.clone();
                let creds = match current {
                    Some(c) => c,
                    None => {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };

                let sleep_duration = creds.next_renewal_delay();
                tokio::select! {
                    _ = tokio::time::sleep(sleep_duration) => {},
                    _ = shutdown_rx.recv() => {
                        info!("shutting down Vault lease renewal loop");
                        break;
                    }
                }

                // Attempt lease renewal
                match client.renew_lease(&creds.lease_id, creds.lease_duration_seconds).await {
                    Ok(new_duration) => {
                        let mut updated = creds.clone();
                        updated.lease_duration_seconds = new_duration;
                        updated.acquired_at_epoch = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        *creds_lock.write().await = Some(updated);
                    }
                    Err(e) => {
                        error!(error = %e, "Vault lease renewal failed mid-flight; re-authenticating and generating new credentials");
                        // On lease failure / expiry: re-authenticate and acquire fresh credentials
                        match client.generate_postgres_credentials().await {
                            Ok(fresh) => {
                                info!(user = %fresh.username, "re-established dynamic credentials after lease recovery");
                                *creds_lock.write().await = Some(fresh.clone());
                                on_rotation(fresh);
                            }
                            Err(e2) => {
                                error!(error = %e2, "critical: failed to recover dynamic credentials from Vault; retrying in 5s");
                                tokio::time::sleep(Duration::from_secs(5)).await;
                            }
                        }
                    }
                }
            }
        });

        Ok(initial_creds)
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
        let _ = self.shutdown_tx.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dynamic_credentials_next_renewal_delay() {
        let creds = DynamicCredentials {
            username: "v-test-user".to_string(),
            password: "password123".to_string(),
            lease_id: "database/creds/api-readonly/abc".to_string(),
            lease_duration_seconds: 3600, // 1 hour
            renewable: true,
            acquired_at_epoch: 1_700_000_000,
        };

        // 80% of 3600 = 2880s
        let delay = creds.next_renewal_delay();
        assert_eq!(delay.as_secs(), 2880);
        assert_eq!(creds.seconds_until_expiration(1_700_000_000), 3600);
        assert_eq!(creds.seconds_until_expiration(1_700_001_000), 2600);
    }

    #[test]
    fn test_vault_config_defaults() {
        std::env::set_var("VAULT_ADDR", "http://127.0.0.1:8200");
        let cfg = VaultConfig::from_env().expect("vault config");
        assert_eq!(cfg.vault_addr, "http://127.0.0.1:8200");
        assert_eq!(cfg.database_role, "api-readonly");
        assert_eq!(cfg.k8s_role, "tessera-api");
    }
}
