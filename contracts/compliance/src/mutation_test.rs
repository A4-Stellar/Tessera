//! # Mutation Testing Verification Tests (Issue #448 / #65)
//!
//! Targeted tests to kill surviving mutations in the compliance contract:
//! 1. Exact boundary `sequence == expires_at` kills the `>=` -> `>` off-by-one mutant.
//! 2. Persistent storage validation kills the `persistent()` -> `temporary()` mutant.
//! 3. Deterministic boolean assertion kills the `is_allowed` negation mutant.

#![cfg(test)]

use crate::{ComplianceContract, ComplianceContractClient, ComplianceStatus, DataKey, KycRecord};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Address, Env,
};

#[test]
fn test_kyc_exact_expiry_boundary_kills_relational_mutant() {
    let env = Env::default();
    let contract_id = env.register_contract(None, ComplianceContract);
    let client = ComplianceContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let user = Address::generate(&env);
    let expiry_sequence = 1500u32;

    // Register user with Approved status expiring at ledger 1500
    env.as_contract(&contract_id, || {
        let record = KycRecord {
            status: ComplianceStatus::Approved,
            jurisdiction: symbol_short!("US"),
            accredited: true,
            expires_at: expiry_sequence,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Record(user.clone()), &record);
    });

    // Case 1: One ledger BEFORE expiry (1499 < 1500) -> Allowed
    env.ledger().set_sequence_number(1499);
    assert_eq!(
        client.is_allowed(&user),
        true,
        "User must be allowed before expiry sequence"
    );

    // Case 2: EXACTLY AT expiry (1500 >= 1500) -> Expired / NOT allowed
    // A mutated operator replacing `>=` with `>` would evaluate 1500 > 1500 (false)
    // and incorrectly allow the expired user! This assertion kills that mutant.
    env.ledger().set_sequence_number(1500);
    assert_eq!(
        client.is_allowed(&user),
        false,
        "User MUST NOT be allowed at exact expiration sequence (kills >= to > mutation)"
    );

    // Case 3: One ledger AFTER expiry (1501 >= 1500) -> Expired / NOT allowed
    env.ledger().set_sequence_number(1501);
    assert_eq!(
        client.is_allowed(&user),
        false,
        "User must not be allowed after expiration sequence"
    );
}

#[test]
fn test_persistent_storage_kills_temporary_storage_mutant() {
    let env = Env::default();
    let contract_id = env.register_contract(None, ComplianceContract);
    let client = ComplianceContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let user = Address::generate(&env);

    // Write record to persistent storage
    env.as_contract(&contract_id, || {
        let record = KycRecord {
            status: ComplianceStatus::Approved,
            jurisdiction: symbol_short!("UK"),
            accredited: false,
            expires_at: 0,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Record(user.clone()), &record);

        // Assert record is present in persistent storage and NOT temporary storage
        assert!(env.storage().persistent().has(&DataKey::Record(user.clone())));
        assert!(!env.storage().temporary().has(&DataKey::Record(user.clone())));
    });

    assert_eq!(client.is_allowed(&user), true);
}

#[test]
fn test_is_allowed_negation_mutant_killed() {
    let env = Env::default();
    let contract_id = env.register_contract(None, ComplianceContract);
    let client = ComplianceContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let unknown_user = Address::generate(&env);

    // Non-existent KYC record must return false.
    // If negated, it would return true.
    assert_eq!(
        client.is_allowed(&unknown_user),
        false,
        "Unknown address must not be allowed (kills negation mutant)"
    );
}
