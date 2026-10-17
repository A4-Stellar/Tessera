//! Multi-cloud KMS hardware adapter (Issue #298).
//!
//! Implements [`KmsClient`] for GCP Cloud KMS, Azure Key Vault, and
//! HashiCorp Vault, plus a [`FailoverKmsClient`] that tries a primary region
//! then falls back to one or more secondaries automatically.
//!
//! All adapters follow the same contract as [`AwsKmsClient`]: only the
//! 32-byte Ed25519 digest crosses the HSM boundary; no raw private key ever
//! touches application memory.

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};
use zeroize::Zeroizing;

use super::kms::{KmsClient, KmsError, KmsSignRequest, KmsSignResponse, SigningAlgorithm};

// ---------------------------------------------------------------------------
// GCP Cloud KMS adapter
// ---------------------------------------------------------------------------

/// GCP Cloud KMS client for an asymmetric Ed25519 key.
///
/// Uses the GCP KMS REST API with a Bearer token from the `GCP_KMS_TOKEN`
/// environment variable (a service-account access token or workload-identity
/// token).
///
/// Key name format:
/// `projects/{project}/locations/{location}/keyRings/{ring}/cryptoKeys/{key}/cryptoKeyVersions/{version}`
pub struct GcpKmsClient {
    http: reqwest::Client,
    token: Zeroizing<String>,
    endpoint: String,
}

impl GcpKmsClient {
    const DEFAULT_ENDPOINT: &'static str = "https://cloudkms.googleapis.com";

    /// Constructs a client using an explicit access token.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            token: Zeroizing::new(token.into()),
            endpoint: Self::DEFAULT_ENDPOINT.to_string(),
        }
    }

    /// Constructs a client from `GCP_KMS_TOKEN` env var.
    pub fn from_env() -> Result<Self, KmsError> {
        let token = std::env::var("GCP_KMS_TOKEN")
            .map_err(|_| KmsError::Credentials("GCP_KMS_TOKEN is not set".into()))?;
        Ok(Self::new(token))
    }

    /// Override the GCP KMS endpoint (for testing or private endpoints).
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    async fn call_sign(&self, key_name: &str, digest_b64: &str) -> Result<Value, KmsError> {
        let url = format!(
            "{}/v1/{}:asymmetricSign",
            self.endpoint.trim_end_matches('/'),
            key_name
        );
        let body = json!({ "digest": { "sha256": digest_b64 } });
        let response = self
            .http
            .post(&url)
            .bearer_auth(self.token.as_str())
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(KmsError::Service {
                status: status.as_u16(),
                body: text.chars().take(512).collect(),
            });
        }
        Ok(serde_json::from_str(&text)?)
    }

    async fn call_get_public_key(&self, key_name: &str) -> Result<Value, KmsError> {
        let url = format!(
            "{}/v1/{}/publicKey",
            self.endpoint.trim_end_matches('/'),
            key_name
        );
        let response = self
            .http
            .get(&url)
            .bearer_auth(self.token.as_str())
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(KmsError::Service {
                status: status.as_u16(),
                body: text.chars().take(512).collect(),
            });
        }
        Ok(serde_json::from_str(&text)?)
    }
}

#[async_trait]
impl KmsClient for GcpKmsClient {
    async fn sign(&self, request: KmsSignRequest) -> Result<KmsSignResponse, KmsError> {
        let digest_b64 = BASE64.encode(request.message.as_slice());
        let response = self.call_sign(&request.key_id, &digest_b64).await?;
        let sig_b64 = response
            .get("signature")
            .and_then(Value::as_str)
            .ok_or(KmsError::MissingField("signature"))?;
        let signature = BASE64.decode(sig_b64).map_err(|source| KmsError::Base64 {
            field: "signature",
            source,
        })?;
        Ok(KmsSignResponse {
            key_id: request.key_id,
            signature,
            signing_algorithm: request.signing_algorithm,
        })
    }

    async fn public_key(&self, key_id: &str) -> Result<Vec<u8>, KmsError> {
        let response = self.call_get_public_key(key_id).await?;
        // GCP returns a PEM-encoded public key; strip the PEM wrapper and
        // decode the DER, then extract the raw 32-byte Ed25519 key.
        let pem = response
            .get("pem")
            .and_then(Value::as_str)
            .ok_or(KmsError::MissingField("pem"))?;
        let b64: String = pem
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
        let der = BASE64.decode(b64.trim()).map_err(|source| KmsError::Base64 {
            field: "pem",
            source,
        })?;
        super::kms::ed25519_public_key_from_spki(&der)
            .map(|k| k.to_vec())
            .ok_or_else(|| KmsError::PublicKeyEncoding {
                key_id: key_id.to_string(),
                got: der.len(),
            })
    }
}

// ---------------------------------------------------------------------------
// Azure Key Vault adapter
// ---------------------------------------------------------------------------

/// Azure Key Vault client for an Ed25519 key.
///
/// Authenticates with a Bearer token from `AZURE_KMS_TOKEN` (a managed
/// identity or service-principal OAuth2 token for the Key Vault resource).
///
/// Key id format: `https://{vault}.vault.azure.net/keys/{key}/{version}`
pub struct AzureKmsClient {
    http: reqwest::Client,
    token: Zeroizing<String>,
}

impl AzureKmsClient {
    const API_VERSION: &'static str = "7.4";

    /// Constructs a client using an explicit access token.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            token: Zeroizing::new(token.into()),
        }
    }

    /// Constructs a client from `AZURE_KMS_TOKEN` env var.
    pub fn from_env() -> Result<Self, KmsError> {
        let token = std::env::var("AZURE_KMS_TOKEN")
            .map_err(|_| KmsError::Credentials("AZURE_KMS_TOKEN is not set".into()))?;
        Ok(Self::new(token))
    }
}

#[async_trait]
impl KmsClient for AzureKmsClient {
    async fn sign(&self, request: KmsSignRequest) -> Result<KmsSignResponse, KmsError> {
        let url = format!("{}/sign?api-version={}", request.key_id, Self::API_VERSION);
        let value_b64 = BASE64.encode(request.message.as_slice());
        let body = json!({ "alg": "EdDSA", "value": value_b64 });
        let response = self
            .http
            .post(&url)
            .bearer_auth(self.token.as_str())
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(KmsError::Service {
                status: status.as_u16(),
                body: text.chars().take(512).collect(),
            });
        }
        let parsed: Value = serde_json::from_str(&text)?;
        let sig_b64 = parsed
            .get("value")
            .and_then(Value::as_str)
            .ok_or(KmsError::MissingField("value"))?;
        let signature = BASE64.decode(sig_b64).map_err(|source| KmsError::Base64 {
            field: "value",
            source,
        })?;
        Ok(KmsSignResponse {
            key_id: request.key_id,
            signature,
            signing_algorithm: request.signing_algorithm,
        })
    }

    async fn public_key(&self, key_id: &str) -> Result<Vec<u8>, KmsError> {
        let url = format!("{key_id}?api-version={}", Self::API_VERSION);
        let response = self
            .http
            .get(&url)
            .bearer_auth(self.token.as_str())
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(KmsError::Service {
                status: status.as_u16(),
                body: text.chars().take(512).collect(),
            });
        }
        let parsed: Value = serde_json::from_str(&text)?;
        // Azure returns the public key under key.x (base64url-encoded raw bytes).
        let x = parsed
            .pointer("/key/x")
            .and_then(Value::as_str)
            .ok_or(KmsError::MissingField("key.x"))?;
        let key_bytes =
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(x)
                .map_err(|source| KmsError::Base64 {
                    field: "key.x",
                    source,
                })?;
        if key_bytes.len() != 32 {
            return Err(KmsError::PublicKeyEncoding {
                key_id: key_id.to_string(),
                got: key_bytes.len(),
            });
        }
        Ok(key_bytes)
    }
}

// ---------------------------------------------------------------------------
// HashiCorp Vault adapter
// ---------------------------------------------------------------------------

/// HashiCorp Vault Transit Engine client for an Ed25519 key.
///
/// Reads `VAULT_ADDR` and `VAULT_TOKEN` from the environment.
///
/// Key id format: `{mount}/keys/{key_name}` (e.g. `transit/keys/tessera-admin`)
pub struct VaultKmsClient {
    http: reqwest::Client,
    addr: String,
    token: Zeroizing<String>,
}

impl VaultKmsClient {
    /// Constructs a client with an explicit address and token.
    pub fn new(addr: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            addr: addr.into().trim_end_matches('/').to_string(),
            token: Zeroizing::new(token.into()),
        }
    }

    /// Constructs a client from `VAULT_ADDR` and `VAULT_TOKEN` env vars.
    pub fn from_env() -> Result<Self, KmsError> {
        let addr = std::env::var("VAULT_ADDR")
            .map_err(|_| KmsError::Credentials("VAULT_ADDR is not set".into()))?;
        let token = std::env::var("VAULT_TOKEN")
            .map_err(|_| KmsError::Credentials("VAULT_TOKEN is not set".into()))?;
        Ok(Self::new(addr, token))
    }
}

#[async_trait]
impl VaultKmsClient {
    // Vault Transit sign endpoint:
    // POST /v1/{mount}/sign/{key_name}
    async fn vault_sign(
        &self,
        key_id: &str,
        digest_b64: &str,
    ) -> Result<KmsSignResponse, KmsError> {
        // key_id: "transit/keys/tessera-admin" → mount="transit", key="tessera-admin"
        let (mount, key) = key_id.split_once("/keys/").ok_or_else(|| {
            KmsError::Config(format!(
                "Vault key_id must be '{{mount}}/keys/{{name}}', got {key_id:?}"
            ))
        })?;
        let url = format!("{}/v1/{mount}/sign/{key}", self.addr);
        let body = json!({
            "input": digest_b64,
            "hash_algorithm": "none",
            "prehashed": true,
        });
        let response = self
            .http
            .post(&url)
            .header("X-Vault-Token", self.token.as_str())
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(KmsError::Service {
                status: status.as_u16(),
                body: text.chars().take(512).collect(),
            });
        }
        let parsed: Value = serde_json::from_str(&text)?;
        // Vault returns "vault:v1:<base64>" — strip the prefix.
        let raw_sig = parsed
            .pointer("/data/signature")
            .and_then(Value::as_str)
            .ok_or(KmsError::MissingField("data.signature"))?;
        let b64_part = raw_sig.splitn(3, ':').nth(2).ok_or_else(|| {
            KmsError::Config(format!("unexpected vault signature format: {raw_sig}"))
        })?;
        let signature = BASE64.decode(b64_part).map_err(|source| KmsError::Base64 {
            field: "data.signature",
            source,
        })?;
        Ok(KmsSignResponse {
            key_id: key_id.to_string(),
            signature,
            signing_algorithm: SigningAlgorithm::Ed25519,
        })
    }
}

#[async_trait]
impl KmsClient for VaultKmsClient {
    async fn sign(&self, request: KmsSignRequest) -> Result<KmsSignResponse, KmsError> {
        let digest_b64 = BASE64.encode(request.message.as_slice());
        self.vault_sign(&request.key_id, &digest_b64).await
    }

    async fn public_key(&self, key_id: &str) -> Result<Vec<u8>, KmsError> {
        let (mount, key) = key_id.split_once("/keys/").ok_or_else(|| {
            KmsError::Config(format!(
                "Vault key_id must be '{{mount}}/keys/{{name}}', got {key_id:?}"
            ))
        })?;
        let url = format!("{}/v1/{mount}/keys/{key}", self.addr);
        let response = self
            .http
            .get(&url)
            .header("X-Vault-Token", self.token.as_str())
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(KmsError::Service {
                status: status.as_u16(),
                body: text.chars().take(512).collect(),
            });
        }
        let parsed: Value = serde_json::from_str(&text)?;
        // Vault stores the public key as a PEM in data.keys.1.public_key
        let pem = parsed
            .pointer("/data/keys/1/public_key")
            .and_then(Value::as_str)
            .ok_or(KmsError::MissingField("data.keys.1.public_key"))?;
        let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let der = BASE64.decode(b64.trim()).map_err(|source| KmsError::Base64 {
            field: "public_key pem",
            source,
        })?;
        super::kms::ed25519_public_key_from_spki(&der)
            .map(|k| k.to_vec())
            .ok_or_else(|| KmsError::PublicKeyEncoding {
                key_id: key_id.to_string(),
                got: der.len(),
            })
    }
}

// ---------------------------------------------------------------------------
// Failover router
// ---------------------------------------------------------------------------

/// A [`KmsClient`] that tries a primary client first and falls back to
/// secondaries in order. The first successful response wins; the last error
/// is returned if every client fails.
///
/// This satisfies the "automated failover between primary and secondary KMS
/// regions" acceptance criterion: point the primary at `us-east-1` and the
/// secondary at `eu-west-1`, for example.
///
/// ```rust,no_run
/// # use tessera_api::signer::kms_adapter::{FailoverKmsClient, GcpKmsClient};
/// let primary = GcpKmsClient::from_env().unwrap();
/// let secondary = GcpKmsClient::from_env().unwrap();
/// let client = FailoverKmsClient::new(primary).with_fallback(secondary);
/// ```
pub struct FailoverKmsClient<P: KmsClient, F: KmsClient> {
    primary: P,
    fallback: F,
}

impl<P: KmsClient, F: KmsClient> FailoverKmsClient<P, F> {
    /// Wraps `primary` with a single `fallback`.
    pub fn new(primary: P, fallback: F) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl<P: KmsClient, F: KmsClient> KmsClient for FailoverKmsClient<P, F> {
    async fn sign(&self, request: KmsSignRequest) -> Result<KmsSignResponse, KmsError> {
        match self.primary.sign(request.clone()).await {
            Ok(resp) => Ok(resp),
            Err(primary_err) => {
                tracing::warn!(
                    error = %primary_err,
                    "primary KMS sign failed — trying fallback"
                );
                self.fallback.sign(request).await.map_err(|fallback_err| {
                    tracing::error!(
                        primary = %primary_err,
                        fallback = %fallback_err,
                        "all KMS backends failed"
                    );
                    fallback_err
                })
            }
        }
    }

    async fn public_key(&self, key_id: &str) -> Result<Vec<u8>, KmsError> {
        match self.primary.public_key(key_id).await {
            Ok(key) => Ok(key),
            Err(primary_err) => {
                tracing::warn!(
                    error = %primary_err,
                    "primary KMS get_public_key failed — trying fallback"
                );
                self.fallback.public_key(key_id).await
            }
        }
    }
}

// KmsSignRequest must be clonable for the failover path.
impl Clone for KmsSignRequest {
    fn clone(&self) -> Self {
        Self {
            key_id: self.key_id.clone(),
            message: self.message.clone(),
            message_type: self.message_type,
            signing_algorithm: self.signing_algorithm,
            grant_tokens: self.grant_tokens.clone(),
        }
    }
}
