//! Property: the bridge only accepts a message carrying two genuine, distinct,
//! registered validator signatures.
//!
//! The bug this guards against was a total authentication bypass. The original
//! verification loop called `ed25519_verify` and then incremented
//! `valid_signatures` unconditionally, discarding the result — so *any* two
//! 64-byte blobs, including two copies of all zeroes, satisfied a two-of-N
//! multisig. Two properties are needed to pin that down:
//!
//! 1. No combination of invalid signatures ever authenticates. This is the
//!    bypass itself.
//! 2. Two *genuine* signatures from two *distinct registered* validators do
//!    authenticate. Without this, the trivial fix — always reject — would pass
//!    property 1 and silently break the bridge.

use bridge::{BridgeProtocol, BridgeProtocolClient, Error as BridgeError, MIN_SIGNATURES};
use proptest::prelude::*;
use soroban_sdk::{
    testutils::Address as _, testutils::Ledger as _, Address, Bytes, BytesN, Env,
    Vec as SdkVec,
};

type Try<T, E> =
    Result<Result<T, soroban_sdk::ConversionError>, Result<E, soroban_sdk::InvokeError>>;

fn is_ok<T, E>(r: &Try<T, E>) -> bool {
    matches!(r, Ok(Ok(_)))
}

fn is_err<T, E>(r: &Try<T, E>, e: E) -> bool
where
    E: PartialEq + core::fmt::Debug,
{
    matches!(r, Err(Ok(actual)) if *actual == e)
}

struct Keypair {
    key: BytesN<32>,
    secret: BytesN<32>,
}

impl Keypair {
    fn new(env: &Env, seed: u8) -> Self {
        let secret = BytesN::from_array(env, &[seed; 32]);
        let key = env.crypto().ed25519_public_key(&secret);
        Self { key, secret }
    }

    fn sign(&self, env: &Env, msg: &BytesN<32>) -> BytesN<64> {
        env.crypto().ed25519_sign(&self.secret, msg)
    }
}

fn to_sigs(env: &Env, items: &[BytesN<64>]) -> SdkVec<BytesN<64>> {
    let mut v = SdkVec::new(env);
    for i in items {
        v.push_back(i.clone());
    }
    v
}

fn to_keys(env: &Env, items: &[BytesN<32>]) -> SdkVec<BytesN<32>> {
    let mut v = SdkVec::new(env);
    for i in items {
        v.push_back(i.clone());
    }
    v
}

struct Fixture {
    env: Env,
    bridge: BridgeProtocolClient<'static>,
    caller: Address,
    admin: Address,
    /// Registered validators, all with a distinct seed.
    validators: Vec<Keypair>,
}

fn setup(registered: usize) -> Fixture {
    let env = Env::default();
    let id = env.register(BridgeProtocol, ());
    let admin = Address::generate(&env);
    let caller = Address::generate(&env);
    let bridge = BridgeProtocolClient::new(&env, &id).mock_all_auths();

    // The test ledger starts at timestamp 0, which would make every ttl of 0
    // valid under `timestamp > ttl`. Push it forward so "expired" and "not yet
    // expired" are distinguishable.
    env.ledger().set_timestamp(1_000_000);

    let r = bridge.try_initialize(&admin);
    assert!(is_ok(&r), "setup: initialize failed: {r:?}");

    let validators: Vec<Keypair> = (0..4).map(|i| Keypair::new(&env, (i + 1) as u8)).collect();
    for v in validators.iter().take(registered) {
        let r = bridge.try_register_validator(&admin, &v.key);
        assert!(is_ok(&r), "setup: validator registration failed: {r:?}");
    }

    Fixture { env, bridge, caller, admin, validators }
}

fn message(env: &Env, tag: u8) -> BytesN<32> {
    let mut raw = [0u8; 32];
    raw[0] = tag;
    let _ = env;
    BytesN::from_array(env, &raw)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Two genuine signatures from two distinct registered validators are
    /// accepted, and the message is then marked processed.
    #[test]
    fn genuine_distinct_signatures_are_accepted(registered in 2usize..=4) {
        let f = setup(registered);
        let msg = message(&f.env, 0x11);
        let ttl = f.env.ledger().timestamp() + 86_400;

        let sigs = to_sigs(
            &f.env,
            &[
                f.validators[0].sign(&f.env, &msg),
                f.validators[1].sign(&f.env, &msg),
            ],
        );
        let keys = to_keys(
            &f.env,
            &[f.validators[0].key.clone(), f.validators[1].key.clone()],
        );

        let r = f.bridge.try_burn_and_unlock_request(&f.caller, &1_000, &msg, &sigs, &keys, &ttl);
        prop_assert!(
            is_ok(&r),
            "genuine distinct signatures from {registered} registered validators were rejected: {r:?}"
        );
        prop_assert!(f.bridge.is_processed(&msg), "an accepted message was not marked processed");
    }

    /// A single genuine signature is never enough for a two-of-N threshold,
    /// however it is padded.
    #[test]
    fn one_signature_is_never_enough(registered in 2usize..=4) {
        let f = setup(registered);
        let msg = message(&f.env, 0x22);
        let ttl = f.env.ledger().timestamp() + 86_400;
        let good = f.validators[0].sign(&f.env, &msg);

        // One real signature alone.
        let sigs = to_sigs(&f.env, &[good.clone()]);
        let keys = to_keys(&f.env, &[f.validators[0].key.clone()]);
        let r = f.bridge.try_burn_and_unlock_request(&f.caller, &1_000, &msg, &sigs, &keys, &ttl);
        prop_assert!(!is_ok(&r), "a single signature was accepted");
        prop_assert!(
            is_err(&r, BridgeError::InsufficientSignatures),
            "expected InsufficientSignatures, got {r:?}"
        );
        prop_assert!(!f.bridge.is_processed(&msg), "a rejected message was marked processed");

        // The same real signature replayed under the same key twice: this is
        // the "one validator signing twice" attack. Lengths match, so the
        // rejection has to come from the distinctness rule, not the length one.
        let sigs = to_sigs(&f.env, &[good.clone(), good.clone()]);
        let keys = to_keys(
            &f.env,
            &[f.validators[0].key.clone(), f.validators[0].key.clone()],
        );
        let r = f.bridge.try_burn_and_unlock_request(&f.caller, &1_000, &msg, &sigs, &keys, &ttl);
        prop_assert!(!is_ok(&r), "one validator signing twice met the threshold");
        prop_assert!(
            is_err(&r, BridgeError::InsufficientSignatures),
            "expected InsufficientSignatures, got {r:?}"
        );
        prop_assert!(!f.bridge.is_processed(&msg), "a replayed message was marked processed");
    }

    /// Arbitrary garbage bytes never authenticate, at any length. This is the
    /// direct regression test for the discarded-`ed25519_verify` bypass.
    #[test]
    fn garbage_signatures_never_authenticate(
        sig_a in any::<[u8; 64]>(),
        sig_b in any::<[u8; 64]>(),
        registered in 2usize..=4,
    ) {
        let f = setup(registered);
        let msg = message(&f.env, 0x33);
        let ttl = f.env.ledger().timestamp() + 86_400;

        let sigs = to_sigs(
            &f.env,
            &[
                BytesN::from_array(&f.env, &sig_a),
                BytesN::from_array(&f.env, &sig_b),
            ],
        );
        let keys = to_keys(
            &f.env,
            &[f.validators[0].key.clone(), f.validators[1].key.clone()],
        );

        let r = f.bridge.try_burn_and_unlock_request(&f.caller, &1_000, &msg, &sigs, &keys, &ttl);
        // Overwhelmingly likely the sigs are invalid, so this must be refused.
        // In the astronomically unlikely case a random pair happens to be a
        // valid signature, an acceptance is legitimate and the test is vacuous
        // for that case rather than wrong.
        if !is_ok(&r) {
            prop_assert!(
                is_err(&r, BridgeError::InsufficientSignatures),
                "garbage was rejected for the wrong reason: {r:?}"
            );
        }
    }

    /// Mismatched lengths between `signatures` and `public_keys` are a declared
    /// `LengthMismatch`, never a trap. The original code unwrapped
    /// `public_keys.get(i)`, so this was reachable with one call.
    #[test]
    fn length_mismatch_is_declared_not_a_trap(
        n_sigs in 0usize..=4,
        n_keys in 0usize..=4,
        registered in 2usize..=4,
    ) {
        let f = setup(registered);
        let msg = message(&f.env, 0x44);
        let ttl = f.env.ledger().timestamp() + 86_400;

        let sigs = to_sigs(
            &f.env,
            &(0..n_sigs)
                .map(|_| BytesN::from_array(&f.env, &[0u8; 64]))
                .collect::<Vec<_>>(),
        );
        let keys = to_keys(
            &f.env,
            &(0..n_keys)
                .map(|i| f.validators[i % f.validators.len()].key.clone())
                .collect::<Vec<_>>(),
        );

        let r = f.bridge.try_burn_and_unlock_request(&f.caller, &1_000, &msg, &sigs, &keys, &ttl);
        if n_sigs != n_keys {
            // Must be a clean, declared error — and specifically the length one,
            // not a trap and not AlreadyProcessed.
            prop_assert!(!is_ok(&r), "mismatched lengths ({n_sigs} vs {n_keys}) were accepted");
            prop_assert!(
                is_err(&r, BridgeError::LengthMismatch),
                "mismatched lengths ({n_sigs} vs {n_keys}) reported the wrong error: {r:?}"
            );
        }
        // Equal lengths fall through to the signature check, which must still
        // refuse these all-zero signatures.
        if n_sigs == n_keys && n_sigs >= MIN_SIGNATURES as usize {
            prop_assert!(!is_ok(&r), "all-zero signatures at matching lengths were accepted");
        }
    }

    /// A valid message is accepted exactly once; every replay is refused.
    #[test]
    fn replays_are_refused(registered in 2usize..=4) {
        let f = setup(registered);
        let msg = message(&f.env, 0x55);
        let ttl = f.env.ledger().timestamp() + 86_400;
        let sigs = to_sigs(
            &f.env,
            &[
                f.validators[0].sign(&f.env, &msg),
                f.validators[1].sign(&f.env, &msg),
            ],
        );
        let keys = to_keys(
            &f.env,
            &[f.validators[0].key.clone(), f.validators[1].key.clone()],
        );

        let first = f.bridge.try_burn_and_unlock_request(&f.caller, &1_000, &msg, &sigs, &keys, &ttl);
        prop_assert!(is_ok(&first), "the first submission was rejected: {first:?}");

        for replay in 1..3 {
            let r = f.bridge.try_burn_and_unlock_request(&f.caller, &1_000, &msg, &sigs, &keys, &ttl);
            prop_assert!(!is_ok(&r), "replay {replay} of a processed message was accepted");
            prop_assert!(
                is_err(&r, BridgeError::AlreadyProcessed),
                "replay {replay} reported the wrong error: {r:?}"
            );
        }
    }

    /// An expired ttl is refused, and the nonce advances by exactly one per
    /// `lock_and_mint_request`, never skipping or repeating.
    #[test]
    fn ttl_and_nonce_behaviour(ttl_offset in 0i64..200_000i64, calls in 1u8..5) {
        let f = setup(2);
        let base = f.env.ledger().timestamp() as i64;
        let ttl = (base + ttl_offset).max(0) as u64;
        let msg = message(&f.env, 0x66);
        let zero = BytesN::from_array(&f.env, &[0u8; 64]);
        let sigs = to_sigs(&f.env, &[zero.clone(), zero]);
        let keys = to_keys(
            &f.env,
            &[f.validators[0].key.clone(), f.validators[1].key.clone()],
        );

        let r = f.bridge.try_burn_and_unlock_request(&f.caller, &1_000, &msg, &sigs, &keys, &ttl);
        if ttl < f.env.ledger().timestamp() {
            prop_assert!(!is_ok(&r), "an expired message (ttl {ttl}) was accepted");
            prop_assert!(
                is_err(&r, BridgeError::MessageExpired),
                "expected MessageExpired for ttl {ttl}, got {r:?}"
            );
        }

        // Nonce monotonicity, one step at a time.
        let mut expected = f.bridge.nonce_of(&f.caller);
        for i in 0..calls {
            let r = f.bridge.try_lock_and_mint_request(
                &f.caller, &1_000, &Bytes::from_array(&f.env, &[1, 2, 3, 4]), &Bytes::from_array(&f.env, &[5, 6, 7, 8]),
            );
            prop_assert!(is_ok(&r), "lock_and_mint_request {i} failed: {r:?}");
            prop_assert_eq!(
                f.bridge.nonce_of(&f.caller),
                expected + 1,
                "the nonce must advance by exactly one per call"
            );
            let req = f.bridge.get_request(&f.caller, &expected);
            prop_assert_eq!(req.nonce, expected, "the request is filed under the wrong nonce");
            expected += 1;
        }
    }

    /// Only the admin may register or revoke a validator.
    #[test]
    fn validator_admin_only(registered in 1usize..=4) {
        let f = setup(registered);
        let stranger = Address::generate(&f.env);
        let fresh = Keypair::new(&f.env, 0x7f);

        let reg = f.bridge.try_register_validator(&stranger, &fresh.key);
        prop_assert!(!is_ok(&reg), "a non-admin registered a validator");
        prop_assert!(
            is_err(&reg, BridgeError::Unauthorized),
            "expected Unauthorized, got {reg:?}"
        );
        prop_assert!(!f.bridge.is_validator(&fresh.key), "a refused registration took effect");

        // Revoking an unregistered key is a declared error, not a trap.
        let rev = f.bridge.try_revoke_validator(&f.admin, &fresh.key);
        prop_assert!(!is_ok(&rev), "revoking an unregistered validator succeeded");
        prop_assert!(
            is_err(&rev, BridgeError::ValidatorNotRegistered),
            "expected ValidatorNotRegistered, got {rev:?}"
        );
    }
}
