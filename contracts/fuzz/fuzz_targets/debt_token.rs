//! Fuzz target for `debt-token` (`DebtTokenAmortization`).
//!
//! The property that matters is **waterfall conservation**: a repayment retires
//! debt in `Senior` → `Mezzanine` → `Equity` order, never retires more than is
//! outstanding, and leaves no tranche negative. That is exactly what the
//! original implementation violated in three separate ways, each reachable from
//! a public entry point with caller-chosen arguments:
//!
//! * a negative `amount` was accepted and *increased* every tranche's debt,
//!   because each leg stored `owed - payment` with `payment` itself negative;
//! * the `Equity` leg took the whole uncapped remainder, so repaying more than
//!   was outstanding underflowed `equity_owed - equity_payment` and trapped;
//! * `accrue_interest` had no auth check at all, so anyone could inflate any
//!   tranche, and its `owed + amount` was unchecked.
//!
//! The contract has been fixed for all three. The assertions here are the
//! proof, and the host-side mirror replays the waterfall independently of the
//! contract so that agreement is a real cross-check rather than a restatement.
//! Reverting any one of the three fixes reproduces the original crash.
//!
//! `i128::MIN` and `i128::MAX` are fed in directly: they are the sharpest cases
//! for `owed - payment` and `owed + amount` respectively.

#![no_main]

use libfuzzer_sys::fuzz_target;

use debt_token::{DebtTokenAmortization, DebtTokenAmortizationClient, Tranche};
use soroban_sdk::{testutils::Address as _, Address, Env};
use tessera_fuzz_harness::{check_no_trap, finding, step_ledger, succeeded, Input, MAX_OPS};

/// Payment-priority order. The index into the host-side mirror is the same.
const TRANCHES: [Tranche; 3] = [Tranche::Senior, Tranche::Mezzanine, Tranche::Equity];

/// Amounts chosen to sit either side of the `i128` ceiling, because the
/// overflow paths are the point.
fn hostile_amount(input: &mut Input<'_>) -> i128 {
    match input.below(6) {
        0 => i128::MIN,
        1 => i128::MAX,
        2 => 1,
        3 => -1,
        4 => i128::MIN + 1,
        _ => input.i128_interesting(),
    }
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    let debt_id = env.register(DebtTokenAmortization, ());
    let admin = Address::generate(&env);
    let borrower = Address::generate(&env);
    let impostor = Address::generate(&env);

    let d = DebtTokenAmortizationClient::new(&env, &debt_id).mock_all_auths();

    check_no_trap(&d.try_initialize(&admin), "debt_token::initialize");
    let initialized = succeeded(&d.try_admin());

    // Host-side mirror of the three tranche balances, in payment-priority
    // order. Kept independently of the contract so the assertions below are a
    // genuine cross-check.
    let mut owed = [0i128; 3];

    for _ in 0..MAX_OPS {
        step_ledger(&env, &mut input);
        let which = input.below(TRANCHES.len());
        let tranche = TRANCHES[which];

        match input.below(8) {
            0 | 1 | 2 => {
                let amount = hostile_amount(&mut input);
                let before: i128 = owed.iter().fold(0i128, |a, x| a.saturating_add(*x));
                let res = d.try_deposit_repayment(&borrower, &amount);
                check_no_trap(&res, "debt_token::deposit_repayment");

                if amount <= 0 {
                    assert!(
                        !succeeded(&res),
                        "deposit_repayment accepted a non-positive amount ({amount})"
                    );
                    assert_eq!(
                        d.total_owed(),
                        before,
                        "a rejected repayment moved the ledger"
                    );
                } else if succeeded(&res) {
                    // Replay the waterfall host-side: pay each tranche down to
                    // zero in order, capping every leg at that tranche's own
                    // balance, and drop any surplus.
                    let mut remaining = amount;
                    for slot in owed.iter_mut() {
                        let pay = core::cmp::min(remaining, *slot);
                        remaining -= pay;
                        *slot -= pay;
                    }
                    assert!(
                        d.total_owed() <= before,
                        "a repayment increased the total debt"
                    );
                }
            }
            3 | 4 => {
                let amount = hostile_amount(&mut input);
                let res = d.try_accrue_interest(&admin, &tranche, &amount);
                check_no_trap(&res, "debt_token::accrue_interest");

                if amount <= 0 {
                    assert!(
                        !succeeded(&res),
                        "accrue_interest accepted a non-positive amount ({amount})"
                    );
                    assert_eq!(
                        d.total_owed(),
                        owed.iter().fold(0i128, |a, x| a.saturating_add(*x)),
                        "a rejected accrual moved the ledger"
                    );
                } else if succeeded(&res) {
                    // On success the addition fitted exactly, so the mirror can
                    // be updated without a clamp.
                    assert!(
                        owed[which].checked_add(amount).is_some(),
                        "the contract succeeded on an addition the host mirror \
                         believes overflows"
                    );
                    owed[which] += amount;
                }
            }
            5 => {
                // A non-admin must not be able to accrue.
                let amount = hostile_amount(&mut input);
                let res = d.try_accrue_interest(&impostor, &tranche, &amount);
                check_no_trap(&res, "debt_token::accrue_interest(impostor)");
                assert!(!succeeded(&res), "a non-admin accrued interest");
            }
            6 => {
                let res = d.try_trigger_default(&impostor);
                check_no_trap(&res, "debt_token::trigger_default(impostor)");
                assert!(!succeeded(&res), "a non-admin triggered a default");
                if initialized {
                    let res = d.try_trigger_default(&admin);
                    check_no_trap(&res, "debt_token::trigger_default(admin)");
                    assert!(succeeded(&res), "the admin could not trigger a default");
                }
            }
            _ => {
                // The overpayment boundary, stated directly: repaying far more
                // than is outstanding must retire the ledger, not underflow.
                let total = d.total_owed();
                let res = d.try_deposit_repayment(&borrower, &total.saturating_add(1_000_000));
                check_no_trap(&res, "debt_token::deposit_repayment(overpay)");
                if succeeded(&res) {
                    assert_eq!(
                        d.total_owed(),
                        0,
                        "an overpayment must retire the whole ledger"
                    );
                    owed = [0, 0, 0];
                }
            }
        }

        // ---- invariants ---------------------------------------------------
        let live = [d.senior_owed(), d.mezzanine_owed(), d.equity_owed()];
        for i in 0..TRANCHES.len() {
            if live[i] < 0 {
                finding(
                    "debt_token",
                    &format!("{:?} owed went negative: {}", TRANCHES[i], live[i]),
                );
            }
            assert_eq!(
                live[i], owed[i],
                "{:?} disagrees with the host-side mirror",
                TRANCHES[i]
            );
        }
        let live_total: i128 = live.iter().fold(0i128, |a, x| a.saturating_add(*x));
        assert_eq!(
            live_total,
            d.total_owed(),
            "total_owed must be the sum of the three tranches"
        );
    }

    // Re-initialisation must always be refused and must never trap.
    let res = d.try_initialize(&impostor);
    check_no_trap(&res, "debt_token::initialize(again)");
    if initialized {
        assert!(!succeeded(&res), "the debt ledger was initialised twice");
    }
});
