//! HSM / AWS KMS signer for Stellar transaction envelopes.
//!
//! The API process is read-only, but the protocol is not: dividends, rent and
//! registry updates are written on chain by scheduled jobs. Those jobs have to
//! authorise transactions without a raw Stellar secret key living in an
//! environment variable, so the key stays inside a managed HSM (AWS KMS, or any
//! other provider that can produce Ed25519 signatures) and only the signature
//! ever leaves it.
//!
//! Three pieces make that work:
//!
//! * [`KmsClient`] is the boundary to the HSM. It is deliberately narrow: the
//!   signer hands over the 32-byte SHA-256 digest of the Stellar signature
//!   payload and gets back a 64-byte Ed25519 signature. [`AwsKmsClient`]
//!   implements the trait against the documented KMS `Sign`/`GetPublicKey` JSON
//!   API, signing each call with AWS Signature Version 4, so the (large) AWS
//!   SDK is not required; a GCP KMS, PKCS#11 or in-memory implementation only
//!   has to implement the same trait.
//! * [`KmsSigner`] builds the `TransactionSignaturePayload` that binds a
//!   transaction to a network, hashes it, asks the HSM to sign that digest and
//!   attaches the resulting `DecoratedSignature` to a copy of the envelope.
//! * [`SecretBytes`] is a heap buffer that is zeroed when it is dropped.
//!
//! Only the digest crosses the HSM boundary, and every copy of it is scrubbed
//! as soon as it is no longer needed: the XDR encoding that gets hashed, the
//! digest computed for a signature payload, the message inside the request the
//! client builds, the response buffer a signature is copied out of, and the
//! intermediate keys derived for SigV4.
//!
//! ```text
//! let client = AwsKmsClient::from_env()?;                     // AWS_* env vars
//! let signer = KmsSigner::from_kms(client, "alias/tessera-dividends").await?;
//! let unsigned = TransactionEnvelope::from(transaction);       // raw, no signatures
//! let signed = signer.sign_envelope(&unsigned, TESTNET_PASSPHRASE).await?;
//! ```

use std::fmt;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use stellar_xdr::curr::{
    BytesM, DecoratedSignature, FeeBumpTransaction, Hash, Limits, Signature, SignatureHint,
    Transaction, TransactionEnvelope, TransactionSignaturePayload,
    TransactionSignaturePayloadTaggedTransaction, WriteXdr,
};
use zeroize::{Zeroize, Zeroizing};

/// The largest number of signatures a Stellar transaction envelope can carry.
pub const MAX_SIGNATURES: usize = 20;

/// Network passphrase of Stellar's public network.
pub const MAINNET_PASSPHRASE: &str = "Public Global Stellar Network ; September 2015";

/// Network passphrase of Stellar's test network.
pub const TESTNET_PASSPHRASE: &str = "Test SDF Network ; September 2015";

/// Length of an Ed25519 signature, in bytes.
const SIGNATURE_LENGTH: usize = 64;

/// Longest error body echoed back to the caller; keeps a misbehaving endpoint
/// from filling the logs.
const MAX_ERROR_BODY: usize = 512;

const AWS4_PREFIX: &[u8] = b"AWS4";
const KMS_SERVICE: &str = "kms";
const KMS_CONTENT_TYPE: &str = "application/x-amz-json-1.1";
const KMS_SIGN_TARGET: &str = "TrentService.Sign";
const KMS_GET_PUBLIC_KEY_TARGET: &str = "TrentService.GetPublicKey";

/// DER prefix of an Ed25519 `SubjectPublicKeyInfo`, the form AWS KMS returns
/// from `GetPublicKey`; the 32 bytes that follow it are the raw public key.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

// ---------------------------------------------------------------------------
// Secret buffers
// ---------------------------------------------------------------------------

/// A heap byte buffer that is zeroed when it is dropped.
///
/// The digest handed to an HSM is not the private key, but a signature over a
/// known digest is enough to authorise the transaction it belongs to, so every
/// copy of it is treated as secret material and overwritten before the
/// allocation is released.
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// Wraps `bytes` in a buffer that is zeroed on drop.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes, borrowed — they are never copied out implicitly.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Number of bytes held.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Clone for SecretBytes {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Zeroize for SecretBytes {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        // Runs on the success path, the error path and on an early return: the
        // buffer is overwritten before the allocation is released.
        self.zeroize();
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the contents: this buffer exists precisely because its
        // contents authorise transactions.
        let len = self.0.len();
        write!(f, "SecretBytes({len} bytes, redacted)")
    }
}

// ---------------------------------------------------------------------------
// KMS request and response types
// ---------------------------------------------------------------------------

/// The `MessageType` value sent to AWS KMS.
///
/// Stellar signs a pre-computed SHA-256 digest, and for an Ed25519 key that
/// digest *is* the message. AWS KMS only accepts `DIGEST` for RSA keys, so the
/// signer always sends [`MessageType::Raw`] with the 32 digest bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    /// The message is the raw byte string to sign.
    Raw,
    /// The message is a pre-computed digest (RSA keys only).
    Digest,
}

impl MessageType {
    /// The string AWS KMS expects on the wire.
    pub const fn as_aws_str(&self) -> &'static str {
        match self {
            Self::Raw => "RAW",
            Self::Digest => "DIGEST",
        }
    }
}

/// The `SigningAlgorithm` value sent to AWS KMS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigningAlgorithm {
    /// Ed25519, the signature scheme Stellar accounts use.
    Ed25519,
}

impl SigningAlgorithm {
    /// The string AWS KMS expects on the wire.
    pub const fn as_aws_str(&self) -> &'static str {
        "ED25519"
    }
}

/// A request to sign bytes with a key held in an HSM.
#[derive(Debug, Clone)]
pub struct KmsSignRequest {
    /// Provider-specific key identifier; for AWS, a key id or an alias ARN.
    pub key_id: String,
    /// The bytes to sign, held in a [`SecretBytes`] so this copy is scrubbed
    /// when the request is dropped, on the success and failure paths alike.
    pub message: SecretBytes,
    /// How the HSM should interpret `message`.
    pub message_type: MessageType,
    /// Signature scheme to use.
    pub signing_algorithm: SigningAlgorithm,
    /// Optional AWS KMS grant tokens.
    pub grant_tokens: Vec<String>,
}

impl KmsSignRequest {
    /// A request to sign a 32-byte Stellar transaction digest with an Ed25519
    /// key.
    pub fn ed25519_digest(key_id: impl Into<String>, digest: &[u8; 32]) -> Self {
        Self {
            key_id: key_id.into(),
            message: SecretBytes::new(digest.to_vec()),
            message_type: MessageType::Raw,
            signing_algorithm: SigningAlgorithm::Ed25519,
            grant_tokens: Vec::new(),
        }
    }
}

/// An HSM's answer to a [`KmsSignRequest`].
#[derive(Debug, Clone)]
pub struct KmsSignResponse {
    /// The key that produced the signature.
    pub key_id: String,
    /// The signature; 64 bytes for an Ed25519 Stellar key.
    pub signature: Vec<u8>,
    /// The scheme the HSM used.
    pub signing_algorithm: SigningAlgorithm,
}

/// Failures raised by a [`KmsClient`].
#[derive(Debug, thiserror::Error)]
pub enum KmsError {
    /// The request never reached the HSM.
    #[error("kms transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// The HSM answered with an HTTP error that was not a JSON error document.
    #[error("kms returned HTTP {status}: {body}")]
    Service { status: u16, body: String },
    /// The HSM rejected the request.
    #[error("kms rejected the request ({kind}): {message}")]
    Rejected { kind: String, message: String },
    /// The response did not contain a field the caller needs.
    #[error("kms response is missing the {0} field")]
    MissingField(&'static str),
    /// A field that should hold base64 did not.
    #[error("kms {field} is not valid base64: {source}")]
    Base64 {
        field: &'static str,
        #[source]
        source: base64::DecodeError,
    },
    /// The HSM signed with a different algorithm than the one requested.
    #[error("kms signed with {0} instead of the requested algorithm")]
    UnexpectedAlgorithm(String),
    /// The returned public key is not a 32-byte Ed25519 key.
    #[error("kms returned a {got}-byte public key for {key_id} (expected 32)")]
    PublicKeyEncoding { key_id: String, got: usize },
    /// AWS credentials could not be loaded.
    #[error("aws credentials unavailable: {0}")]
    Credentials(String),
    /// The client was configured with something unusable.
    #[error("kms client misconfigured: {0}")]
    Config(String),
    /// A request or response body is not valid JSON.
    #[error("kms json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Failures raised while assembling or signing a Stellar envelope.
#[derive(Debug, thiserror::Error)]
pub enum SignerError {
    /// The HSM call failed.
    #[error(transparent)]
    Kms(#[from] KmsError),
    /// A transaction envelope could not be encoded or rebuilt.
    #[error("xdr error: {0}")]
    Xdr(#[from] stellar_xdr::curr::Error),
    /// The public key is not 32 bytes, so it is not an Ed25519 key.
    #[error("an ed25519 public key must be 32 bytes, got {0}")]
    InvalidPublicKey(usize),
    /// The HSM returned something that is not an Ed25519 signature.
    #[error("kms returned a {0}-byte signature (expected 64)")]
    InvalidSignature(usize),
    /// The envelope is in the pre-2018 v0 format, which cannot carry a v1
    /// signature payload.
    #[error("cannot sign a v0 transaction envelope; upgrade it to a v1 transaction first")]
    LegacyEnvelope,
    /// The envelope already carries the maximum number of signatures.
    #[error("the envelope already carries the maximum of 20 signatures")]
    TooManySignatures,
}

/// The boundary between the signer and the key material.
///
/// Implementations hold no Stellar secret key: they ask an HSM to sign bytes
/// and return whatever signature it produces.
#[async_trait]
pub trait KmsClient: Send + Sync {
    /// Signs `request.message` with the key named by `request.key_id`.
    async fn sign(&self, request: KmsSignRequest) -> Result<KmsSignResponse, KmsError>;

    /// Returns the raw 32-byte Ed25519 public key for `key_id`.
    async fn public_key(&self, key_id: &str) -> Result<Vec<u8>, KmsError>;
}

// ---------------------------------------------------------------------------
// Stellar signature payloads
// ---------------------------------------------------------------------------

/// The network id a Stellar signature payload is bound to: the SHA-256 of the
/// network passphrase.
pub fn network_id(network_passphrase: &str) -> [u8; 32] {
    Sha256::digest(network_passphrase.as_bytes()).into()
}

/// Computes the 32-byte digest a signer must authorise for `envelope`.
///
/// This is the SHA-256 of the XDR encoding of the `TransactionSignaturePayload`,
/// which binds the transaction to `network_passphrase`. It is exactly the value
/// [`KmsSigner::sign_envelope`] hands to the HSM.
pub fn envelope_digest(
    envelope: &TransactionEnvelope,
    network_passphrase: &str,
) -> Result<[u8; 32], SignerError> {
    let network_id = network_id(network_passphrase);
    match envelope {
        TransactionEnvelope::Tx(env) => transaction_digest(&env.tx, network_id),
        TransactionEnvelope::TxFeeBump(env) => fee_bump_digest(&env.tx, network_id),
        // V0 is the pre-2018 legacy format: its signature payload is defined
        // over the v1 form of the transaction, and the signature could not be
        // attached back to a v0 envelope, so the caller must upgrade it first.
        TransactionEnvelope::TxV0(_) => Err(SignerError::LegacyEnvelope),
    }
}

fn transaction_digest(tx: &Transaction, network_id: [u8; 32]) -> Result<[u8; 32], SignerError> {
    let payload = TransactionSignaturePayload {
        network_id: Hash(network_id),
        tagged_transaction: TransactionSignaturePayloadTaggedTransaction::Tx(tx.clone()),
    };
    payload_digest(&payload)
}

fn fee_bump_digest(tx: &FeeBumpTransaction, network_id: [u8; 32]) -> Result<[u8; 32], SignerError> {
    let payload = TransactionSignaturePayload {
        network_id: Hash(network_id),
        tagged_transaction: TransactionSignaturePayloadTaggedTransaction::TxFeeBump(tx.clone()),
    };
    payload_digest(&payload)
}

/// SHA-256 of the XDR encoding of a signature payload.
fn payload_digest(payload: &TransactionSignaturePayload) -> Result<[u8; 32], SignerError> {
    // The encoding is an intermediate buffer that is only needed to hash;
    // scrub it as soon as the digest exists.
    let mut encoded = payload.to_xdr(Limits::none())?;
    let digest = Sha256::digest(&encoded);
    encoded.zeroize();
    Ok(digest.into())
}

/// Wraps a 64-byte signature together with the hint Stellar expects next to it.
fn decorated_signature(
    hint: SignatureHint,
    signature: [u8; SIGNATURE_LENGTH],
) -> Result<DecoratedSignature, SignerError> {
    let signature = BytesM::<64>::try_from(signature)?;
    Ok(DecoratedSignature {
        hint,
        signature: Signature::from(signature),
    })
}

/// Copies `envelope` and appends `signature` to its signature list.
fn append_signature(
    envelope: &TransactionEnvelope,
    signature: DecoratedSignature,
) -> Result<TransactionEnvelope, SignerError> {
    let mut signed = envelope.clone();
    let signatures = match &mut signed {
        TransactionEnvelope::Tx(env) => &mut env.signatures,
        TransactionEnvelope::TxFeeBump(env) => &mut env.signatures,
        TransactionEnvelope::TxV0(_) => return Err(SignerError::LegacyEnvelope),
    };
    let mut updated = signatures.to_vec();
    // Report the overflow explicitly rather than leaking the XDR length error
    // out of the `VecM` conversion below.
    if updated.len() >= MAX_SIGNATURES {
        return Err(SignerError::TooManySignatures);
    }
    updated.push(signature);
    *signatures = updated.try_into()?;
    Ok(signed)
}

// ---------------------------------------------------------------------------
// KmsSigner
// ---------------------------------------------------------------------------

/// Signs Stellar envelopes with a key that never leaves an HSM.
pub struct KmsSigner<C: KmsClient> {
    client: C,
    key_id: String,
    public_key: [u8; 32],
}

impl<C: KmsClient> KmsSigner<C> {
    /// Builds a signer from an already-known public key.
    pub fn new(client: C, key_id: impl Into<String>, public_key: [u8; 32]) -> Self {
        Self {
            client,
            key_id: key_id.into(),
            public_key,
        }
    }

    /// Builds a signer by asking the HSM for the public key of `key_id`.
    pub async fn from_kms(client: C, key_id: impl Into<String>) -> Result<Self, SignerError> {
        let key_id = key_id.into();
        let public_key = client.public_key(&key_id).await?;
        Self::from_public_key_bytes(client, key_id, &public_key)
    }

    /// Builds a signer from the raw public key bytes an HSM returned.
    pub fn from_public_key_bytes(
        client: C,
        key_id: impl Into<String>,
        public_key: &[u8],
    ) -> Result<Self, SignerError> {
        let public_key: [u8; 32] = public_key
            .try_into()
            .map_err(|_| SignerError::InvalidPublicKey(public_key.len()))?;
        Ok(Self::new(client, key_id, public_key))
    }

    /// The HSM client this signer uses.
    pub fn client(&self) -> &C {
        &self.client
    }

    /// The HSM key identifier.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The signer's raw Ed25519 public key.
    pub fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }

    /// The 4-byte hint Stellar stores next to a signature: the last four bytes
    /// of the signer's public key.
    pub fn signature_hint(&self) -> SignatureHint {
        SignatureHint([
            self.public_key[28],
            self.public_key[29],
            self.public_key[30],
            self.public_key[31],
        ])
    }

    /// Asks the HSM to sign a 32-byte Stellar transaction digest.
    ///
    /// Only the digest crosses the HSM boundary. The copy held by the request
    /// is zeroed the moment the request is dropped, and the response buffer the
    /// signature is read out of is zeroed before it is released too.
    pub async fn sign_digest(
        &self,
        digest: &[u8; 32],
    ) -> Result<[u8; SIGNATURE_LENGTH], SignerError> {
        let request = KmsSignRequest::ed25519_digest(self.key_id.clone(), digest);
        let response = self.client.sign(request).await?;
        let mut signature = response.signature;
        if signature.len() != SIGNATURE_LENGTH {
            let len = signature.len();
            signature.zeroize();
            return Err(SignerError::InvalidSignature(len));
        }
        let mut signature_bytes = [0u8; SIGNATURE_LENGTH];
        signature_bytes.copy_from_slice(&signature);
        signature.zeroize();
        Ok(signature_bytes)
    }

    /// Hashes and signs an already-built signature payload.
    pub async fn sign_transaction_payload(
        &self,
        payload: &TransactionSignaturePayload,
    ) -> Result<DecoratedSignature, SignerError> {
        let mut digest = payload_digest(payload)?;
        let signature = self.sign_digest(&digest).await;
        digest.zeroize();
        decorated_signature(self.signature_hint(), signature?)
    }

    /// Signs `envelope` for `network_passphrase` and returns a copy carrying the
    /// new signature. The envelope passed in is left untouched.
    pub async fn sign_envelope(
        &self,
        envelope: &TransactionEnvelope,
        network_passphrase: &str,
    ) -> Result<TransactionEnvelope, SignerError> {
        let mut digest = envelope_digest(envelope, network_passphrase)?;
        let signature = self.sign_digest(&digest).await;
        digest.zeroize();
        let decorated = decorated_signature(self.signature_hint(), signature?)?;
        append_signature(envelope, decorated)
    }
}

// ---------------------------------------------------------------------------
// AWS credentials
// ---------------------------------------------------------------------------

/// Long-lived AWS credentials used to authorise KMS API calls.
///
/// The secret access key and the optional session token are held in
/// [`Zeroizing`] wrappers, so they are wiped when the credentials are dropped,
/// and they are never rendered by the [`fmt::Debug`] implementation.
#[derive(Clone)]
pub struct AwsCredentials {
    access_key_id: String,
    secret_access_key: Zeroizing<String>,
    session_token: Option<Zeroizing<String>>,
}

impl AwsCredentials {
    /// Credentials from an explicit, long-lived access key pair.
    pub fn new(access_key_id: impl Into<String>, secret_access_key: impl Into<String>) -> Self {
        Self {
            access_key_id: access_key_id.into(),
            secret_access_key: Zeroizing::new(secret_access_key.into()),
            session_token: None,
        }
    }

    /// Attaches a session token, for temporary (STS) credentials.
    #[must_use]
    pub fn with_session_token(mut self, session_token: impl Into<String>) -> Self {
        self.session_token = Some(Zeroizing::new(session_token.into()));
        self
    }

    /// Reads the standard AWS environment variables: `AWS_ACCESS_KEY_ID`,
    /// `AWS_SECRET_ACCESS_KEY` and, when set, `AWS_SESSION_TOKEN`.
    pub fn from_env() -> Result<Self, KmsError> {
        let access_key_id = std::env::var("AWS_ACCESS_KEY_ID")
            .map_err(|_| KmsError::Credentials("AWS_ACCESS_KEY_ID is not set".to_string()))?;
        let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY")
            .map_err(|_| KmsError::Credentials("AWS_SECRET_ACCESS_KEY is not set".to_string()))?;
        let mut credentials = Self::new(access_key_id, secret_access_key);
        if let Ok(session_token) = std::env::var("AWS_SESSION_TOKEN") {
            if !session_token.is_empty() {
                credentials = credentials.with_session_token(session_token);
            }
        }
        Ok(credentials)
    }

    /// The AWS access key id (not secret).
    pub fn access_key_id(&self) -> &str {
        &self.access_key_id
    }

    /// The AWS secret access key.
    pub fn secret_access_key(&self) -> &str {
        self.secret_access_key.as_str()
    }

    /// The session token, when the credentials are temporary.
    pub fn session_token(&self) -> Option<&str> {
        self.session_token.as_ref().map(|token| token.as_str())
    }
}

impl fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AwsCredentials")
            .field("access_key_id", &self.access_key_id)
            .field("session_token", &self.session_token.is_some())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// AWS KMS client
// ---------------------------------------------------------------------------

/// AWS KMS client for an asymmetric Ed25519 key.
///
/// The client speaks the KMS JSON API directly (no AWS SDK), signing each
/// request with AWS Signature Version 4. Use [`AwsKmsClient::with_endpoint`] to
/// target a VPC endpoint, a FIPS endpoint or a local emulator instead of the
/// regional public endpoint.
pub struct AwsKmsClient {
    http: reqwest::Client,
    credentials: AwsCredentials,
    region: String,
    endpoint: String,
    host: String,
}

impl AwsKmsClient {
    /// A client for the regional KMS endpoint of `region`.
    pub fn for_region(
        credentials: AwsCredentials,
        region: impl Into<String>,
    ) -> Result<Self, KmsError> {
        let region = region.into();
        let endpoint = format!("https://kms.{region}.amazonaws.com");
        Self::with_endpoint(credentials, region, endpoint)
    }

    /// A client for an explicit endpoint.
    pub fn with_endpoint(
        credentials: AwsCredentials,
        region: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Result<Self, KmsError> {
        let endpoint = endpoint.into();
        let parsed = url::Url::parse(&endpoint)
            .map_err(|e| KmsError::Config(format!("invalid KMS endpoint {endpoint:?}: {e}")))?;
        let host = match (parsed.host_str(), parsed.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_string(),
            (None, _) => {
                return Err(KmsError::Config(format!(
                    "KMS endpoint {endpoint:?} has no host"
                )));
            }
        };
        Ok(Self {
            http: reqwest::Client::new(),
            credentials,
            region: region.into(),
            endpoint,
            host,
        })
    }

    /// A client for the region named by `AWS_REGION` (or
    /// `AWS_DEFAULT_REGION`), with credentials from the standard AWS
    /// environment variables.
    pub fn from_env() -> Result<Self, KmsError> {
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .map_err(|_| KmsError::Config("AWS_REGION is not set".to_string()))?;
        Self::for_region(AwsCredentials::from_env()?, region)
    }

    /// Sends a signed JSON request to the KMS endpoint.
    async fn call(&self, target: &str, body: Value) -> Result<Value, KmsError> {
        let body = serde_json::to_string(&body)?;
        let headers = self.signed_headers(target, &body);

        let mut request = self.http.post(self.endpoint.as_str()).body(body);
        for (name, value) in headers {
            request = request.header(name, value);
        }

        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(service_error(status.as_u16(), &text));
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// The SigV4 headers for a KMS JSON request.
    fn signed_headers(&self, target: &str, body: &str) -> Vec<(&'static str, String)> {
        self.signed_headers_at(target, body, Utc::now())
    }

    /// [`signed_headers`](Self::signed_headers) with an explicit clock, so the
    /// signature is reproducible.
    fn signed_headers_at(
        &self,
        target: &str,
        body: &str,
        now: DateTime<Utc>,
    ) -> Vec<(&'static str, String)> {
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let region = self.region.as_str();
        let scope = format!("{date}/{region}/{KMS_SERVICE}/aws4_request");

        // Canonical headers must be lowercased and sorted by name. With a
        // session token present the order is content-type, host, x-amz-date,
        // x-amz-security-token, x-amz-target.
        let mut canonical: Vec<(&str, &str)> = vec![
            ("content-type", KMS_CONTENT_TYPE),
            ("host", self.host.as_str()),
            ("x-amz-date", amz_date.as_str()),
        ];
        if let Some(token) = self.credentials.session_token() {
            canonical.push(("x-amz-security-token", token));
        }
        canonical.push(("x-amz-target", target));

        let canonical_headers: String = canonical
            .iter()
            .map(|(name, value)| format!("{name}:{value}\n"))
            .collect();
        let signed_headers = canonical
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(";");
        let payload_hash = hex::encode(Sha256::digest(body.as_bytes()));
        let canonical_request =
            format!("POST\n/\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
        let request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{request_hash}");

        let signing_key = derive_signing_key(
            self.credentials.secret_access_key(),
            &date,
            region,
            KMS_SERVICE,
        );
        let mut signature = hmac_sha256(&signing_key[..], string_to_sign.as_bytes());
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={}",
            self.credentials.access_key_id(),
            hex::encode(signature)
        );
        signature.zeroize();

        let mut headers = vec![
            ("content-type", KMS_CONTENT_TYPE.to_string()),
            ("x-amz-date", amz_date),
            ("x-amz-target", target.to_string()),
        ];
        if let Some(token) = self.credentials.session_token() {
            headers.push(("x-amz-security-token", token.to_string()));
        }
        headers.push(("authorization", authorization));
        headers
    }
}

#[async_trait]
impl KmsClient for AwsKmsClient {
    async fn sign(&self, request: KmsSignRequest) -> Result<KmsSignResponse, KmsError> {
        let key_id = request.key_id.clone();
        let signing_algorithm = request.signing_algorithm;
        let body = sign_request_body(&request);
        let response = self.call(KMS_SIGN_TARGET, body).await?;
        parse_sign_response(&key_id, signing_algorithm, &response)
    }

    async fn public_key(&self, key_id: &str) -> Result<Vec<u8>, KmsError> {
        let body = json!({ "KeyId": key_id });
        let response = self.call(KMS_GET_PUBLIC_KEY_TARGET, body).await?;
        Ok(parse_public_key_response(key_id, &response)?.to_vec())
    }
}

/// The JSON body of a KMS `Sign` request.
fn sign_request_body(request: &KmsSignRequest) -> Value {
    let mut body = Map::new();
    body.insert("KeyId".to_string(), Value::String(request.key_id.clone()));
    body.insert(
        "Message".to_string(),
        Value::String(BASE64.encode(request.message.as_slice())),
    );
    body.insert(
        "MessageType".to_string(),
        Value::String(request.message_type.as_aws_str().to_string()),
    );
    body.insert(
        "SigningAlgorithm".to_string(),
        Value::String(request.signing_algorithm.as_aws_str().to_string()),
    );
    if !request.grant_tokens.is_empty() {
        let grants = request
            .grant_tokens
            .iter()
            .cloned()
            .map(Value::String)
            .collect();
        body.insert("GrantTokens".to_string(), Value::Array(grants));
    }
    Value::Object(body)
}

/// Reads a KMS `Sign` response.
fn parse_sign_response(
    key_id: &str,
    signing_algorithm: SigningAlgorithm,
    response: &Value,
) -> Result<KmsSignResponse, KmsError> {
    if let Some(algorithm) = response.get("SigningAlgorithm").and_then(Value::as_str) {
        if algorithm != signing_algorithm.as_aws_str() {
            return Err(KmsError::UnexpectedAlgorithm(algorithm.to_string()));
        }
    }
    let encoded = response
        .get("Signature")
        .and_then(Value::as_str)
        .ok_or(KmsError::MissingField("Signature"))?;
    let signature = BASE64.decode(encoded).map_err(|source| KmsError::Base64 {
        field: "Signature",
        source,
    })?;
    Ok(KmsSignResponse {
        key_id: key_id.to_string(),
        signature,
        signing_algorithm,
    })
}

/// Reads a KMS `GetPublicKey` response and extracts the raw Ed25519 key.
fn parse_public_key_response(key_id: &str, response: &Value) -> Result<[u8; 32], KmsError> {
    let encoded = response
        .get("PublicKey")
        .and_then(Value::as_str)
        .ok_or(KmsError::MissingField("PublicKey"))?;
    let der = BASE64.decode(encoded).map_err(|source| KmsError::Base64 {
        field: "PublicKey",
        source,
    })?;
    ed25519_public_key_from_spki(&der).ok_or_else(|| KmsError::PublicKeyEncoding {
        key_id: key_id.to_string(),
        got: der.len(),
    })
}

/// Extracts the raw 32-byte Ed25519 public key from a DER-encoded
/// `SubjectPublicKeyInfo`, the encoding AWS KMS returns from `GetPublicKey`.
///
/// The Ed25519 encoding is a fixed 12-byte prefix followed by the key, so this
/// only has to check the prefix instead of pulling in a DER parser.
pub fn ed25519_public_key_from_spki(der: &[u8]) -> Option<[u8; 32]> {
    let prefix = ED25519_SPKI_PREFIX.len();
    if der.len() != prefix + 32 || der[..prefix] != ED25519_SPKI_PREFIX[..] {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&der[prefix..]);
    Some(key)
}

/// Maps an unsuccessful KMS HTTP response to a [`KmsError`].
fn service_error(status: u16, body: &str) -> KmsError {
    #[derive(serde::Deserialize)]
    struct AwsError {
        #[serde(rename = "__type")]
        kind: Option<String>,
        message: Option<String>,
    }

    match serde_json::from_str::<AwsError>(body) {
        Ok(error) => KmsError::Rejected {
            kind: error.kind.unwrap_or_else(|| status.to_string()),
            message: error
                .message
                .unwrap_or_else(|| truncate(body, MAX_ERROR_BODY)),
        },
        Err(_) => KmsError::Service {
            status,
            body: truncate(body, MAX_ERROR_BODY),
        },
    }
}

/// Truncates a response body so a chatty endpoint cannot fill the logs.
fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

// ---------------------------------------------------------------------------
// AWS Signature Version 4
// ---------------------------------------------------------------------------

/// HMAC-SHA-256 (RFC 2104).
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_LENGTH: usize = 64;

    let mut key_block = Zeroizing::new([0u8; BLOCK_LENGTH]);
    if key.len() > BLOCK_LENGTH {
        key_block[..32].copy_from_slice(&Sha256::digest(key)[..]);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = Zeroizing::new([0x36u8; BLOCK_LENGTH]);
    let mut outer_pad = Zeroizing::new([0x5cu8; BLOCK_LENGTH]);
    for index in 0..BLOCK_LENGTH {
        inner_pad[index] ^= key_block[index];
        outer_pad[index] ^= key_block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad.as_slice());
    inner.update(message);
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad.as_slice());
    outer.update(&inner[..]);
    outer.finalize().into()
}

/// Derives the SigV4 signing key by running the request date, region and
/// service through the AWS4 HMAC chain.
fn derive_signing_key(
    secret_access_key: &str,
    date: &str,
    region: &str,
    service: &str,
) -> Zeroizing<[u8; 32]> {
    let capacity = AWS4_PREFIX.len() + secret_access_key.len();
    let mut prefixed = Zeroizing::new(Vec::with_capacity(capacity));
    prefixed.extend_from_slice(AWS4_PREFIX);
    prefixed.extend_from_slice(secret_access_key.as_bytes());

    let mut key = hmac_sha256(prefixed.as_slice(), date.as_bytes());
    let mut next = hmac_sha256(&key, region.as_bytes());
    key.zeroize();
    key = next;
    next.zeroize();
    next = hmac_sha256(&key, service.as_bytes());
    key.zeroize();
    key = next;
    next.zeroize();
    next = hmac_sha256(&key, b"aws4_request");
    key.zeroize();
    Zeroizing::new(next)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use chrono::TimeZone;
    use stellar_xdr::curr::{
        FeeBumpTransaction, FeeBumpTransactionEnvelope, FeeBumpTransactionExt,
        FeeBumpTransactionInnerTx, Memo, MuxedAccount, Operation, Preconditions, SequenceNumber,
        TransactionExt, TransactionV0, TransactionV0Envelope, TransactionV0Ext,
        TransactionV1Envelope, Uint256, VecM,
    };

    use super::*;

    const KEY_ID: &str = "alias/tessera-dividends";

    /// 32 distinct bytes, so the last four (the Stellar signature hint) are easy
    /// to assert on.
    const PUBLIC_KEY: [u8; 32] = [
        0x02, 0x03, 0x05, 0x07, 0x0b, 0x0d, 0x11, 0x13, 0x17, 0x19, 0x1d, 0x1f, 0x23, 0x25, 0x29,
        0x2b, 0x2d, 0x2f, 0x31, 0x35, 0x37, 0x3b, 0x3d, 0x41, 0x43, 0x47, 0x49, 0x4d, 0x4f, 0x53,
        0x59, 0x61,
    ];

    const SEED: [u8; 32] = [0x5a; 32];

    fn no_operations() -> VecM<Operation, 100> {
        Vec::new().try_into().expect("no operations is valid")
    }

    fn sample_transaction() -> Transaction {
        Transaction {
            source_account: MuxedAccount::Ed25519(Uint256([0x11; 32])),
            fee: 100,
            seq_num: SequenceNumber(42),
            cond: Preconditions::None,
            memo: Memo::None,
            operations: no_operations(),
            ext: TransactionExt::V0,
        }
    }

    fn sample_envelope() -> TransactionEnvelope {
        TransactionEnvelope::Tx(TransactionV1Envelope {
            tx: sample_transaction(),
            signatures: VecM::default(),
        })
    }

    fn sample_v0_envelope() -> TransactionEnvelope {
        TransactionEnvelope::TxV0(TransactionV0Envelope {
            tx: TransactionV0 {
                source_account_ed25519: Uint256([0x22; 32]),
                fee: 100,
                seq_num: SequenceNumber(1),
                time_bounds: None,
                memo: Memo::None,
                operations: no_operations(),
                ext: TransactionV0Ext::V0,
            },
            signatures: VecM::default(),
        })
    }

    fn sample_fee_bump_envelope() -> TransactionEnvelope {
        TransactionEnvelope::TxFeeBump(FeeBumpTransactionEnvelope {
            tx: FeeBumpTransaction {
                fee_source: MuxedAccount::Ed25519(Uint256([0x33; 32])),
                fee: 200,
                inner_tx: FeeBumpTransactionInnerTx::Tx(TransactionV1Envelope {
                    tx: sample_transaction(),
                    signatures: VecM::default(),
                }),
                ext: FeeBumpTransactionExt::V0,
            },
            signatures: VecM::default(),
        })
    }

    fn signature_bytes(envelope: &TransactionEnvelope) -> Vec<u8> {
        match envelope {
            TransactionEnvelope::Tx(v1) => v1.signatures[0].signature.0.to_vec(),
            TransactionEnvelope::TxFeeBump(fee_bump) => fee_bump.signatures[0].signature.0.to_vec(),
            TransactionEnvelope::TxV0(v0) => v0.signatures[0].signature.0.to_vec(),
        }
    }

    /// Deterministic stand-in for an HSM.
    ///
    /// It is *not* Ed25519: it derives a 64-byte MAC from a per-key seed so the
    /// tests can drive the whole `KmsClient` boundary — including the exact
    /// bytes the signer hands over, and a real verify step — without AWS
    /// credentials. Production signs through a real HSM; this exists so the
    /// module's request construction, digest handling and signature assembly are
    /// exercised end to end.
    #[derive(Debug, Default)]
    struct MockKms {
        keys: HashMap<String, ([u8; 32], [u8; 32])>,
        captured: Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl MockKms {
        fn new() -> Self {
            Self::default()
        }

        fn with_key(mut self, key_id: &str, seed: [u8; 32], public_key: [u8; 32]) -> Self {
            self.keys.insert(key_id.to_string(), (seed, public_key));
            self
        }

        /// Every `(key_id, message)` pair the signer has handed over.
        fn captured(&self) -> Vec<(String, Vec<u8>)> {
            self.captured
                .lock()
                .expect("mock kms lock is not poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl KmsClient for MockKms {
        async fn sign(&self, request: KmsSignRequest) -> Result<KmsSignResponse, KmsError> {
            let (seed, _) =
                self.keys.get(&request.key_id).copied().ok_or_else(|| {
                    KmsError::Config(format!("unknown mock key {}", request.key_id))
                })?;
            let signature = mock_signature(&seed, request.message.as_slice());
            self.captured
                .lock()
                .expect("mock kms lock is not poisoned")
                .push((request.key_id.clone(), request.message.as_slice().to_vec()));
            Ok(KmsSignResponse {
                key_id: request.key_id.clone(),
                signature: signature.to_vec(),
                signing_algorithm: request.signing_algorithm,
            })
        }

        async fn public_key(&self, key_id: &str) -> Result<Vec<u8>, KmsError> {
            let (_, public_key) = self
                .keys
                .get(key_id)
                .copied()
                .ok_or_else(|| KmsError::Config(format!("unknown mock key {key_id}")))?;
            Ok(public_key.to_vec())
        }
    }

    /// A KMS that returns a public key that is not an Ed25519 key.
    struct ShortPublicKeyKms;

    #[async_trait]
    impl KmsClient for ShortPublicKeyKms {
        async fn sign(&self, _request: KmsSignRequest) -> Result<KmsSignResponse, KmsError> {
            Ok(KmsSignResponse {
                key_id: KEY_ID.to_string(),
                signature: vec![0u8; SIGNATURE_LENGTH],
                signing_algorithm: SigningAlgorithm::Ed25519,
            })
        }

        async fn public_key(&self, _key_id: &str) -> Result<Vec<u8>, KmsError> {
            Ok(vec![0u8; 5])
        }
    }

    /// A KMS that returns a signature that is not an Ed25519 signature.
    struct ShortSignatureKms;

    #[async_trait]
    impl KmsClient for ShortSignatureKms {
        async fn sign(&self, _request: KmsSignRequest) -> Result<KmsSignResponse, KmsError> {
            Ok(KmsSignResponse {
                key_id: KEY_ID.to_string(),
                signature: vec![0u8; 32],
                signing_algorithm: SigningAlgorithm::Ed25519,
            })
        }

        async fn public_key(&self, _key_id: &str) -> Result<Vec<u8>, KmsError> {
            Ok(vec![0u8; 32])
        }
    }

    fn mock_signature(seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
        let tag = hmac_sha256(seed, message);
        let mut second_input = Vec::with_capacity(tag.len() + message.len());
        second_input.extend_from_slice(&tag);
        second_input.extend_from_slice(message);
        let check = hmac_sha256(seed, &second_input);

        let mut signature = [0u8; 64];
        signature[..32].copy_from_slice(&tag);
        signature[32..].copy_from_slice(&check);
        signature
    }

    fn mock_verify(seed: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
        if signature.len() != SIGNATURE_LENGTH {
            return false;
        }
        let expected = mock_signature(seed, message);
        let mut difference = 0u8;
        for (expected, actual) in expected.iter().zip(signature.iter()) {
            difference |= expected ^ actual;
        }
        difference == 0
    }

    #[test]
    fn network_id_matches_the_stellar_testnet_and_mainnet_vectors() {
        assert_eq!(
            hex::encode(network_id(TESTNET_PASSPHRASE)),
            "cee0302d59844d32bdca915c8203dd44b33fbb7edc19051ea37abedf28ecd472"
        );
        assert_eq!(
            hex::encode(network_id(MAINNET_PASSPHRASE)),
            "7ac33997544e3175d266bd022439b22cdb16508c01163f26e5cb2a3e1045a979"
        );
    }

    #[test]
    fn envelope_digest_matches_the_stellar_xdr_transaction_hash() {
        // Independent of the implementation under test, `stellar-xdr` computes
        // the same digest from the same envelope.
        let stellar_network_id = network_id(TESTNET_PASSPHRASE);
        for envelope in [sample_envelope(), sample_fee_bump_envelope()] {
            assert_eq!(
                envelope_digest(&envelope, TESTNET_PASSPHRASE).expect("digest"),
                envelope.hash(stellar_network_id).expect("hash"),
                "digest must match the SHA-256 of the signature payload"
            );
        }
    }

    #[test]
    fn legacy_v0_envelopes_cannot_be_signed() {
        let result = envelope_digest(&sample_v0_envelope(), TESTNET_PASSPHRASE);
        assert!(matches!(result, Err(SignerError::LegacyEnvelope)));
    }

    #[tokio::test]
    async fn sign_envelope_produces_a_signature_the_hsm_can_verify() {
        let signer = KmsSigner::new(
            MockKms::new().with_key(KEY_ID, SEED, PUBLIC_KEY),
            KEY_ID,
            PUBLIC_KEY,
        );
        let envelope = sample_envelope();
        let signed = signer
            .sign_envelope(&envelope, TESTNET_PASSPHRASE)
            .await
            .expect("signing succeeds");

        // The envelope that was handed in is not modified.
        let TransactionEnvelope::Tx(unsigned) = &envelope else {
            panic!("the sample envelope is a v1 envelope");
        };
        assert!(unsigned.signatures.is_empty());

        let TransactionEnvelope::Tx(v1) = &signed else {
            panic!("a v1 envelope is signed as a v1 envelope");
        };
        assert_eq!(v1.signatures.len(), 1);
        assert_eq!(v1.signatures[0].hint.0, [0x4f, 0x53, 0x59, 0x61]);

        // Appending a signature does not change what was signed, and the HSM
        // can verify the signature it produced over that digest.
        let digest = envelope_digest(&signed, TESTNET_PASSPHRASE).expect("digest");
        assert_eq!(
            digest,
            envelope_digest(&envelope, TESTNET_PASSPHRASE).expect("digest")
        );
        assert!(mock_verify(
            &SEED,
            &digest,
            &v1.signatures[0].signature.0.to_vec()
        ));

        // The only thing that crossed the HSM boundary is the 32-byte digest.
        let captured = signer.client().captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, KEY_ID);
        assert_eq!(captured[0].1, digest.to_vec());
        assert_eq!(captured[0].1.len(), 32);
    }

    #[tokio::test]
    async fn fee_bump_envelopes_are_signed_over_the_fee_bump_transaction() {
        let signer = KmsSigner::new(
            MockKms::new().with_key(KEY_ID, SEED, PUBLIC_KEY),
            KEY_ID,
            PUBLIC_KEY,
        );
        let envelope = sample_fee_bump_envelope();
        let signed = signer
            .sign_envelope(&envelope, TESTNET_PASSPHRASE)
            .await
            .expect("signing succeeds");

        let TransactionEnvelope::TxFeeBump(fee_bump) = &signed else {
            panic!("a fee bump envelope is signed as a fee bump envelope");
        };
        assert_eq!(fee_bump.signatures.len(), 1);

        let digest = envelope_digest(&signed, TESTNET_PASSPHRASE).expect("digest");
        assert!(mock_verify(
            &SEED,
            &digest,
            &fee_bump.signatures[0].signature.0.to_vec()
        ));
    }

    #[tokio::test]
    async fn signatures_are_bound_to_the_network_passphrase() {
        let signer = KmsSigner::new(
            MockKms::new().with_key(KEY_ID, SEED, PUBLIC_KEY),
            KEY_ID,
            PUBLIC_KEY,
        );
        let envelope = sample_envelope();
        let testnet = signer
            .sign_envelope(&envelope, TESTNET_PASSPHRASE)
            .await
            .expect("signing succeeds");
        let mainnet = signer
            .sign_envelope(&envelope, MAINNET_PASSPHRASE)
            .await
            .expect("signing succeeds");

        assert_ne!(signature_bytes(&testnet), signature_bytes(&mainnet));
        assert_ne!(
            envelope_digest(&envelope, TESTNET_PASSPHRASE).expect("digest"),
            envelope_digest(&envelope, MAINNET_PASSPHRASE).expect("digest")
        );
    }

    #[tokio::test]
    async fn from_kms_fetches_the_public_key() {
        let signer = KmsSigner::from_kms(MockKms::new().with_key(KEY_ID, SEED, PUBLIC_KEY), KEY_ID)
            .await
            .expect("the mock key exists");
        assert_eq!(*signer.public_key(), PUBLIC_KEY);
        assert_eq!(
            signer.signature_hint().0,
            [
                PUBLIC_KEY[28],
                PUBLIC_KEY[29],
                PUBLIC_KEY[30],
                PUBLIC_KEY[31]
            ]
        );

        let Err(error) = KmsSigner::from_kms(ShortPublicKeyKms, KEY_ID).await else {
            panic!("a 5-byte public key must be rejected");
        };
        assert!(matches!(error, SignerError::InvalidPublicKey(5)));
    }

    #[tokio::test]
    async fn signer_rejects_a_signature_that_is_not_64_bytes() {
        let signer = KmsSigner::new(ShortSignatureKms, KEY_ID, [0u8; 32]);
        let result = signer.sign_digest(&[0u8; 32]).await;
        assert!(matches!(result, Err(SignerError::InvalidSignature(32))));
    }

    #[tokio::test]
    async fn signer_refuses_to_overflow_the_signature_slot() {
        let signer = KmsSigner::new(
            MockKms::new().with_key(KEY_ID, SEED, PUBLIC_KEY),
            KEY_ID,
            PUBLIC_KEY,
        );
        let mut envelope = sample_envelope();
        let TransactionEnvelope::Tx(v1) = &mut envelope else {
            panic!("the sample envelope is a v1 envelope");
        };
        let full: Vec<DecoratedSignature> = (0..MAX_SIGNATURES)
            .map(|_| DecoratedSignature {
                hint: SignatureHint([0u8; 4]),
                signature: Signature::from(
                    BytesM::<64>::try_from(vec![0u8; SIGNATURE_LENGTH]).expect("64 bytes fits"),
                ),
            })
            .collect();
        v1.signatures = full.try_into().expect("20 signatures fit");

        let result = signer.sign_envelope(&envelope, TESTNET_PASSPHRASE).await;
        assert!(matches!(result, Err(SignerError::TooManySignatures)));
    }

    #[test]
    fn secret_bytes_scrub_their_buffer_in_place() {
        let mut secret = SecretBytes::new(vec![0xa5; 32]);
        assert_eq!(secret.as_slice(), &[0xa5; 32]);

        // `Drop` calls exactly this method, so the scrub also runs when the
        // buffer is released. The assertion reads a live allocation on purpose:
        // reading memory after the allocation is freed is unsound, because the
        // allocator is allowed to recycle it (glibc's tcache, for one, writes
        // its free-list pointers into a freed chunk immediately).
        secret.zeroize();

        assert_eq!(secret.as_slice(), &[0u8; 32]);
        assert_eq!(
            secret.len(),
            32,
            "the scrub overwrites, it does not truncate"
        );
    }

    #[test]
    fn the_digest_handed_to_the_hsm_is_the_only_message_and_is_scrubbable() {
        let mut request = KmsSignRequest::ed25519_digest(KEY_ID, &[0xa5; 32]);
        assert_eq!(request.message.as_slice(), &[0xa5; 32]);
        assert_eq!(request.message.len(), 32);
        assert_eq!(request.message_type, MessageType::Raw);
        assert_eq!(request.signing_algorithm, SigningAlgorithm::Ed25519);
        assert!(request.grant_tokens.is_empty());

        // The request owns the message in a `SecretBytes`, so releasing it
        // scrubs the digest; scrubbing it up front is the same call.
        request.message.zeroize();
        assert_eq!(request.message.as_slice(), &[0u8; 32]);
    }

    #[test]
    fn sigv4_signing_key_matches_the_aws_documented_example() {
        // From the SigV4 documentation: the derived key for the example secret,
        // 2012-02-15, us-east-1, iam.
        let key = derive_signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20120215",
            "us-east-1",
            "iam",
        );
        assert_eq!(
            hex::encode(&key[..]),
            "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d"
        );
    }

    fn authorization_signature(headers: &[(&'static str, String)]) -> String {
        let authorization = headers
            .iter()
            .find(|(name, _)| *name == "authorization")
            .map(|(_, value)| value.clone())
            .expect("an authorization header");
        authorization
            .rsplit_once("Signature=")
            .expect("a Signature component")
            .1
            .to_string()
    }

    #[test]
    fn sigv4_authorization_covers_the_target_and_the_body() {
        let client = AwsKmsClient::for_region(
            AwsCredentials::new("AKIDEXAMPLE", "secret").with_session_token("token"),
            "us-east-1",
        )
        .expect("the regional endpoint is valid");
        let at = Utc
            .timestamp_opt(1_700_000_000, 0)
            .expect("a valid timestamp");

        let signed = client.signed_headers_at(KMS_SIGN_TARGET, "{\"KeyId\":\"a\"}", at);
        let again = client.signed_headers_at(KMS_SIGN_TARGET, "{\"KeyId\":\"a\"}", at);
        assert_eq!(signed, again, "signing is deterministic for a fixed clock");

        let amz_date = signed
            .iter()
            .find(|(name, _)| *name == "x-amz-date")
            .expect("an x-amz-date header")
            .1
            .clone();
        assert_eq!(amz_date, "20231114T221320Z");

        let authorization = signed
            .iter()
            .find(|(name, _)| *name == "authorization")
            .expect("an authorization header")
            .1
            .clone();
        assert!(authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"));
        assert!(authorization.contains("/20231114/us-east-1/kms/aws4_request"));
        assert!(authorization.contains(
            "SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target"
        ));
        assert!(signed
            .iter()
            .any(|(name, value)| *name == "x-amz-target" && value == KMS_SIGN_TARGET));
        assert!(signed
            .iter()
            .any(|(name, value)| *name == "host" && value == "kms.us-east-1.amazonaws.com"));
        assert!(signed
            .iter()
            .any(|(name, value)| *name == "x-amz-security-token" && value == "token"));

        // The body is part of the signed payload, so changing it changes the
        // signature.
        let other = client.signed_headers_at(KMS_SIGN_TARGET, "{\"KeyId\":\"b\"}", at);
        assert_ne!(
            authorization_signature(&signed),
            authorization_signature(&other)
        );
    }

    #[test]
    fn aws_client_derives_the_regional_endpoint_and_host() {
        let credentials = AwsCredentials::new("AKID", "SECRET");
        let client =
            AwsKmsClient::for_region(credentials.clone(), "eu-west-1").expect("valid endpoint");
        assert_eq!(client.endpoint, "https://kms.eu-west-1.amazonaws.com");
        assert_eq!(client.host, "kms.eu-west-1.amazonaws.com");

        let local = AwsKmsClient::with_endpoint(credentials, "us-east-1", "http://localhost:4566")
            .expect("valid endpoint");
        assert_eq!(local.host, "localhost:4566");

        let broken =
            AwsKmsClient::with_endpoint(AwsCredentials::new("a", "b"), "us-east-1", "nope");
        assert!(matches!(broken, Err(KmsError::Config(_))));
    }

    #[test]
    fn credentials_are_redacted_in_debug_output() {
        let credentials =
            AwsCredentials::new("AKIDEXAMPLE", "super-secret").with_session_token("session-secret");
        let rendered = format!("{credentials:?}");
        assert!(rendered.contains("AKIDEXAMPLE"));
        assert!(!rendered.contains("super-secret"));
        assert!(!rendered.contains("session-secret"));
    }

    #[test]
    fn kms_sign_request_body_matches_the_documented_shape() {
        let request = KmsSignRequest::ed25519_digest(KEY_ID, &[0x01; 32]);
        let body = sign_request_body(&request);
        assert_eq!(body["KeyId"], KEY_ID);
        assert_eq!(body["Message"], BASE64.encode([0x01u8; 32]));
        assert_eq!(body["MessageType"], "RAW");
        assert_eq!(body["SigningAlgorithm"], "ED25519");
        assert!(body.get("GrantTokens").is_none());

        let mut with_grants = request;
        with_grants.grant_tokens.push("grant".to_string());
        let body = sign_request_body(&with_grants);
        assert_eq!(body["GrantTokens"], json!(["grant"]));
    }

    #[test]
    fn kms_sign_and_public_key_responses_are_parsed() {
        let signature = vec![0x7fu8; SIGNATURE_LENGTH];
        let response = json!({
            "KeyId": KEY_ID,
            "Signature": BASE64.encode(&signature),
            "SigningAlgorithm": "ED25519",
        });
        let parsed = parse_sign_response(KEY_ID, SigningAlgorithm::Ed25519, &response)
            .expect("a well-formed response");
        assert_eq!(parsed.signature, signature);
        assert_eq!(parsed.key_id, KEY_ID);

        let wrong_algorithm = json!({
            "Signature": BASE64.encode(&signature),
            "SigningAlgorithm": "RSASSA_PSS_SHA_256",
        });
        let result = parse_sign_response(KEY_ID, SigningAlgorithm::Ed25519, &wrong_algorithm);
        assert!(matches!(result, Err(KmsError::UnexpectedAlgorithm(_))));

        let missing = json!({ "SigningAlgorithm": "ED25519" });
        let result = parse_sign_response(KEY_ID, SigningAlgorithm::Ed25519, &missing);
        assert!(matches!(result, Err(KmsError::MissingField("Signature"))));

        let mut der = ED25519_SPKI_PREFIX.to_vec();
        let public_key: Vec<u8> = (0u8..32).collect();
        der.extend_from_slice(&public_key);
        let response = json!({ "PublicKey": BASE64.encode(&der) });
        let parsed = parse_public_key_response(KEY_ID, &response).expect("a well-formed response");
        assert_eq!(parsed.to_vec(), public_key);

        let not_a_key = json!({ "PublicKey": BASE64.encode([0u8; 10]) });
        let result = parse_public_key_response(KEY_ID, &not_a_key);
        assert!(matches!(result, Err(KmsError::PublicKeyEncoding { .. })));
    }

    #[test]
    fn service_errors_are_decoded_and_truncated() {
        let error = service_error(
            400,
            r#"{"__type":"NotFoundException","message":"no such key"}"#,
        );
        assert!(matches!(
            error,
            KmsError::Rejected { ref kind, ref message }
                if kind == "NotFoundException" && message == "no such key"
        ));

        let error = service_error(500, "upstream is unhappy");
        assert!(matches!(error, KmsError::Service { status: 500, .. }));

        let long_body = "x".repeat(MAX_ERROR_BODY + 10);
        let error = service_error(500, &long_body);
        let KmsError::Service { body, .. } = error else {
            panic!("a non-JSON body is reported verbatim");
        };
        assert_eq!(body.chars().count(), MAX_ERROR_BODY);
    }
}
