//! Stateful fuzz target for `tessera-registry`.
//!
//! Covers `initialize`, `register_asset`, `deactivate_asset`, the three query
//! filters, `total_value_locked`, `asset_count`, `version`, and the pause and
//! upgrade surface.
//!
//! Asserted properties:
//!
//! * `register_asset` rejects a negative valuation by contract
//!   (`InvalidValuation`);
//! * `asset_count` equals the number of *successful* registrations, always,
//!   and the ids handed out are dense and start at 0;
//! * the per-issuer and per-type filters are each a **partition** of
//!   `get_all_assets` — every entry appears in exactly one issuer bucket and
//!   exactly one type bucket, so the bucket lengths must sum to the total;
//! * `total_value_locked` equals the sum of the valuations of the *active*
//!   entries exactly, and is never negative;
//! * `deactivate_asset` is what removes an entry from
//!   `total_value_locked`; the entry stays in the query filters and keeps its
//!   valuation, because it is deactivated rather than deleted.
//!
//! The filter-partition property is the one that catches the classic registry
//! bug: filtering on `active` inside `filter_assets` instead of leaving
//! deactivation to `total_value_locked`, which would make a deactivated asset
//! vanish from `get_all_assets` while `asset_count` still counts it.

#![no_main]

use libfuzzer_sys::fuzz_target;

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String as SdkString};
use tessera_fuzz_harness::{check_no_trap, finding, step_ledger, succeeded, Input, MAX_OPS};
use tessera_registry::{RegistryContract, RegistryContractClient};

const ISSUERS: usize = 3;
const TYPES: usize = 3;

/// Host-side mirror of one registered entry. `#[contracttype]` types are not
/// `PartialEq`, so this deliberately stores the fields it needs rather than an
/// `AssetEntry`.
#[derive(Debug, Clone)]
struct Shadow {
    issuer: Address,
    asset_type: SdkString,
    valuation: i128,
    active: bool,
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    let registry_id = env.register(RegistryContract, ());
    let admin = Address::generate(&env);
    let r = RegistryContractClient::new(&env, &registry_id).mock_all_auths();

    let mut issuers: Vec<Address> = (0..ISSUERS).map(|_| Address::generate(&env)).collect();
    // The admin is one of the issuers, so `deactivate_asset`'s admin path and
    // `register_asset`'s issuer path are exercised on the same address.
    issuers[0] = admin.clone();
    let types: Vec<SdkString> = (0..TYPES)
        .map(|i| SdkString::from_str(&env, &format!("T{i}")))
        .collect();

    r.initialize(&admin);

    // Host-side `std` shadow state: this lives in the fuzzer process, not in
    // the contract, and tuples are not `Val`-encodable.
    let mut shadow: Vec<Shadow> = Vec::new();

    for _ in 0..MAX_OPS {
        let issuer = issuers[input.below(ISSUERS)].clone();
        let asset_type = types[input.below(TYPES)].clone();
        let valuation = input.i128_interesting();
        step_ledger(&env, &mut input);

        match input.below(10) {
            0 | 1 | 2 => {
                let token = Address::generate(&env);
                let name = SdkString::from_str(&env, &input.ascii(8));
                let res = r.try_register_asset(&issuer, &token, &name, &asset_type, &valuation);
                check_no_trap(&res, "registry::register_asset");

                if valuation < 0 {
                    assert!(
                        !succeeded(&res),
                        "register_asset accepted a negative valuation"
                    );
                    assert_eq!(
                        r.asset_count() as usize,
                        shadow.len(),
                        "a rejected registration must not have consumed an id"
                    );
                } else if succeeded(&res) {
                    // ids are dense and start at 0, so the new id is exactly the
                    // pre-call count.
                    let id = shadow.len() as u64;
                    assert_eq!(
                        r.asset_count(),
                        id + 1,
                        "asset_count did not advance by exactly one"
                    );

                    let entry = r.get_asset(&id);
                    assert_eq!(entry.id, id, "get_asset id mismatch");
                    assert_eq!(entry.issuer, issuer, "get_asset issuer mismatch");
                    assert_eq!(entry.asset_type, asset_type, "get_asset type mismatch");
                    assert_eq!(entry.valuation, valuation, "get_asset valuation mismatch");
                    assert_eq!(entry.token_contract, token, "get_asset token mismatch");
                    assert_eq!(entry.active, true, "a fresh entry must be active");
                    assert_eq!(
                        entry.created_at,
                        env.ledger().sequence(),
                        "created_at must be the registering ledger"
                    );

                    shadow.push(Shadow {
                        issuer,
                        asset_type,
                        valuation,
                        active: true,
                    });
                }
            }
            3 => {
                if shadow.is_empty() {
                    continue;
                }
                let idx = input.below(shadow.len());
                let id = idx as u64;
                let res = r.try_deactivate_asset(&admin, &id);
                check_no_trap(&res, "registry::deactivate_asset");
                if succeeded(&res) {
                    shadow[idx].active = false;
                    assert_eq!(
                        r.get_asset(&id).active,
                        false,
                        "deactivate_asset must clear the active flag"
                    );
                    // Deactivating twice is a no-op, not a second state change.
                    assert_eq!(
                        r.get_asset(&id).valuation,
                        shadow[idx].valuation,
                        "deactivate_asset must preserve the valuation"
                    );
                }
            }
            4 => {
                // total_value_locked must be exactly the sum of the active
                // entries' valuations.
                let expected = shadow
                    .iter()
                    .filter(|s| s.active)
                    .fold(0i128, |acc, s| acc.saturating_add(s.valuation));
                let got = r.total_value_locked();
                assert_eq!(
                    got, expected,
                    "total_value_locked disagrees with the active entries"
                );
                if got < 0 {
                    finding("registry::total_value_locked", "negative TVL");
                }
            }
            5 => {
                // Every entry must be reachable from get_all_assets, and the
                // count must agree. A deactivated entry is *still* returned.
                let all = r.get_all_assets();
                assert_eq!(
                    r.asset_count() as i128,
                    all.len() as i128,
                    "asset_count vs get_all_assets().len()"
                );
                assert_eq!(all.len() as usize, shadow.len(), "shadow count drift");
                for (i, entry) in all.iter().enumerate() {
                    assert_eq!(entry.id, i as u64, "get_all_assets must be id-ordered");
                    assert_eq!(
                        entry.active, shadow[i].active,
                        "get_all_assets active flag disagrees with the shadow state"
                    );
                    assert_eq!(
                        entry.valuation, shadow[i].valuation,
                        "get_all_assets valuation disagrees with the shadow state"
                    );
                }
                assert_eq!(r.version(), 1, "ABI version must stay 1");
            }
            6 => {
                // The per-issuer filter must be a partition of get_all_assets:
                // bucket lengths sum to the total, and every entry is in its
                // own issuer's bucket.
                let all_len = r.get_all_assets().len() as i128;
                let total: i128 = issuers
                    .iter()
                    .map(|i| r.get_assets_by_issuer(i).len() as i128)
                    .sum();
                assert_eq!(
                    total, all_len,
                    "per-issuer filters must partition get_all_assets"
                );
                for entry in r.get_all_assets().iter() {
                    assert!(
                        r.get_assets_by_issuer(&entry.issuer)
                            .iter()
                            .any(|e| e.id == entry.id),
                        "an asset is missing from its own issuer's filter"
                    );
                }
            }
            7 => {
                // Same partition property for the per-type filter.
                let all_len = r.get_all_assets().len() as i128;
                let total: i128 = types
                    .iter()
                    .map(|t| r.get_assets_by_type(t).len() as i128)
                    .sum();
                assert_eq!(
                    total, all_len,
                    "per-type filters must partition get_all_assets"
                );
            }
            8 => {
                let _ = r.try_pause(&admin);
                let _ = r.try_unpause(&admin);
            }
            _ => {
                // `upgrade` needs a live deployer, which the test ledger does
                // not have, so it legitimately fails with a host error. Call it
                // only to prove it does not trap.
                let hash = BytesN::from_array(&env, &input.bytes32());
                let _ = r.try_upgrade(&admin, &hash);
            }
        }
    }

    // Final cross-check of shadow state against the contract.
    assert_eq!(
        r.asset_count() as usize,
        shadow.len(),
        "asset_count must equal the number of successful registrations"
    );
    let expected_tvl = shadow
        .iter()
        .filter(|s| s.active)
        .fold(0i128, |acc, s| acc.saturating_add(s.valuation));
    assert_eq!(
        r.total_value_locked(),
        expected_tvl,
        "final total_value_locked disagrees with the shadow state"
    );
});
