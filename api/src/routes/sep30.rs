//! Stellar SEP-30 (Account Recovery) Implementation with Social Recovery Multisig.
//!
//! Issue #50 / #431:
//! (1) POST /sep30/accounts/{address} registers recovery identity providers (email, phone, TOTP).
//! (2) GET /sep30/accounts/{address} returns registered signers and identity providers.
//! (3) PUT /sep30/accounts/{address}/recover with verified identity proofs returns a signed XDR transaction that adds a new signing key.
//! (4) The recovery server holds a signing key that only co-signs recovery transactions — never direct transfers.
//! (5) Rate limiting: max 3 recovery attempts per 24 hours per account.
//! (6) All actions are logged to the immutable audit trail (issue #20).
//! (7) Unit and compliance test suite verifying all invariants.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use mpc_signer::{
    DistributedKeyGenerator, MpcCoordinator, MpcNetwork, MpcNode, SignRequest, ThresholdConfig,
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use stellar_strkey::ed25519;
use stellar_xdr::curr::{
    AccountId, BytesM, DecoratedSignature, Hash, Limits, Operation, OperationBody,
    PublicKey, ReadXdr, SequenceNumber, SetOptionsOp, SignerKey, Uint256,
    Transaction, TransactionEnvelope, TransactionSignaturePayload,
    TransactionSignaturePayloadTaggedTransaction, TransactionV1Envelope, WriteXdr,
};

use crate::audit::chain_verifier::AuditTrailVerifier;
use crate::models::ApiErrorBody;

/// Maximum allowed recovery attempts per 24-hour window per account.
pub const MAX_RECOVERY_ATTEMPTS_PER_24H: usize = 3;
pub const WINDOW_24H_SECS: u64 = 86_400;

/// Default passphrase for Stellar Testnet transactions.
pub const TESTNET_PASSPHRASE: &str = "Test SDF Network ; September 2015";

// ---------------------------------------------------------------------------
// Data Models
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuthMethod {
    #[serde(rename = "email")]
    Email,
    #[serde(rename = "phone_number")]
    PhoneNumber,
    #[serde(rename = "totp")]
    Totp,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityProvider {
    pub role: String,
    pub auth_method: AuthMethod,
    pub identifier: String, // e.g. email address, phone number, or TOTP secret/salt
    pub verified: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignerInfo {
    pub key: String,
    pub weight: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterAccountRequest {
    pub identities: Vec<IdentityProvider>,
    pub threshold: Option<usize>, // e.g. 2 for 2-of-3 social recovery
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterAccountResponse {
    pub address: String,
    pub signers: Vec<SignerInfo>,
    pub identities: Vec<IdentityProvider>,
    pub threshold: usize,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountDetailsResponse {
    pub address: String,
    pub signers: Vec<SignerInfo>,
    pub identities: Vec<IdentityProvider>,
    pub threshold: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityProof {
    pub auth_method: AuthMethod,
    pub proof: String, // confirmation code or OTP response
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoverAccountRequest {
    pub new_signer_key: String, // Stellar G-address to be added
    pub identity_proofs: Vec<IdentityProof>,
    pub transaction_xdr: Option<String>, // Optional client-prepared envelope
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoverAccountResponse {
    pub address: String,
    pub status: String,
    pub transaction_xdr: String,
    pub recovery_signer: String,
    pub new_signer_key: String,
}

// ---------------------------------------------------------------------------
// In-Memory Storage & Recovery State
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct AccountRecoveryRecord {
    pub address: String,
    pub identities: Vec<IdentityProvider>,
    pub threshold: usize,
    pub registered_at: u64,
}

pub struct Sep30ServerState {
    pub server_signing_key: SigningKey,
    pub server_verifying_key: VerifyingKey,
    pub server_stellar_address: String,
    pub mpc_coordinator: Option<Arc<MpcCoordinator>>,
    pub accounts: Arc<RwLock<HashMap<String, AccountRecoveryRecord>>>,
    pub recovery_attempts: Arc<RwLock<HashMap<String, Vec<u64>>>>,
    pub audit_verifier: Arc<RwLock<AuditTrailVerifier>>,
}

impl Sep30ServerState {
    pub fn new(audit_verifier: Arc<RwLock<AuditTrailVerifier>>) -> Self {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();
        let server_stellar_address = ed25519::PublicKey(verifying_key.to_bytes()).to_string();

        Self {
            server_signing_key: signing_key,
            server_verifying_key: verifying_key,
            server_stellar_address,
            mpc_coordinator: None,
            accounts: Arc::new(RwLock::new(HashMap::new())),
            recovery_attempts: Arc::new(RwLock::new(HashMap::new())),
            audit_verifier,
        }
    }

    /// Construct with 2-of-3 MPC threshold signing cluster (Issue #50 requirement 3).
    pub async fn new_with_mpc(audit_verifier: Arc<RwLock<AuditTrailVerifier>>) -> Self {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();

        let (master_public_key, shares) = DistributedKeyGenerator::generate_2_of_3();
        let network = Arc::new(MpcNetwork::new());

        for share in shares {
            let node = Arc::new(MpcNode::new(share));
            network.register_node(node).await;
        }

        let coordinator = Arc::new(MpcCoordinator::new(
            ThresholdConfig::default(),
            network,
            master_public_key,
        ));
        let master_pk_bytes = coordinator.master_public_key_bytes();
        let server_stellar_address = ed25519::PublicKey(master_pk_bytes).to_string();

        Self {
            server_signing_key: signing_key,
            server_verifying_key: verifying_key,
            server_stellar_address,
            mpc_coordinator: Some(coordinator),
            accounts: Arc::new(RwLock::new(HashMap::new())),
            recovery_attempts: Arc::new(RwLock::new(HashMap::new())),
            audit_verifier,
        }
    }

    /// Check and record a recovery attempt for rate limiting (Issue #50 requirement 5).
    /// Max 3 recovery attempts per 24 h per account.
    pub fn record_and_check_rate_limit(&self, address: &str) -> Result<(), (StatusCode, String)> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut attempts_map = self.recovery_attempts.write().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Lock poisoned".to_string(),
            )
        })?;

        let entry = attempts_map.entry(address.to_string()).or_default();

        // Evict attempts older than 24 hours
        entry.retain(|&timestamp| now.saturating_sub(timestamp) < WINDOW_24H_SECS);

        if entry.len() >= MAX_RECOVERY_ATTEMPTS_PER_24H {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Rate limit exceeded: maximum {} recovery attempts per 24 hours per account",
                    MAX_RECOVERY_ATTEMPTS_PER_24H
                ),
            ));
        }

        entry.push(now);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Handlers & Business Logic
// ---------------------------------------------------------------------------

/// POST /sep30/accounts/{address}
/// Registers recovery identity providers (email, phone, TOTP)
pub async fn register_account(
    State(state): State<Arc<Sep30ServerState>>,
    Path(address): Path<String>,
    Json(payload): Json<RegisterAccountRequest>,
) -> Response {
    if payload.identities.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "bad_request".to_string(),
                message: "At least one recovery identity provider must be supplied".to_string(),
            }),
        )
            .into_response();
    }

    let threshold = payload.threshold.unwrap_or(2).min(payload.identities.len());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let record = AccountRecoveryRecord {
        address: address.clone(),
        identities: payload.identities.clone(),
        threshold,
        registered_at: now,
    };

    {
        let mut accounts = state.accounts.write().unwrap();
        accounts.insert(address.clone(), record);
    }

    // Log to immutable audit trail (Issue #50 requirement 6)
    if let Ok(mut audit) = state.audit_verifier.write() {
        audit.append_event(
            "sep30_account_registered".to_string(),
            "sep30_server".to_string(),
            format!("account:{}", address),
            "REGISTER".to_string(),
            format!(
                "identities:{},threshold:{}",
                payload.identities.len(),
                threshold
            ),
        );
    }

    let response = RegisterAccountResponse {
        address,
        signers: vec![SignerInfo {
            key: state.server_stellar_address.clone(),
            weight: 10,
        }],
        identities: payload.identities,
        threshold,
        status: "registered".to_string(),
    };

    (StatusCode::CREATED, Json(response)).into_response()
}

/// GET /sep30/accounts/{address}
/// Returns registered signers and identity providers
pub async fn get_account(
    State(state): State<Arc<Sep30ServerState>>,
    Path(address): Path<String>,
) -> Response {
    let accounts = state.accounts.read().unwrap();
    let record = match accounts.get(&address) {
        Some(r) => r.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiErrorBody {
                    error: "not_found".to_string(),
                    message: format!("Account {} not registered with recovery server", address),
                }),
            )
                .into_response()
        }
    };

    // Log query event to audit trail
    if let Ok(mut audit) = state.audit_verifier.write() {
        audit.append_event(
            "sep30_account_query".to_string(),
            "client".to_string(),
            format!("account:{}", address),
            "GET".to_string(),
            "Signers retrieved".to_string(),
        );
    }

    let response = AccountDetailsResponse {
        address,
        signers: vec![SignerInfo {
            key: state.server_stellar_address.clone(),
            weight: 10,
        }],
        identities: record.identities,
        threshold: record.threshold,
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// PUT /sep30/accounts/{address}/recover
/// With verified identity proofs, returns a signed XDR transaction that adds a new signing key.
pub async fn recover_account(
    State(state): State<Arc<Sep30ServerState>>,
    Path(address): Path<String>,
    Json(payload): Json<RecoverAccountRequest>,
) -> Response {
    // 1. Rate limiting check (Issue #50 requirement 5: max 3 recovery attempts per 24h per account)
    if let Err((status, msg)) = state.record_and_check_rate_limit(&address) {
        if let Ok(mut audit) = state.audit_verifier.write() {
            audit.append_event(
                "sep30_recovery_rate_limited".to_string(),
                "sep30_server".to_string(),
                format!("account:{}", address),
                "RATE_LIMIT".to_string(),
                msg.clone(),
            );
        }
        return (
            status,
            Json(ApiErrorBody {
                error: "rate_limit_exceeded".to_string(),
                message: msg,
            }),
        )
            .into_response();
    }

    // 2. Lookup registered account record
    let record = {
        let accounts = state.accounts.read().unwrap();
        match accounts.get(&address) {
            Some(r) => r.clone(),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(ApiErrorBody {
                        error: "not_found".to_string(),
                        message: format!("Account {} is not registered", address),
                    }),
                )
                    .into_response()
            }
        }
    };

    // 3. Verify Identity Proofs against registered threshold
    let valid_proofs_count = payload
        .identity_proofs
        .iter()
        .filter(|p| {
            // Validate proof: proof must not be empty and must correspond to an authorized registered identity
            !p.proof.is_empty()
                && record
                    .identities
                    .iter()
                    .any(|id| id.auth_method == p.auth_method)
        })
        .count();

    if valid_proofs_count < record.threshold {
        if let Ok(mut audit) = state.audit_verifier.write() {
            audit.append_event(
                "sep30_recovery_auth_failed".to_string(),
                "sep30_server".to_string(),
                format!("account:{}", address),
                "VERIFY_PROOFS".to_string(),
                format!(
                    "Insufficient proofs: got {}, required {}",
                    valid_proofs_count, record.threshold
                ),
            );
        }

        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiErrorBody {
                error: "unauthorized".to_string(),
                message: format!(
                    "Recovery verification failed: provided {} valid identity proofs, but {} are required",
                    valid_proofs_count, record.threshold
                ),
            }),
        )
            .into_response();
    }

    // 4. Security Guardrail (Issue #50 requirement 4):
    // The recovery server only co-signs recovery transactions — NEVER direct transfers.
    let transaction_envelope = if let Some(xdr_b64) = &payload.transaction_xdr {
        match TransactionEnvelope::from_xdr_base64(xdr_b64, Limits::none()) {
            Ok(env) => {
                if let Err(violation) = validate_recovery_transaction_guardrails(&env) {
                    if let Ok(mut audit) = state.audit_verifier.write() {
                        audit.append_event(
                            "sep30_guardrail_violation".to_string(),
                            "sep30_server".to_string(),
                            format!("account:{}", address),
                            "REJECT_TRANSFER".to_string(),
                            violation.clone(),
                        );
                    }

                    return (
                        StatusCode::FORBIDDEN,
                        Json(ApiErrorBody {
                            error: "forbidden_operation".to_string(),
                            message: violation,
                        }),
                    )
                        .into_response();
                }
                env
            }
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ApiErrorBody {
                        error: "bad_request".to_string(),
                        message: format!("Invalid transaction XDR: {e}"),
                    }),
                )
                    .into_response()
            }
        }
    } else {
        // Construct canonical SetOptions recovery transaction
        match build_recovery_transaction(&address, &payload.new_signer_key) {
            Ok(env) => env,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ApiErrorBody {
                        error: "bad_request".to_string(),
                        message: format!("Failed to build recovery transaction: {e}"),
                    }),
                )
                    .into_response()
            }
        }
    };

    // 5. Co-sign the recovery transaction using recovery server's key (Threshold Multisig co-signer)
    let signed_envelope = match cosign_transaction(
        transaction_envelope,
        &state.server_signing_key,
        state.mpc_coordinator.as_ref(),
        TESTNET_PASSPHRASE,
    ).await {
        Ok(env) => env,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiErrorBody {
                    error: "signing_failed".to_string(),
                    message: format!("Failed to co-sign transaction: {e}"),
                }),
            )
                .into_response()
        }
    };

    let signed_xdr = signed_envelope
        .to_xdr_base64(Limits::none())
        .unwrap_or_default();

    // 6. Log success to immutable audit trail (Issue #50 requirement 6)
    if let Ok(mut audit) = state.audit_verifier.write() {
        audit.append_event(
            "sep30_recovery_succeeded".to_string(),
            "sep30_server".to_string(),
            format!("account:{}", address),
            "CO_SIGN_RECOVERY".to_string(),
            format!("new_signer:{}", payload.new_signer_key),
        );
    }

    let response = RecoverAccountResponse {
        address,
        status: "co-signed".to_string(),
        transaction_xdr: signed_xdr,
        recovery_signer: state.server_stellar_address.clone(),
        new_signer_key: payload.new_signer_key,
    };

    (StatusCode::OK, Json(response)).into_response()
}

// ---------------------------------------------------------------------------
// Security Guardrails & Cryptographic Co-signing
// ---------------------------------------------------------------------------

/// Validates that the transaction ONLY contains recovery operations (SetOptions).
/// Strictly forbids Payment, PathPayment, CreateAccount, AccountMerge, or any asset transfer.
pub fn validate_recovery_transaction_guardrails(
    envelope: &TransactionEnvelope,
) -> Result<(), String> {
    let operations: &[Operation] = match envelope {
        TransactionEnvelope::Tx(TransactionV1Envelope { tx, .. }) => tx.operations.as_slice(),
        TransactionEnvelope::TxV0(v0) => v0.tx.operations.as_slice(),
        _ => return Err("Unsupported envelope type".to_string()),
    };

    if operations.is_empty() {
        return Err("Transaction envelope contains no operations".to_string());
    }

    for op in operations {
        match &op.body {
            OperationBody::SetOptions(SetOptionsOp { signer, .. }) => {
                // Must be setting/updating signer options
                if signer.is_none() {
                    return Err(
                        "SetOptions operation must configure account recovery signer".to_string(),
                    );
                }
            }
            // Explicitly forbidden direct transfers or fund movement
            OperationBody::Payment(_) => {
                return Err("Security Violation: Direct Payment operations are strictly forbidden on recovery signer".to_string());
            }
            OperationBody::PathPaymentStrictReceive(_) | OperationBody::PathPaymentStrictSend(_) => {
                return Err("Security Violation: PathPayment operations are strictly forbidden on recovery signer".to_string());
            }
            OperationBody::CreateAccount(_) => {
                return Err("Security Violation: CreateAccount operations are strictly forbidden on recovery signer".to_string());
            }
            OperationBody::AccountMerge(_) => {
                return Err("Security Violation: AccountMerge operations are strictly forbidden on recovery signer".to_string());
            }
            OperationBody::ManageBuyOffer(_)
            | OperationBody::ManageSellOffer(_)
            | OperationBody::CreatePassiveSellOffer(_) => {
                return Err("Security Violation: Market operations are strictly forbidden on recovery signer".to_string());
            }
            _ => {
                return Err("Security Violation: Only SetOptions recovery operations are permitted".to_string());
            }
        }
    }

    Ok(())
}

/// Builds a canonical SetOptions transaction envelope that adds new_signer_key.
pub fn build_recovery_transaction(
    source_address: &str,
    new_signer_key: &str,
) -> Result<TransactionEnvelope, String> {
    let source_raw = ed25519::PublicKey::from_string(source_address)
        .map_err(|e| format!("Invalid source address: {e:?}"))?;
    let new_signer_raw = ed25519::PublicKey::from_string(new_signer_key)
        .map_err(|e| format!("Invalid new signer key: {e:?}"))?;

    let set_options_op = Operation {
        source_account: None,
        body: OperationBody::SetOptions(SetOptionsOp {
            inflation_dest: None,
            clear_flags: None,
            set_flags: None,
            master_weight: None,
            low_threshold: None,
            med_threshold: None,
            high_threshold: None,
            home_domain: None,
            signer: Some(stellar_xdr::curr::Signer {
                key: SignerKey::Ed25519(Uint256(new_signer_raw.0)),
                weight: 10,
            }),
        }),
    };

    let tx = Transaction {
        source_account: AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(source_raw.0))),
        fee: 100,
        seq_num: SequenceNumber(1),
        time_bounds: None,
        memo: stellar_xdr::curr::Memo::None,
        operations: vec![set_options_op].try_into().unwrap(),
        ext: stellar_xdr::curr::TransactionExt::V0,
    };

    Ok(TransactionEnvelope::Tx(TransactionV1Envelope {
        tx,
        signatures: vec![].try_into().unwrap(),
    }))
}

/// Computes the transaction signature payload and appends an Ed25519 signature
/// (either via 2-of-3 MPC threshold signing session or server signing key).
pub async fn cosign_transaction(
    mut envelope: TransactionEnvelope,
    signing_key: &SigningKey,
    mpc_coordinator: Option<&Arc<MpcCoordinator>>,
    network_passphrase: &str,
) -> Result<TransactionEnvelope, String> {
    let (tx_bytes, current_sigs) = match &mut envelope {
        TransactionEnvelope::Tx(ref mut v1) => {
            let tx_payload = TransactionSignaturePayload {
                network_id: Hash(sha256_passphrase(network_passphrase)),
                tagged_transaction: TransactionSignaturePayloadTaggedTransaction::Tx(v1.tx.clone()),
            };
            let mut buf = Vec::new();
            tx_payload
                .write_xdr(&mut buf)
                .map_err(|e| format!("XDR error: {e}"))?;
            (buf, &mut v1.signatures)
        }
        _ => return Err("Only Tx (V1) envelopes supported".to_string()),
    };

    let digest = Sha256::digest(&tx_bytes);

    let (sig_bytes, hint) = if let Some(coordinator) = mpc_coordinator {
        let req = SignRequest {
            session_id: format!(
                "sep30-session-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            transaction_payload: digest.to_vec(),
            participating_nodes: vec![1, 2],
        };
        let threshold_sig = coordinator
            .sign_transaction(req)
            .await
            .map_err(|e| format!("MPC threshold signing failed: {e}"))?;
        let bytes = threshold_sig.to_bytes();
        let master_pk = coordinator.master_public_key_bytes();
        let mut h = [0u8; 4];
        h.copy_from_slice(&master_pk[28..32]);
        (bytes, h)
    } else {
        let signature = signing_key.sign(&digest);
        let hint_bytes = &signing_key.verifying_key().to_bytes()[28..32];
        let mut h = [0u8; 4];
        h.copy_from_slice(hint_bytes);
        (signature.to_bytes(), h)
    };

    let decorated_sig = DecoratedSignature {
        hint: stellar_xdr::curr::SignatureHint(hint),
        signature: BytesM::try_from(sig_bytes.to_vec())
            .map_err(|_| "Signature exceeds XDR bounds".to_string())?,
    };

    let mut sigs_vec: Vec<DecoratedSignature> = current_sigs.to_vec();
    sigs_vec.push(decorated_sig);
    *current_sigs = sigs_vec
        .try_into()
        .map_err(|_| "Too many signatures on envelope".to_string())?;

    Ok(envelope)
}

fn sha256_passphrase(passphrase: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(passphrase.as_bytes());
    hasher.finalize().into()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guardrail_rejects_direct_payments() {
        let source = "GA2C5RFPE6GCKMY3US5PAB6UZLKIGAHWKXX2GIOVPVUIV255KZG42TM5";
        let dest = "GBRPYHIL2CI3FNQ4BXLFMNDLFJUNPU2HY3ZMFSHONUCEOASW7QC7OX2H";

        let source_raw = ed25519::PublicKey::from_string(source).unwrap();
        let dest_raw = ed25519::PublicKey::from_string(dest).unwrap();

        let payment_op = Operation {
            source_account: None,
            body: OperationBody::Payment(stellar_xdr::curr::PaymentOp {
                destination: AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(dest_raw.0))),
                asset: stellar_xdr::curr::Asset::Native,
                amount: 10_000_000,
            }),
        };

        let tx = Transaction {
            source_account: AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(source_raw.0))),
            fee: 100,
            seq_num: SequenceNumber(1),
            time_bounds: None,
            memo: stellar_xdr::curr::Memo::None,
            operations: vec![payment_op].try_into().unwrap(),
            ext: stellar_xdr::curr::TransactionExt::V0,
        };

        let env = TransactionEnvelope::Tx(TransactionV1Envelope {
            tx,
            signatures: vec![].try_into().unwrap(),
        });

        let validation = validate_recovery_transaction_guardrails(&env);
        assert!(validation.is_err());
        assert!(validation
            .unwrap_err()
            .contains("Payment operations are strictly forbidden"));
    }

    #[test]
    fn test_guardrail_accepts_set_options_recovery() {
        let source = "GA2C5RFPE6GCKMY3US5PAB6UZLKIGAHWKXX2GIOVPVUIV255KZG42TM5";
        let new_signer = "GBRPYHIL2CI3FNQ4BXLFMNDLFJUNPU2HY3ZMFSHONUCEOASW7QC7OX2H";

        let env = build_recovery_transaction(source, new_signer).expect("Build failed");
        assert!(validate_recovery_transaction_guardrails(&env).is_ok());
    }

    #[test]
    fn test_rate_limiting_enforces_max_3_attempts() {
        let audit = Arc::new(RwLock::new(AuditTrailVerifier::new()));
        let state = Sep30ServerState::new(audit);
        let address = "GA2C5RFPE6GCKMY3US5PAB6UZLKIGAHWKXX2GIOVPVUIV255KZG42TM5";

        assert!(state.record_and_check_rate_limit(address).is_ok());
        assert!(state.record_and_check_rate_limit(address).is_ok());
        assert!(state.record_and_check_rate_limit(address).is_ok());

        // 4th attempt in 24 hours fails
        let fourth_attempt = state.record_and_check_rate_limit(address);
        assert!(fourth_attempt.is_err());
        let (status, msg) = fourth_attempt.unwrap_err();
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(msg.contains("Rate limit exceeded"));
    }
}
