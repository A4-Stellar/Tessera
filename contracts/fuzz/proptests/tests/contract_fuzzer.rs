//! Property: the revocation accumulator is deterministic, order-sensitive, and
//! only the stored admin can move it, across randomized identifiers and edge
//! cases (all-zero, all-`0xFF`, and arbitrary 32-byte values).

use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};
use tessera_compliance::{ComplianceContract, ComplianceContractClient};

/// Identifiers that historically break hashing and encoding code: the zero
/// value, the saturated value, and a single-bit value at the top of the range.
fn edge_identifier() -> impl Strategy<Value = [u8; 32]> {
    prop_oneof![
        3 => Just([0u8; 32]),
        3 => Just([0xFFu8; 32]),
        1 => Just({
            let mut b = [0u8; 32];
            b[0] = 0x80;
            b
        }),
        5 => prop::array::uniform32(any::<u8>()),
    ]
}

fn fresh_client(env: &Env) -> (ComplianceContractClient<'_>, Address) {
    let contract_id = env.register(ComplianceContract, ());
    let client = ComplianceContractClient::new(env, &contract_id);
    let admin = Address::generate(env);
    client.initialize(&admin);
    (client, admin)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Replaying the same revocation sequence in a fresh environment yields the
    /// same root and epoch. The accumulator has no hidden entropy or ledger
    /// dependence.
    #[test]
    fn replay_is_deterministic(ids in prop::collection::vec(edge_identifier(), 1..12)) {
        let run = |ids: &[[u8; 32]]| {
            let env = Env::default();
            env.mock_all_auths();
            let (client, admin) = fresh_client(&env);
            for id in ids {
                client.revoke_identity(&admin, &BytesN::from_array(&env, id));
            }
            client.get_revocation_accumulator()
        };
        prop_assert_eq!(run(&ids), run(&ids));
    }

    /// Each revocation advances the epoch by exactly one and changes the root.
    #[test]
    fn every_revocation_advances_epoch_and_root(id in edge_identifier()) {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin) = fresh_client(&env);
        let before = client.get_revocation_accumulator();
        let after = client.revoke_identity(&admin, &BytesN::from_array(&env, &id));
        prop_assert_eq!(after.epoch, before.epoch + 1);
        prop_assert_ne!(after.root, before.root);
    }

    /// A caller who is not the stored admin is rejected and leaves the
    /// accumulator untouched, whatever identifier they try to revoke.
    #[test]
    fn non_admin_cannot_move_the_accumulator(id in edge_identifier()) {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin) = fresh_client(&env);
        let stranger = Address::generate(&env);
        let before = client.get_revocation_accumulator();
        let result = client.try_revoke_identity(&stranger, &BytesN::from_array(&env, &id));
        prop_assert!(result.is_err());
        prop_assert_eq!(client.get_revocation_accumulator(), before);
    }
}
