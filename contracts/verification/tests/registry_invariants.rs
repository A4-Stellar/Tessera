//! Formal invariant: the registry's `total_value_locked` always equals the
//! sum of every *active* asset's valuation (issue #296).
//!
//! `total_value_locked` recomputes this from storage on every call rather
//! than maintaining a running counter, so this test's real job is catching a
//! future refactor that moves it to an incrementally-maintained total and
//! gets the increment/decrement wrong on `deactivate_asset` -- exactly the
//! kind of bug a fixed set of example-based tests would miss and a long
//! randomized sequence of register/deactivate calls reliably finds.
//!
//! `ProptestConfig::cases` is raised well past the default so the suite
//! actually runs the "thousands of simulated transaction sequences" the
//! issue's acceptance criteria asks for, each itself a chain of up to 40
//! register/deactivate operations.

use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, Address, Env, String as SdkString};
use tessera_registry::{AssetEntry, RegistryContract, RegistryContractClient};
use tessera_verification::{registry_op_sequence, RegistryOp};

fn setup(env: &Env) -> (RegistryContractClient<'static>, Address) {
    let registry_id = env.register(RegistryContract, ());
    let registry = RegistryContractClient::new(env, &registry_id).mock_all_auths();
    let admin = Address::generate(env);
    registry.initialize(&admin);
    (registry, admin)
}

/// Sum of `valuation` over every asset currently marked `active`, read
/// straight from `get_all_assets` -- an independent recomputation of the
/// invariant, deliberately not calling `total_value_locked` itself.
fn expected_tvl(all_assets: &soroban_sdk::Vec<AssetEntry>) -> i128 {
    all_assets
        .iter()
        .filter(|entry| entry.active)
        .map(|entry| entry.valuation)
        .fold(0i128, |acc, v| acc.saturating_add(v))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2048, .. ProptestConfig::default() })]

    #[test]
    fn total_value_locked_matches_sum_of_active_valuations(
        ops in registry_op_sequence(40),
    ) {
        let env = Env::default();
        let (registry, admin) = setup(&env);
        let mut registered_ids: Vec<u64> = Vec::new();

        for op in ops {
            match op {
                RegistryOp::Register { valuation } => {
                    let issuer = Address::generate(&env);
                    let token_contract = Address::generate(&env);
                    let name = SdkString::from_str(&env, "Asset");
                    let asset_type = SdkString::from_str(&env, "real-estate");

                    let id = registry.register_asset(
                        &issuer,
                        &token_contract,
                        &name,
                        &asset_type,
                        &valuation,
                    );
                    registered_ids.push(id);
                }
                RegistryOp::Deactivate { index_into_registered } => {
                    if registered_ids.is_empty() {
                        continue;
                    }
                    let id = registered_ids[index_into_registered % registered_ids.len()];
                    // Idempotent in effect either way -- re-deactivating an
                    // already-inactive asset must not double-subtract it, which
                    // is exactly the kind of bug an incrementally-maintained
                    // total (rather than the current recompute-from-storage
                    // approach) could introduce.
                    registry.deactivate_asset(&admin, &id);
                }
            }

            let all_assets = registry.get_all_assets();
            let expected = expected_tvl(&all_assets);
            let actual = registry.total_value_locked();

            prop_assert_eq!(
                actual,
                expected,
                "total_value_locked() diverged from sum(active valuations) after {:?}",
                registered_ids,
            );
        }
    }

    #[test]
    fn asset_count_never_shrinks_on_deactivation(
        ops in registry_op_sequence(40),
    ) {
        // A second, independent invariant on the same state machine:
        // deactivating an asset must never remove it from the id list, only
        // flip its `active` flag -- `asset_count` tracks "ever registered",
        // not "currently active".
        let env = Env::default();
        let (registry, admin) = setup(&env);
        let mut registered_ids: Vec<u64> = Vec::new();
        let mut previous_count = 0u64;

        for op in ops {
            match op {
                RegistryOp::Register { valuation } => {
                    let issuer = Address::generate(&env);
                    let token_contract = Address::generate(&env);
                    let name = SdkString::from_str(&env, "Asset");
                    let asset_type = SdkString::from_str(&env, "real-estate");

                    let id = registry.register_asset(
                        &issuer,
                        &token_contract,
                        &name,
                        &asset_type,
                        &valuation,
                    );
                    registered_ids.push(id);
                }
                RegistryOp::Deactivate { index_into_registered } => {
                    if registered_ids.is_empty() {
                        continue;
                    }
                    let id = registered_ids[index_into_registered % registered_ids.len()];
                    registry.deactivate_asset(&admin, &id);
                }
            }

            let count = registry.asset_count();
            prop_assert!(
                count >= previous_count,
                "asset_count() shrank from {previous_count} to {count}"
            );
            previous_count = count;
        }

        prop_assert_eq!(previous_count, registered_ids.len() as u64);
    }
}
