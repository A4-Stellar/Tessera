//! Fuzz target for `bridge` (`BridgeProtocol`).
//!
//! This target covers two bugs that were both trivially reachable from a public
//! entry point:
//!
//! 1. **Signature verification was discarded.** The old loop called
//!    `ed25519_verify` and then counted the signature unconditionally, so any
//!    two 64-byte blobs — two copies of all zeroes included — satisfied the
//!    two-of-N threshold. The target asserts the opposite directly: a message
//!    signed only by garbage, only by unregistered keys, or only by one key
//!    repeated, must be *rejected*; and a message genuinely signed by two
//!    distinct registered validators must be *accepted*. Without the positive
//!    case a verifier that rejects everything would look correct.
//! 2. **`public_keys.get(i).unwrap()` trapped** when `signatures` was longer
//!    than `public_keys`. The target drives mismatched lengths in both
//!    directions and asserts a declared `LengthMismatch`, never a trap.
//!
//! Also covered: TTL expiry, replay protection, nonce monotonicity, validator
//! registration and revocation, and admin authorization.

#![no_main]

use libfuzzer_sys::fuzz_target;

use bridge::{BridgeProtocol, BridgeProtocolClient, Error as BridgeError};
use soroban_sdk::{
    testutils::Address as _, testutils::Ledger as _, Address, Bytes, BytesN, Env, Vec as SdkVec,
};
use tessera_fuzz_harness::{
    check_no_trap, failed_with, step_ledger, succeeded, Input, MAX_OPS,
};

/// A validator keypair the target controls, so it can produce *genuine*
/// signatures rather than only garbage.
struct Validator {
    key: BytesN<32>,
    secret: BytesN<32>,
}

impl Validator {
    fn new(env: &Env, seed: u8) -> Self {
        let secret = BytesN::from_array(env, &[seed; 32]);
        let key = env.crypto().ed25519_public_key(&secret);
        Self { key, secret }
    }

    fn sign(&self, env: &Env, message: &BytesN<32>) -> BytesN<64> {
        env.crypto().ed25519_sign(&self.secret, message)
    }
}

fn to_sdk(env: &Env, items: &[BytesN<32>]) -> SdkVec<BytesN<32>> {
    let mut v = SdkVec::new(env);
    for i in items {
        v.push_back(i.clone());
    }
    v
}

fn to_sigs(env: &Env, items: &[BytesN<64>]) -> SdkVec<BytesN<64>> {
    let mut v = SdkVec::new(env);
    for i in items {
        v.push_back(i.clone());
    }
    v
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    let bridge_id = env.register(BridgeProtocol, ());
    let admin = Address::generate(&env);
    let impostor = Address::generate(&env);
    let caller = Address::generate(&env);
    let b = BridgeProtocolClient::new(&env, &bridge_id).mock_all_auths();

    // The test ledger starts at timestamp 0, which would make every `ttl` of 0
    // *valid* under `timestamp > ttl`. Push it forward so "expired" and "not
    // yet expired" are genuinely distinguishable.
    env.ledger().set_timestamp(1_000_000);

    check_no_trap(&b.try_initialize(&admin), "bridge::initialize");
    let initialized = succeeded(&b.try_admin());

    // Three candidate validators, only some of which get registered.
    let validators: Vec<Validator> = (0..3)
        .map(|i| Validator::new(&env, input.u8().wrapping_add(i as u8).wrapping_add(1)))
        .collect();
    let registered = [
        input.bool(),
        input.bool(),
        // Always register at least one so the positive path is reachable.
        true,
    ];
    for (v, reg) in validators.iter().zip(registered.iter()) {
        if *reg {
            check_no_trap(
                &b.try_register_validator(&admin, &v.key),
                "bridge::register_validator",
            );
        }
    }
    for (i, v) in validators.iter().enumerate() {
        assert_eq!(
            b.is_validator(&v.key),
            registered[i] && initialized,
            "is_validator disagrees with the registration"
        );
    }

    // A message hash, and a ttl that is sometimes already in the past.
    let message = BytesN::from_array(&env, &input.bytes32());
    let far_future = env.ledger().timestamp().saturating_add(86_400);
    let past = env.ledger().timestamp().saturating_sub(86_400);

    // One fresh hash per negative case, none of which may collide with
    // `message`. Tags are folded into the first byte so a collision is
    // vanishingly unlikely, and each case asserts its own hash is unprocessed.
    let case_hashes: [BytesN<32>; 4] = core::array::from_fn(|i| {
        let mut raw = input.bytes32();
        raw[0] ^= 0xa5u8.wrapping_add(i as u8);
        BytesN::from_array(&env, &raw)
    });
    for h in case_hashes.iter() {
        assert_ne!(*h, message, "a negative case hash collided with the positive case");
    }

    // ---- the two headline properties ------------------------------------
    if initialized && registered.iter().filter(|r| **r).count() >= 2 {
        // Positive case: two genuine, distinct, registered signatures must pass.
        let good: Vec<&Validator> = validators
            .iter()
            .zip(registered.iter())
            .filter(|(_, r)| **r)
            .collect();
        let a = good[0];
        let bb = good[1];
        let sigs = to_sigs(&env, &[a.sign(&env, &message), bb.sign(&env, &message)]);
        let keys = to_sdk(&env, &[a.key.clone(), bb.key.clone()]);
        let res = b.try_burn_and_unlock_request(
            &caller,
            &1_000,
            &message,
            &sigs,
            &keys,
            &far_future,
        );
        check_no_trap(&res, "bridge::burn_and_unlock_request(genuine)");
        if succeeded(&res) {
            assert!(
                b.is_processed(&message),
                "a message accepted with genuine signatures was not marked processed"
            );
        }
    }

    if initialized {
        // Negative case: two garbage signatures must NOT be accepted. This is
        // the assertion that the discarded-verify-result bug would fail.
        //
        // Every negative case below uses its own message hash. Reusing `message`
        // here would be a bug in the target: the positive case above already
        // marked it processed, so the contract would reject it with
        // `AlreadyProcessed` and the assertion would pass for the wrong reason —
        // it would no longer be testing the signature check at all.
        let zero_sig = BytesN::from_array(&env, &[0u8; 64]);
        let unregistered = Validator::new(&env, 200);

        // The "signed a different message" case needs a hash that is guaranteed
        // to differ from the one it submits. Deriving it from `case_hashes[3]`
        // by flipping one bit makes that a fact rather than a probability, and
        // the guard below turns even a violation of that fact into a loud
        // failure instead of a signature that verifies for the wrong reason.
        let mut other_raw = case_hashes[3].to_array();
        other_raw[31] ^= 0xff;
        let other_message = BytesN::from_array(&env, &other_raw);
        assert!(
            !case_hashes.contains(&other_message),
            "the alternate message collided with a negative case hash"
        );

        for case in 0..4usize {
            let neg = case_hashes[case];
            // Guard the premise: if this hash were already processed, the
            // rejection below would prove nothing.
            assert!(
                !b.is_processed(&neg),
                "negative case {case} reused an already-processed message"
            );

            // The signatures and keys are built *inside* the loop from `neg`,
            // so a case cannot accidentally sign a different message than the
            // one it submits. Building them in a literal before the loop made
            // that possible, and the "different message" case in particular
            // signed `neg` itself — which is a *genuine* pair of signatures and
            // so was accepted, turning this whole block into a false-positive
            // generator whenever two validators happened to be registered.
            let (label, sigs, keys) = match case {
                0 => (
                    "identical garbage signatures",
                    to_sigs(&env, &[zero_sig.clone(), zero_sig.clone()]),
                    to_sdk(&env, &[unregistered.key.clone(), unregistered.key.clone()]),
                ),
                1 => (
                    "garbage signatures under distinct keys",
                    to_sigs(
                        &env,
                        &[zero_sig.clone(), BytesN::from_array(&env, &[9u8; 64])],
                    ),
                    to_sdk(
                        &env,
                        &[
                            validators[0].key.clone(),
                            unregistered.key.clone(),
                        ],
                    ),
                ),
                2 => {
                    // A genuine signature, replayed under the same key, over the
                    // message this case submits. Lengths match, so the rejection
                    // can only come from the distinctness rule.
                    match validators.iter().find(|v| b.is_validator(&v.key)) {
                        Some(v) => (
                            "one registered validator signing twice",
                            to_sigs(&env, &[v.sign(&env, &neg), v.sign(&env, &neg)]),
                            to_sdk(&env, &[v.key.clone(), v.key.clone()]),
                        ),
                        None => (
                            "one registered validator signing twice",
                            to_sigs(&env, &[zero_sig.clone(), zero_sig.clone()]),
                            to_sdk(&env, &[unregistered.key.clone(), unregistered.key.clone()]),
                        ),
                    }
                }
                _ => (
                    "a signature over a different message",
                    to_sigs(
                        &env,
                        &[
                            validators[0].sign(&env, &other_message),
                            validators[1].sign(&env, &other_message),
                        ],
                    ),
                    to_sdk(
                        &env,
                        &[validators[0].key.clone(), validators[1].key.clone()],
                    ),
                ),
            };

            let res = b.try_burn_and_unlock_request(
                &caller,
                &1_000,
                &neg,
                &sigs,
                &keys,
                &far_future,
            );
            check_no_trap(&res, "bridge::burn_and_unlock_request(negative)");
            assert!(
                !succeeded(&res),
                "burn_and_unlock_request accepted {label}"
            );
            assert!(
                failed_with(&res, BridgeError::InsufficientSignatures),
                "{label} was not rejected with InsufficientSignatures"
            );
            assert!(
                !b.is_processed(&neg),
                "a rejected message was marked processed ({label})"
            );
        }

        // Mismatched lengths, in both directions, must be a declared error and
        // never a trap. The old code unwrapped `public_keys.get(i)`.
        //
        // Each case gets a fresh hash *and* asserts the specific error code.
        // Reusing `message` would be rejected with `AlreadyProcessed` before the
        // length check is ever reached, so the assertion would pass while
        // testing nothing — and the old trapping behaviour would slip through.
        let zero64 = BytesN::from_array(&env, &[0u8; 64]);
        let len_hashes: [BytesN<32>; 4] = core::array::from_fn(|i| {
            let mut raw = input.bytes32();
            raw[0] ^= 0x5cu8.wrapping_add(i as u8);
            BytesN::from_array(&env, &raw)
        });
        for (case, (label, sigs, keys)) in [
            (
                "more signatures than keys",
                to_sigs(&env, &[zero64.clone(), zero64.clone(), zero64.clone()]),
                to_sdk(&env, &[validators[0].key.clone()]),
            ),
            (
                "more keys than signatures",
                to_sigs(&env, &[zero64.clone()]),
                to_sdk(&env, &[
                    validators[0].key.clone(),
                    validators[1].key.clone(),
                    validators[2].key.clone(),
                ]),
            ),
            (
                "keys but no signatures",
                to_sigs(&env, &[]),
                to_sdk(&env, &[validators[0].key.clone()]),
            ),
            (
                "signatures but no keys",
                to_sigs(&env, &[zero64.clone()]),
                to_sdk(&env, &[]),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let h = len_hashes[case];
            let res = b.try_burn_and_unlock_request(
                &caller,
                &1_000,
                &h,
                &sigs,
                &keys,
                &far_future,
            );
            check_no_trap(&res, "bridge::burn_and_unlock_request(length)");
            assert!(!succeeded(&res), "mismatched lengths accepted: {label}");
            assert!(
                failed_with(&res, BridgeError::LengthMismatch),
                "mismatched lengths did not report LengthMismatch: {label}"
            );
            assert!(!b.is_processed(&h), "a length-rejected message was marked processed");
        }
    }

    // ---- stateful surface ------------------------------------------------
    for _ in 0..MAX_OPS {
        step_ledger(&env, &mut input);
        let amount = input.i128_interesting();
        let msg = BytesN::from_array(&env, &input.bytes32());
        let sig_len = input.seq_len();
        let key_len = if input.bool() { sig_len } else { input.seq_len() };

        match input.below(10) {
            0 | 1 => {
                // Nonce monotonicity, and request round-trip.
                let before = b.nonce_of(&caller);
                let chain = Bytes::from_array(&env, &input.bytes(8));
                let dest = Bytes::from_array(&env, &input.bytes(8));
                let res = b.try_lock_and_mint_request(&caller, &amount, &chain, &dest);
                check_no_trap(&res, "bridge::lock_and_mint_request");
                if succeeded(&res) {
                    let nonce = before;
                    assert_eq!(
                        b.nonce_of(&caller),
                        nonce + 1,
                        "the nonce must advance by exactly one"
                    );
                    let req = b.get_request(&caller, &nonce);
                    assert_eq!(req.caller, caller, "request caller mismatch");
                    assert_eq!(req.nonce, nonce, "request nonce mismatch");
                    assert_eq!(req.amount, amount, "request amount mismatch");
                    assert_eq!(req.destination_chain, chain, "request chain mismatch");
                    assert_eq!(req.destination_address, dest, "request address mismatch");
                }
            }
            2 => {
                // Registering the same validator twice is a declared error.
                let v = &validators[input.below(validators.len())];
                let res = b.try_register_validator(&admin, &v.key);
                check_no_trap(&res, "bridge::register_validator(dup)");
                if b.is_validator(&v.key) && initialized {
                    assert!(!succeeded(&res), "a validator was registered twice");
                    assert!(
                        failed_with(&res, BridgeError::ValidatorAlreadyRegistered),
                        "re-registering a validator did not report ValidatorAlreadyRegistered"
                    );
                }
            }
            3 => {
                let res = b.try_register_validator(&impostor, &validators[0].key);
                check_no_trap(&res, "bridge::register_validator(impostor)");
                if initialized {
                    assert!(!succeeded(&res), "a non-admin registered a validator");
                    assert!(
                        failed_with(&res, BridgeError::Unauthorized),
                        "a non-admin registration was not rejected with Unauthorized"
                    );
                }
            }
            4 => {
                let i = input.below(validators.len());
                let res = b.try_revoke_validator(&admin, &validators[i].key);
                check_no_trap(&res, "bridge::revoke_validator");
                if succeeded(&res) {
                    assert!(
                        !b.is_validator(&validators[i].key),
                        "a revoked validator still reports as registered"
                    );
                }
            }
            5 => {
                // An already-expired ttl must be refused.
                let zero64 = BytesN::from_array(&env, &[0u8; 64]);
                let res = b.try_burn_and_unlock_request(
                    &caller,
                    &amount,
                    &msg,
                    &to_sigs(&env, &[zero64.clone(), zero64.clone()]),
                    &to_sdk(&env, &[validators[0].key.clone(), validators[1].key.clone()]),
                    &past,
                );
                check_no_trap(&res, "bridge::burn_and_unlock_request(expired)");
                assert!(!succeeded(&res), "an expired message was accepted");
                assert!(
                    failed_with(&res, BridgeError::MessageExpired),
                    "an expired message was not rejected with MessageExpired"
                );
                assert!(!b.is_processed(&msg), "an expired message was marked processed");
            }
            6 => {
                // Replay: a message already marked processed must be refused.
                if b.is_processed(&msg) {
                    let zero64 = BytesN::from_array(&env, &[0u8; 64]);
                    let res = b.try_burn_and_unlock_request(
                        &caller,
                        &amount,
                        &msg,
                        &to_sigs(&env, &[zero64.clone(), zero64.clone()]),
                        &to_sdk(&env, &[validators[0].key.clone(), validators[1].key.clone()]),
                        &far_future,
                    );
                    check_no_trap(&res, "bridge::burn_and_unlock_request(replay)");
                    assert!(!succeeded(&res), "a replayed message was accepted");
                    // `far_future` rules out `MessageExpired`, and the contract
                    // checks the processed flag before the signature count, so
                    // the only remaining rejection is the replay itself.
                    assert!(
                        failed_with(&res, BridgeError::AlreadyProcessed),
                        "a replay was not rejected with AlreadyProcessed"
                    );
                }
            }
            7 => {
                // Fuzz the signature path with unaligned lengths and random
                // material. This is the shape that used to trap.
                let sigs: Vec<BytesN<64>> = (0..sig_len)
                    .map(|_| BytesN::from_array(&env, &input.bytes64()))
                    .collect();
                let keys: Vec<BytesN<32>> = (0..key_len)
                    .map(|_| {
                        if input.bool() {
                            validators[input.below(validators.len())].key.clone()
                        } else {
                            BytesN::from_array(&env, &input.bytes32())
                        }
                    })
                    .collect();
                let ttl = if input.bool() { past } else { far_future };
                let res = b.try_burn_and_unlock_request(
                    &caller,
                    &amount,
                    &msg,
                    &to_sigs(&env, &sigs),
                    &to_sdk(&env, &keys),
                    &ttl,
                );
                check_no_trap(&res, "bridge::burn_and_unlock_request(fuzz)");
            }
            8 => {
                let _ = b.version();
            }
            _ => {
                let res = b.try_initialize(&impostor);
                check_no_trap(&res, "bridge::initialize(again)");
                if initialized {
                    assert!(!succeeded(&res), "the bridge was initialised twice");
                }
            }
        }
    }
});
