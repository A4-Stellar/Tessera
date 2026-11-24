//! Regression tests for the fuzz-found bridge defects (issue #470): the
//! signature loop never actually checked `ed25519_verify`, so two all-zero
//! blobs satisfied the two-of-N multisig.

use soroban_sdk::{
    testutils::Address as _, Address, BytesN, Env, Error as SdkError, Vec as SdkVec,
};

use crate::{BridgeProtocol, BridgeProtocolClient, Error, MIN_SIGNATURES};

fn declared(error: Error) -> SdkError {
    error.into()
}

fn validator(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

#[test]
fn garbage_signatures_do_not_satisfy_the_multisig() {
    let env = Env::default();
    let id = env.register(BridgeProtocol, ());
    let client = BridgeProtocolClient::new(&env, &id).mock_all_auths();
    let admin = Address::generate(&env);
    let caller = Address::generate(&env);
    client.initialize(&admin);
    // Neither key is registered: this is the exact shape of the original
    // bypass — two all-zero 64-byte blobs were accepted as a 2-of-N multisig.
    let message = BytesN::from_array(&env, &[7u8; 32]);
    let mut signatures = SdkVec::new(&env);
    signatures.push_back(BytesN::from_array(&env, &[0u8; 64]));
    signatures.push_back(BytesN::from_array(&env, &[0u8; 64]));
    let mut keys = SdkVec::new(&env);
    keys.push_back(validator(&env, 1));
    keys.push_back(validator(&env, 2));

    let insufficient = declared(Error::InsufficientSignatures);
    assert!(matches!(
        client.try_burn_and_unlock_request(
            &caller,
            &1_000,
            &message,
            &signatures,
            &keys,
            &1_000_000,
        ),
        Err(Ok(error)) if error == insufficient
    ));
    assert!(!client.is_processed(&message));
}

#[test]
fn mismatched_lengths_are_a_declared_error_not_a_trap() {
    let env = Env::default();
    let id = env.register(BridgeProtocol, ());
    let client = BridgeProtocolClient::new(&env, &id).mock_all_auths();
    let admin = Address::generate(&env);
    let caller = Address::generate(&env);
    client.initialize(&admin);

    let mut signatures = SdkVec::new(&env);
    signatures.push_back(BytesN::from_array(&env, &[0u8; 64]));
    let keys = SdkVec::new(&env);
    let message = BytesN::from_array(&env, &[3u8; 32]);
    let mismatch = declared(Error::LengthMismatch);
    assert!(matches!(
        client.try_burn_and_unlock_request(
            &caller,
            &1,
            &message,
            &signatures,
            &keys,
            &1_000_000,
        ),
        Err(Ok(error)) if error == mismatch
    ));
}

#[test]
fn two_genuine_distinct_validators_mark_the_message_processed() {
    use ed25519_dalek::{Signer, SigningKey};

    let env = Env::default();
    let id = env.register(BridgeProtocol, ());
    let client = BridgeProtocolClient::new(&env, &id).mock_all_auths();
    let admin = Address::generate(&env);
    let caller = Address::generate(&env);

    let sk1 = SigningKey::from_bytes(&[1u8; 32]);
    let sk2 = SigningKey::from_bytes(&[2u8; 32]);
    let k1 = BytesN::from_array(&env, &sk1.verifying_key().to_bytes());
    let k2 = BytesN::from_array(&env, &sk2.verifying_key().to_bytes());
    client.initialize(&admin);
    client.register_validator(&admin, &k1);
    client.register_validator(&admin, &k2);

    let raw_message = [9u8; 32];
    let message = BytesN::from_array(&env, &raw_message);
    let mut signatures = SdkVec::new(&env);
    signatures.push_back(BytesN::from_array(&env, &sk1.sign(&raw_message).to_bytes()));
    signatures.push_back(BytesN::from_array(&env, &sk2.sign(&raw_message).to_bytes()));
    let mut keys = SdkVec::new(&env);
    keys.push_back(k1);
    keys.push_back(k2);

    client.burn_and_unlock_request(&caller, &1, &message, &signatures, &keys, &1_000_000);
    assert!(client.is_processed(&message));
    assert_eq!(MIN_SIGNATURES, 2);
}
