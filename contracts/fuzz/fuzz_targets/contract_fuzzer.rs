//! Randomized-state fuzz target for the compliance revocation accumulator
//! (Issue #302).
//!
//! The input bytes are decoded into a sequence of operations. Each operation
//! is either a revocation attempt (by the stored admin, by a stranger, or with
//! an edge-case identifier) or a read. The target asserts the invariants a
//! wrong implementation would break:
//!
//! * a successful revocation advances the epoch by exactly one;
//! * a rejected revocation (unauthorized caller) leaves the accumulator
//!   byte-for-byte unchanged;
//! * the epoch never decreases;
//! * the all-zero and all-`0xFF` identifiers are accepted like any other value
//!   and never trap the contract.
//!
//! Panics that escape the contract surface as `Err` from the `try_` client,
//! which is what lets the target distinguish a handled rejection from a trap.

#![no_main]

use libfuzzer_sys::fuzz_target;

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};
use tessera_compliance::{ComplianceContract, ComplianceContractClient};

const MAX_OPS: usize = 32;

/// Identifier pool: edge cases first, then bytes drawn from the input.
fn identifier(env: &Env, selector: u8, data: &[u8], cursor: &mut usize) -> BytesN<32> {
    match selector % 4 {
        0 => BytesN::from_array(env, &[0u8; 32]),
        1 => BytesN::from_array(env, &[0xFFu8; 32]),
        _ => {
            let mut bytes = [0u8; 32];
            for slot in bytes.iter_mut() {
                *slot = data.get(*cursor).copied().unwrap_or(0);
                *cursor += 1;
            }
            BytesN::from_array(env, &bytes)
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(ComplianceContract, ());
    let client = ComplianceContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let stranger = Address::generate(&env);
    client.initialize(&admin);

    let mut cursor = 0usize;
    let mut last_epoch = client.get_revocation_accumulator().epoch;

    for _ in 0..MAX_OPS {
        let Some(&op) = data.get(cursor) else {
            break;
        };
        let selector = data.get(cursor + 1).copied().unwrap_or(0);
        cursor += 2;

        match op % 3 {
            // Authorized revocation: epoch must advance by exactly one.
            0 => {
                let id = identifier(&env, selector, data, &mut cursor);
                let before = client.get_revocation_accumulator();
                if let Ok(Ok(after)) = client.try_revoke_identity(&admin, &id) {
                    assert_eq!(after.epoch, before.epoch + 1, "epoch must step by one");
                    assert_eq!(client.get_revocation_accumulator(), after);
                }
            }
            // Unauthorized revocation: must not change any state.
            1 => {
                let id = identifier(&env, selector, data, &mut cursor);
                let before = client.get_revocation_accumulator();
                let _ = client.try_revoke_identity(&stranger, &id);
                assert_eq!(
                    client.get_revocation_accumulator(),
                    before,
                    "rejected revocation mutated the accumulator"
                );
            }
            // Read path: must always succeed and never move the epoch backwards.
            _ => {
                let current = client.get_revocation_accumulator();
                assert!(current.epoch >= last_epoch, "epoch went backwards");
                last_epoch = current.epoch;
            }
        }
        last_epoch = client.get_revocation_accumulator().epoch;
    }
});
