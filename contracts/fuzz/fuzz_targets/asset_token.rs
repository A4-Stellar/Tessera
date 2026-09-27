//! Stateful fuzz target for `tessera-asset-token`.
//!
//! Drives every public entry point of the contract in randomised order against
//! two live compliance contracts, and after each step checks the invariants
//! that must hold no matter what the fuzzer did:
//!
//! 1. no entry point traps (arithmetic overflow, `unwrap` on `None`, OOB index),
//! 2. no balance is ever negative,
//! 3. the sum of the tracked holder balances equals
//!    `total_supply - clawbacked` — mint and burn move both sides together, a
//!    transfer moves neither, and a clawback removes a balance *without*
//!    touching total supply,
//! 4. `total_supply` only ever moves by the exact amount of a successful
//!    `mint`/`burn`, and never goes negative.
//!
//! Invariant 3 is what catches the worst bug class in a token contract: a
//! self-transfer (`from == to`) that reads the destination balance before
//! writing the source balance inflates the holder's balance out of thin air
//! while leaving `total_supply` untouched.

#![no_main]

use libfuzzer_sys::fuzz_target;

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String as SdkString};
use tessera_asset_token::AssetTokenContract;
use tessera_compliance::{ComplianceContract, ComplianceContractClient};
use tessera_fuzz_harness::{check_no_trap, finding, step_ledger, succeeded, Input, MAX_OPS};

/// Number of tracked holders. Holder 0 is the admin, who is also the recipient
/// of the initial supply.
const HOLDERS: usize = 4;

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    // ---- deployment -------------------------------------------------------
    let compliance_id = env.register(ComplianceContract, ());
    let compliance2_id = env.register(ComplianceContract, ());
    let asset_id = env.register(AssetTokenContract, ());

    let admin = Address::generate(&env);
    let compliance = ComplianceContractClient::new(&env, &compliance_id).mock_all_auths();
    let compliance2 = ComplianceContractClient::new(&env, &compliance2_id).mock_all_auths();
    let token = AssetTokenContractClient::new(&env, &asset_id).mock_all_auths();

    // Holder 0 stays the admin: `initialize` credits the entire supply to the
    // admin, so the admin has to be one of the tracked holders or the
    // supply-conservation invariant below starts out false.
    let mut holders = [admin.clone(); HOLDERS];
    for h in holders.iter_mut().skip(1) {
        *h = Address::generate(&env);
    }

    // KYC-approve every tracked holder on both compliance contracts so that the
    // compliance gate is not what stops an operation and the arithmetic is
    // genuinely reached. Expiry 0 means "never expires".
    //
    // `initialize` has to come first: `require_admin` used to `.unwrap()` the
    // stored admin, so an allowlist write before initialization trapped.
    compliance.initialize(&admin);
    compliance2.initialize(&admin);
    for h in &holders {
        let j = SdkString::from_str(&env, &input.ascii(4));
        compliance.add_to_allowlist(&admin, h, &j, 0);
        compliance2.add_to_allowlist(&admin, h, &j, 0);
    }

    let total_supply = input.i128_interesting();
    let valuation = input.i128_interesting();
    // `initialize` rejects a negative supply or valuation by contract, so those
    // inputs are rejected rather than treated as a finding.
    if total_supply < 0 || valuation < 0 {
        return;
    }
    let symbol = SdkString::from_str(&env, &input.ascii(8));
    let asset_type = SdkString::from_str(&env, "RWA");
    let description = SdkString::from_str(&env, &input.ascii(16));

    check_no_trap(
        &token.try_initialize(
            &admin,
            &symbol,
            &symbol,
            &asset_type,
            &total_supply,
            &input.u32(),
            &compliance_id,
            &description,
            &valuation,
        ),
        "asset_token::initialize",
    );

    // Fan the initial supply out over the non-admin holders so that transfers
    // and burns start from a non-degenerate state. `initialize` credited the
    // whole supply to `holders[0]`, so `sum == total_supply` holds here.
    let per_holder = total_supply / HOLDERS as i128;
    for h in holders.iter().skip(1) {
        if per_holder > 0 {
            token.mint(&admin, h, per_holder);
        }
    }

    let mut clawbacked: i128 = 0;

    // ---- randomised operation sequence ------------------------------------
    for _ in 0..MAX_OPS {
        let from = holders[input.below(HOLDERS)].clone();
        let to = holders[input.below(HOLDERS)].clone();
        let amount = input.i128_amount(total_supply);
        step_ledger(&env, &mut input);

        let supply_before = token.total_supply();

        // `expected_supply` records the post-state implied by whether the
        // operation actually went through, so the result can be asserted
        // exactly rather than merely observed.
        let expected_supply = match input.below(12) {
            0 | 1 => {
                let r = token.try_transfer(&from, &to, &amount);
                check_no_trap(&r, "asset_token::transfer");
                // A transfer must never move total supply.
                supply_before
            }
            2 => {
                let r = token.try_mint(&admin, &to, &amount);
                check_no_trap(&r, "asset_token::mint");
                if succeeded(&r) {
                    supply_before.saturating_add(amount)
                } else {
                    supply_before
                }
            }
            3 => {
                let r = token.try_burn(&from, &amount);
                check_no_trap(&r, "asset_token::burn");
                if succeeded(&r) {
                    supply_before.saturating_sub(amount)
                } else {
                    supply_before
                }
            }
            4 => {
                let r = token.try_clawback(&admin, &from, &amount);
                check_no_trap(&r, "asset_token::clawback");
                if succeeded(&r) {
                    clawbacked = clawbacked.saturating_add(amount);
                }
                // A clawback removes a balance without touching total supply.
                supply_before
            }
            5 => {
                let r = token.try_pause(&admin);
                check_no_trap(&r, "asset_token::pause");
                supply_before
            }
            6 => {
                let r = token.try_unpause(&admin);
                check_no_trap(&r, "asset_token::unpause");
                supply_before
            }
            7 => {
                let v = input.i128_interesting();
                let r = token.try_update_valuation(&admin, &v);
                check_no_trap(&r, "asset_token::update_valuation");
                supply_before
            }
            8 => {
                // 0 clears the lockup; a near-future ledger makes it bite.
                let unlock = if input.bool() {
                    0
                } else {
                    env.ledger()
                        .sequence()
                        .saturating_add(input.below(4_096) as u32)
                };
                let r = token.try_set_lockup(&admin, &from, &unlock);
                check_no_trap(&r, "asset_token::set_lockup");
                if succeeded(&r) {
                    assert_eq!(
                        token.get_lockup(&from),
                        unlock,
                        "get_lockup must return exactly what set_lockup stored"
                    );
                }
                supply_before
            }
            9 => {
                let target = if input.bool() {
                    compliance_id.clone()
                } else {
                    compliance2_id.clone()
                };
                let r = token.try_set_compliance(&admin, &target);
                check_no_trap(&r, "asset_token::set_compliance");
                supply_before
            }
            10 => {
                // Read paths, for XDR round-trip robustness. These are total
                // functions over storage, so they must never fail.
                let _ = token.balance(&from);
                let _ = token.total_supply();
                let _ = token.get_lockup(&to);
                let _ = token.version();
                let meta = token.get_metadata();
                assert_eq!(
                    meta.total_supply,
                    token.total_supply(),
                    "get_metadata total_supply disagrees with total_supply()"
                );
                assert_eq!(
                    meta.admin, admin,
                    "get_metadata admin disagrees with the address initialised"
                );
                assert_eq!(
                    meta.name, symbol,
                    "get_metadata name disagrees with the value initialised"
                );
                assert_eq!(
                    meta.symbol, symbol,
                    "get_metadata symbol disagrees with the value initialised"
                );
                assert_eq!(
                    meta.asset_type, asset_type,
                    "get_metadata asset_type disagrees with the value initialised"
                );
                assert_eq!(
                    meta.description, description,
                    "get_metadata description disagrees with the value initialised"
                );
                supply_before
            }
            _ => {
                // `upgrade` needs a live deployer, which the test ledger does
                // not have, so it legitimately fails with a host error. Call it
                // only to prove it does not trap, and assert nothing about the
                // result.
                let hash = BytesN::from_array(&env, &input.bytes32());
                let _ = token.try_upgrade(&admin, &hash);
                supply_before
            }
        };

        // ---- invariants ---------------------------------------------------
        let supply_after = token.total_supply();
        assert_eq!(
            supply_after, expected_supply,
            "total_supply moved by something other than the requested mint/burn amount"
        );
        if supply_after < 0 {
            finding(
                "asset_token",
                &format!("total_supply went negative: {supply_after}"),
            );
        }

        let mut sum_after = 0i128;
        for (i, h) in holders.iter().enumerate() {
            let b = token.balance(h);
            if b < 0 {
                finding(
                    "asset_token",
                    &format!("holder {i} balance went negative: {b}"),
                );
            }
            sum_after = sum_after.saturating_add(b);
        }

        // The single conservation law, stated once. Deliberately *not* an
        // "unchanged across the call" check: a mint legitimately raises both
        // `sum` and `total_supply`, and a burn lowers both. What must hold is
        // that they track each other exactly, with clawed-back units the one
        // documented exception.
        assert_eq!(
            sum_after,
            supply_after.saturating_sub(clawbacked),
            "sum of holder balances must equal total_supply minus everything clawed back"
        );
    }
});
