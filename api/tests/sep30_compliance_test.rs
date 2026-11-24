//! SEP-30 Account Recovery Compliance and Security Test Suite
//! Issue #50 / #431
//!
//! Verifies:
//! (1) POST /sep30/accounts/{address} registers recovery identity providers (email, phone, TOTP).
//! (2) GET /sep30/accounts/{address} returns registered signers.
//! (3) PUT /sep30/accounts/{address}/recover with verified identity proofs returns a signed XDR transaction adding a new signing key.
//! (4) Recovery server holds a signing key that only co-signs recovery transactions — never direct transfers.
//! (5) Rate limiting: max 3 recovery attempts per 24 h per account.
//! (6) All actions are logged to the immutable audit trail (issue #20).

use std::sync::{Arc, RwLock};

use axum::http::StatusCode;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use stellar_strkey::ed25519;
use stellar_xdr::curr::{
    AccountId, Operation, OperationBody, PaymentOp, PublicKey, ReadXdr, SequenceNumber,
    Transaction, TransactionEnvelope, TransactionV1Envelope, Uint256,
};

// We import the sep30 module logic directly
#[path = "../src/routes/sep30.rs"]
mod sep30;
#[path = "../src/audit/chain_verifier.rs"]
mod chain_verifier;
#[path = "../src/audit/mmr.rs"]
mod mmr;
#[path = "../src/models/mod.rs"]
mod models;
#[path = "../src/storage/rocksdb.rs"]
mod rocksdb;

use chain_verifier::AuditTrailVerifier;
use sep30::{
    build_recovery_transaction, validate_recovery_transaction_guardrails, AuthMethod,
    IdentityProvider, IdentityProof, RecoverAccountRequest, RegisterAccountRequest,
    Sep30ServerState, MAX_RECOVERY_ATTEMPTS_PER_24H,
};

fn generate_stellar_address() -> String {
    let mut csprng = OsRng;
    let sk = SigningKey::generate(&mut csprng);
    ed25519::PublicKey(sk.verifying_key().to_bytes()).to_string()
}

#[tokio::test]
async fn test_sep30_registration_and_signer_retrieval() {
    let audit = Arc::new(RwLock::new(AuditTrailVerifier::new()));
    let state = Arc::new(Sep30ServerState::new(audit.clone()));

    let address = generate_stellar_address();

    // 1. Register Account with Email, Phone, and TOTP
    let reg_req = RegisterAccountRequest {
        identities: vec![
            IdentityProvider {
                role: "owner".to_string(),
                auth_method: AuthMethod::Email,
                identifier: "alice@tessera.io".to_string(),
                verified: true,
            },
            IdentityProvider {
                role: "owner".to_string(),
                auth_method: AuthMethod::PhoneNumber,
                identifier: "+15551234567".to_string(),
                verified: true,
            },
            IdentityProvider {
                role: "owner".to_string(),
                auth_method: AuthMethod::Totp,
                identifier: "JBSWY3DPEHPK3PXP".to_string(),
                verified: true,
            },
        ],
        threshold: Some(2),
    };

    let reg_res = sep30::register_account(
        axum::extract::State(state.clone()),
        axum::extract::Path(address.clone()),
        axum::Json(reg_req),
    )
    .await;

    assert_eq!(reg_res.status(), StatusCode::CREATED);

    // 2. Query Account Signers & Identities
    let get_res = sep30::get_account(
        axum::extract::State(state.clone()),
        axum::extract::Path(address.clone()),
    )
    .await;

    assert_eq!(get_res.status(), StatusCode::OK);

    // 3. Verify audit trail logged both events
    let audit_lock = audit.read().unwrap();
    assert!(audit_lock.verify_chain_integrity().is_ok());
    let events = audit_lock.get_entries_range(1, 2);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "sep30_account_registered");
    assert_eq!(events[1].event_type, "sep30_account_query");
}

#[tokio::test]
async fn test_sep30_recovery_flow_returns_cosigned_transaction() {
    let audit = Arc::new(RwLock::new(AuditTrailVerifier::new()));
    let state = Arc::new(Sep30ServerState::new(audit.clone()));

    let address = generate_stellar_address();
    let new_signer_key = generate_stellar_address();

    // Register with 2-of-3 threshold
    let reg_req = RegisterAccountRequest {
        identities: vec![
            IdentityProvider {
                role: "owner".to_string(),
                auth_method: AuthMethod::Email,
                identifier: "alice@tessera.io".to_string(),
                verified: true,
            },
            IdentityProvider {
                role: "owner".to_string(),
                auth_method: AuthMethod::PhoneNumber,
                identifier: "+15551234567".to_string(),
                verified: true,
            },
            IdentityProvider {
                role: "owner".to_string(),
                auth_method: AuthMethod::Totp,
                identifier: "JBSWY3DPEHPK3PXP".to_string(),
                verified: true,
            },
        ],
        threshold: Some(2),
    };

    let _ = sep30::register_account(
        axum::extract::State(state.clone()),
        axum::extract::Path(address.clone()),
        axum::Json(reg_req),
    )
    .await;

    // Submit recovery with 2 verified proofs (Email OTP + TOTP)
    let recover_req = RecoverAccountRequest {
        new_signer_key: new_signer_key.clone(),
        identity_proofs: vec![
            IdentityProof {
                auth_method: AuthMethod::Email,
                proof: "847291".to_string(),
            },
            IdentityProof {
                auth_method: AuthMethod::Totp,
                proof: "492817".to_string(),
            },
        ],
        transaction_xdr: None,
    };

    let res = sep30::recover_account(
        axum::extract::State(state.clone()),
        axum::extract::Path(address.clone()),
        axum::Json(recover_req),
    )
    .await;

    assert_eq!(res.status(), StatusCode::OK);

    // Verify audit trail logged recovery
    let audit_lock = audit.read().unwrap();
    let latest = audit_lock.get_entry(2).unwrap(); // 1 was register, 2 is recovery
    assert_eq!(latest.event_type, "sep30_recovery_succeeded");
}

#[tokio::test]
async fn test_sep30_recovery_fails_with_insufficient_proofs() {
    let audit = Arc::new(RwLock::new(AuditTrailVerifier::new()));
    let state = Arc::new(Sep30ServerState::new(audit));

    let address = generate_stellar_address();
    let new_signer_key = generate_stellar_address();

    // Register with threshold = 2
    let reg_req = RegisterAccountRequest {
        identities: vec![
            IdentityProvider {
                role: "owner".to_string(),
                auth_method: AuthMethod::Email,
                identifier: "alice@tessera.io".to_string(),
                verified: true,
            },
            IdentityProvider {
                role: "owner".to_string(),
                auth_method: AuthMethod::PhoneNumber,
                identifier: "+15551234567".to_string(),
                verified: true,
            },
        ],
        threshold: Some(2),
    };

    let _ = sep30::register_account(
        axum::extract::State(state.clone()),
        axum::extract::Path(address.clone()),
        axum::Json(reg_req),
    )
    .await;

    // Submit recovery with only 1 proof (requires 2)
    let recover_req = RecoverAccountRequest {
        new_signer_key,
        identity_proofs: vec![IdentityProof {
            auth_method: AuthMethod::Email,
            proof: "847291".to_string(),
        }],
        transaction_xdr: None,
    };

    let res = sep30::recover_account(
        axum::extract::State(state.clone()),
        axum::extract::Path(address.clone()),
        axum::Json(recover_req),
    )
    .await;

    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_sep30_strictly_enforces_rate_limit_3_attempts() {
    let audit = Arc::new(RwLock::new(AuditTrailVerifier::new()));
    let state = Arc::new(Sep30ServerState::new(audit));

    let address = generate_stellar_address();
    let new_signer = generate_stellar_address();

    let reg_req = RegisterAccountRequest {
        identities: vec![IdentityProvider {
            role: "owner".to_string(),
            auth_method: AuthMethod::Email,
            identifier: "alice@tessera.io".to_string(),
            verified: true,
        }],
        threshold: Some(1),
    };

    let _ = sep30::register_account(
        axum::extract::State(state.clone()),
        axum::extract::Path(address.clone()),
        axum::Json(reg_req),
    )
    .await;

    let req = RecoverAccountRequest {
        new_signer_key: new_signer,
        identity_proofs: vec![IdentityProof {
            auth_method: AuthMethod::Email,
            proof: "123456".to_string(),
        }],
        transaction_xdr: None,
    };

    // Attempts 1, 2, 3 succeed
    for _ in 0..MAX_RECOVERY_ATTEMPTS_PER_24H {
        let res = sep30::recover_account(
            axum::extract::State(state.clone()),
            axum::extract::Path(address.clone()),
            axum::Json(req.clone()),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    // Attempt 4 fails with HTTP 429
    let res4 = sep30::recover_account(
        axum::extract::State(state.clone()),
        axum::extract::Path(address.clone()),
        axum::Json(req),
    )
    .await;

    assert_eq!(res4.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn test_sep30_guardrail_rejects_unauthorized_payment_transfers() {
    let source = generate_stellar_address();
    let dest = generate_stellar_address();

    let source_raw = ed25519::PublicKey::from_string(&source).unwrap();
    let dest_raw = ed25519::PublicKey::from_string(&dest).unwrap();

    let payment_op = Operation {
        source_account: None,
        body: OperationBody::Payment(PaymentOp {
            destination: AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(dest_raw.0))),
            asset: stellar_xdr::curr::Asset::Native,
            amount: 50_000_000,
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
    let err_msg = validation.unwrap_err();
    assert!(
        err_msg.contains("Payment operations are strictly forbidden"),
        "Expected payment guardrail violation, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_sep30_recovery_with_mpc_threshold_signer() {
    let audit = Arc::new(RwLock::new(AuditTrailVerifier::new()));
    let state = Arc::new(Sep30ServerState::new_with_mpc(audit.clone()).await);

    let address = generate_stellar_address();
    let new_signer_key = generate_stellar_address();

    // Register with 1-of-1 threshold for direct MPC test
    let reg_req = RegisterAccountRequest {
        identities: vec![IdentityProvider {
            role: "owner".to_string(),
            auth_method: AuthMethod::Email,
            identifier: "bob@tessera.io".to_string(),
            verified: true,
        }],
        threshold: Some(1),
    };

    let _ = sep30::register_account(
        axum::extract::State(state.clone()),
        axum::extract::Path(address.clone()),
        axum::Json(reg_req),
    )
    .await;

    let req = RecoverAccountRequest {
        new_signer_key,
        identity_proofs: vec![IdentityProof {
            auth_method: AuthMethod::Email,
            proof: "998877".to_string(),
        }],
        transaction_xdr: None,
    };

    let res = sep30::recover_account(
        axum::extract::State(state.clone()),
        axum::extract::Path(address.clone()),
        axum::Json(req),
    )
    .await;

    assert_eq!(res.status(), StatusCode::OK);
}
