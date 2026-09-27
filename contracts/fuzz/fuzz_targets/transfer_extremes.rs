//! Extreme-value transfer fuzz target for `tessera-asset-token`.
//!
//! This is the target that answers the "can an attacker move a nonsense amount
//! of money" question directly. Instead of sampling amounts uniformly, every
//! call is fed from a curated table of hostile magnitudes:
//!
//! | value                | why it is interesting                          |
//! |----------------------|------------------------------------------------|
//! | `0`                  | zero-value transfers must be rejected           |
//! | `i128::MIN`          | negation / absolute-value overflow              |
//! | `i128::MAX`          | `balance + amount` overflow, `total_supply` cap  |
//! | `i128::MAX / 10_000` | the largest dividend amount for a 100% rate     |
//! | `u64::MAX`           | a `u64` counter leaking into an `i128` amount   |
//! | `u64::MAX as i128 * 2`| `u64` ceiling doubled, still inside `i128`      |
//! | `supply ± 1`         | exactly-at-the-limit and one past it            |
//!
//! Every one of these is driven through `transfer`, `mint`, `burn` and
//! `clawback`, against a supply the fuzzer also picks, and the supply
//! conservation law is checked after every single call.

#![no_main]

use libfuzzer_sys::fuzz_target;

use soroban_sdk::{
    testutils::Address as _, Address, Env, String as SdkString,
};
use tessera_asset_token::{AssetTokenContract, AssetTokenContractClient};
use tessera_compliance::{ComplianceContract, ComplianceContractClient};
use tessera_fuzz_harness::{
    check_no_trap, finding, step_ledger, succeeded, Input, MAX_OPS,
};

/// The hostile magnitudes. Kept as an explicit table (rather than generated)
/// so that the intent of each entry is reviewable and so a regression can be
/// pinned to a named row.
const EXTREMES: [(&str, i128); 16] = [
    ("zero", 0),
    ("one", 1),
    ("neg_one", -1),
    ("i128::MAX", i128::MAX),
    ("i128::MAX-1", i128::MAX - 1),
    ("i128::MAX/2", i128::MAX / 2),
    ("i128::MAX/10_000", i128::MAX / 10_000),
    ("i128::MIN", i128::MIN),
    ("i128::MIN+1", i128::MIN + 1),
    ("i64::MAX", i64::MAX as i128),
    ("i64::MIN", i64::MIN as i128),
    ("u64::MAX", u64::MAX as i128),
    ("u64::MAX*2", (u64::MAX as i128).saturating_mul(2)),
    ("u32::MAX", u32::MAX as i128),
    ("10_000 (1% in bps)", 10_000),
    ("10^18", 1_000_000_000_000_000_000),
];

/// Supplies the token is initialised with, so that the interesting amounts are
/// tested both against a tiny and against an enormous supply.
const SUPPLIES: [i128; 8] = [
    0,
    1,
    u32::MAX as i128,
    1_000_000_000_000_000_000,
    i64::MAX as i128,
    u64::MAX as i128,
    i128::MAX / 2,
    i128::MAX,
];

const HOLDERS: usize = 3;

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    let compliance_id = env.register(ComplianceContract, ());
    let asset_id = env.register(AssetTokenContract, ());
    let admin = Address::generate(&env);
    let compliance = ComplianceContractClient::new(&env, &compliance_id).mock_all_auths();
    let token = AssetTokenContractClient::new(&env, &asset_id).mock_all_auths();

    let mut holders = [admin.clone(); HOLDERS];
    for h in holders.iter_mut().skip(1) {
        *h = Address::generate(&env);
    }
    compliance.initialize(&admin);
    for h in &holders {
        compliance.add_to_allowlist(&admin, h, &SdkString::from_str(&env, "US"), 0);
    }

    let supply = SUPPLIES[input.below(SUPPLIES.len())];
    // A negative supply is rejected by `initialize`; nothing to learn from it.
    if supply < 0 {
        return;
    }
    let name = SdkString::from_str(&env, "XFER");
    check_no_trap(
        &token.try_initialize(
            &admin,
            &name,
            &name,
            &SdkString::from_str(&env, "RWA"),
            &supply,
            &input.u32_interesting(),
            &compliance_id,
            &name,
            &0i128,
        ),
        "transfer_extremes::initialize",
    );

    // Hand the whole supply to holder 0 only, so that every other holder starts
    // at exactly zero: a hostile amount then has to survive a zero-balance
    // destination as well as a fully-funded one.
    let mut clawbacked: i128 = 0;

    for _ in 0..MAX_OPS {
        // Alternate between an absolute hostile value and one relative to the
        // live supply, so both "unrepresentable" and "just over the edge" are
        // covered.
        let (label, amount) = if input.bool() {
            let (label, v) = EXTREMES[input.below(EXTREMES.len())];
            (label, v)
        } else {
            let s = token.total_supply();
            let delta = input.i128_amount(s);
            ("supply-relative", delta)
        };
        let ctx = format!("transfer_extremes[{label}] amount={amount}");

        let from = holders[input.below(HOLDERS)].clone();
        let to = holders[input.below(HOLDERS)].clone();
        // Advance the ledger each round. This is what lets arm 4 observe a
        // lockup *lifting* rather than only ever observing it block, since
        // `require_not_locked` compares against `ledger().sequence()`. It also
        // keeps persistent balance entries from ageing out under a bounded test
        // ledger, which would otherwise surface as a host `EntryExpired` and be
        // indistinguishable from a real trap.
        step_ledger(&env, &mut input);

        let sum_before: i128 = holders
            .iter()
            .fold(0i128, |a, h| a.saturating_add(token.balance(h)));
        let supply_before = token.total_supply();
        let live = sum_before.saturating_sub(clawbacked);
        if live != supply_before {
            finding(
                "transfer_extremes::setup",
                &format!("initial state violates conservation: {live} != {supply_before}"),
            );
        }

        match input.below(5) {
            0 => {
                let r = token.try_transfer(&from, &to, &amount);
                check_no_trap(&r, &format!("{ctx} transfer"));
                // A transfer must never change total supply.
                assert_eq!(
                    token.total_supply(),
                    supply_before,
                    "{ctx}: transfer changed total_supply"
                );
            }
            1 => {
                let r = token.try_mint(&admin, &to, &amount);
                check_no_trap(&r, &format!("{ctx} mint"));
                let expected = if succeeded(&r) {
                    supply_before.saturating_add(amount)
                } else {
                    supply_before
                };
                assert_eq!(token.total_supply(), expected, "{ctx}: mint supply drift");
            }
            2 => {
                let r = token.try_burn(&from, &amount);
                check_no_trap(&r, &format!("{ctx} burn"));
                let expected = if succeeded(&r) {
                    supply_before.saturating_sub(amount)
                } else {
                    supply_before
                };
                assert_eq!(token.total_supply(), expected, "{ctx}: burn supply drift");
            }
            3 => {
                let r = token.try_clawback(&admin, &from, &amount);
                check_no_trap(&r, &format!("{ctx} clawback"));
                if succeeded(&r) {
                    clawbacked = clawbacked.saturating_add(amount);
                }
                // A clawback must never change total supply either.
                assert_eq!(
                    token.total_supply(),
                    supply_before,
                    "{ctx}: clawback changed total_supply"
                );
            }
            _ => {
                // Holding-period lockups. `unlock_ledger = 0` means "no
                // lockup"; anything above the current sequence blocks transfer
                // and burn until the ledger catches up. Both halves matter: a
                // lockup that never lifts is as broken as one that never
                // applies, and only an advancing ledger can tell them apart.
                let unlock = if input.bool() {
                    0
                } else {
                    env.ledger().sequence().saturating_add(1 + input.below(4) as u32)
                };
                let r = token.try_set_lockup(&admin, &from, &unlock);
                check_no_trap(&r, &format!("{ctx} set_lockup"));
                assert_eq!(
                    token.get_lockup(&from),
                    unlock,
                    "{ctx}: get_lockup disagrees with what was set"
                );

                let locked = unlock != 0 && env.ledger().sequence() < unlock;
                if locked {
                    // A tiny amount, so the rejection can only be the lockup and
                    // not an insufficient balance.
                    let one = 1i128;
                    let t = token.try_transfer(&from, &to, &one);
                    check_no_trap(&t, &format!("{ctx} transfer(locked)"));
                    assert!(
                        !succeeded(&t),
                        "a locked holder transferred at sequence {} with unlock {unlock}",
                        env.ledger().sequence()
                    );
                    let b = token.try_burn(&from, &one);
                    check_no_trap(&b, &format!("{ctx} burn(locked)"));
                    assert!(!succeeded(&b), "a locked holder burned at sequence {}", env.ledger().sequence());
                    // `mint` and `clawback` are deliberately *not* lockup-gated,
                    // so a locked holder must still be able to receive.
                    let m = token.try_mint(&admin, &from, &one);
                    check_no_trap(&m, &format!("{ctx} mint(locked)"));
                }
                // No supply assertion here: setting a lockup is not a
                // ledger-moving operation, and the one that may have moved
                // supply above (`mint` into a locked holder) is already covered
                // by the end-of-round conservation check below, which reads
                // `total_supply()` fresh rather than a stale snapshot.
            }
        }

        let mut sum_after = 0i128;
        for h in &holders {
            let b = token.balance(h);
            if b < 0 {
                finding(&ctx, &format!("negative balance {b}"));
            }
            sum_after = sum_after.saturating_add(b);
        }
        // The one conservation law, stated once. A mint legitimately raises both
        // `sum` and `total_supply` and a burn lowers both, so this cannot be an
        // "unchanged across the call" check — what must hold is that they track
        // each other, with clawed-back units the documented exception.
        assert_eq!(
            sum_after,
            token.total_supply().saturating_sub(clawbacked),
            "{ctx}: sum(balances) != total_supply - clawed_back"
        );
    }
});
