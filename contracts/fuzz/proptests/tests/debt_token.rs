//! Property: the debt waterfall conserves the ledger.
//!
//! The bug this guards against is the one the contract's own comment describes.
//! The `Equity` leg originally took the entire uncapped remainder, so
//! repaying more than was outstanding computed `equity_owed - equity_payment`
//! with a negative `equity_payment` and trapped. A non-positive `amount` was
//! also accepted, and because each tranche was credited `owed - payment` with
//! `payment` itself negative, a negative repayment *increased* every tranche's
//! debt.
//!
//! The conservation law under test is: after any sequence of accruals and
//! repayments, no tranche is negative, the sum of the tranches equals what was
//! accrued minus what was repaid (capped at zero), and an overpayment retires
//! the ledger without wrapping.

use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, Address, Env};
use debt_token::{DebtTokenAmortization, DebtTokenAmortizationClient, Error as DebtError, Tranche};
use tessera_proptests::positive;

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

const TRANCHES: [Tranche; 3] = [Tranche::Senior, Tranche::Mezzanine, Tranche::Equity];

struct Fixture {
    d: DebtTokenAmortizationClient<'static>,
    admin: Address,
    borrower: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    let id = env.register(DebtTokenAmortization, ());
    let admin = Address::generate(&env);
    let borrower = Address::generate(&env);
    let d = DebtTokenAmortizationClient::new(&env, &id).mock_all_auths();
    let r = d.try_initialize(&admin);
    assert!(is_ok(&r), "setup: initialize failed: {r:?}");
    Fixture { d, admin, borrower }
}

fn tranches(f: &Fixture) -> [i128; 3] {
    [f.d.senior_owed(), f.d.mezzanine_owed(), f.d.equity_owed()]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// No tranche is ever negative, and the total never exceeds what was
    /// successfully accrued.
    ///
    /// A negative tranche is the signature of the uncapped-remainder bug: the
    /// old code wrote `owed - payment` with an oversized `payment`, and the
    /// underflow either trapped or, where it wrapped, left debt outstanding that
    /// had already been paid.
    #[test]
    fn tranches_never_go_negative(
        accruals in prop::collection::vec((0usize..3, positive()), 0..6),
        repayments in prop::collection::vec(positive(), 0..4),
    ) {
        let f = setup();
        let mut accrued = 0i128;

        for (t, amount) in &accruals {
            let r = f.d.try_accrue_interest(&f.admin, &TRANCHES[*t], amount);
            if is_ok(&r) {
                accrued += amount;
            } else {
                prop_assert!(
                    is_err(&r, DebtError::Overflow) || is_err(&r, DebtError::InvalidAmount),
                    "accrual of {amount} to tranche {t} failed unexpectedly: {r:?}"
                );
            }
            let live = tranches(&f);
            for (i, v) in live.iter().enumerate() {
                prop_assert!(
                    *v >= 0,
                    "tranche {i} went negative ({v}) after accruing {amount}"
                );
            }
        }

        for amount in &repayments {
            let r = f.d.try_deposit_repayment(&f.borrower, amount);
            if is_ok(&r) {
                accrued = (accrued - amount).max(0);
            }
            let live = tranches(&f);
            for (i, v) in live.iter().enumerate() {
                prop_assert!(
                    *v >= 0,
                    "tranche {i} went negative ({v}) after repaying {amount}"
                );
            }
            let total: i128 = live.iter().fold(0, |a, v| a.saturating_add(*v));
            prop_assert!(
                total <= accrued,
                "total owed {total} exceeds what is outstanding {accrued} after repaying {amount}"
            );
        }
    }

    /// A non-positive repayment is refused, and specifically does not *increase*
    /// the debt. The old code accepted one, and a negative amount credited every
    /// tranche with `owed - (negative)`.
    #[test]
    fn non_positive_repayments_are_refused(
        seeded in positive(),
        bad in prop::sample::select(vec![0i128, -1i128, -seeded, i128::MIN]),
    ) {
        let f = setup();
        f.d.accrue_interest(&f.admin, &Tranche::Senior, &seeded);
        let before = tranches(&f);

        let r = f.d.try_deposit_repayment(&f.borrower, &bad);
        prop_assert!(!is_ok(&r), "a repayment of {bad} was accepted");
        prop_assert!(
            is_err(&r, DebtError::InvalidAmount),
            "expected InvalidAmount for {bad}, got {r:?}"
        );
        prop_assert_eq!(
            tranches(&f),
            before,
            "a refused repayment of {bad} changed the ledger"
        );
    }

    /// An overpayment retires the whole ledger and does not wrap.
    #[test]
    fn overpayment_retires_the_ledger(
        a in positive().prop_filter("small", |v| *v < 1_000_000),
        b in positive().prop_filter("small", |v| *v < 1_000_000),
        c in positive().prop_filter("small", |v| *v < 1_000_000),
        over in 0i128..1_000_000i128,
    ) {
        let f = setup();
        f.d.accrue_interest(&f.admin, &Tranche::Senior, &a);
        f.d.accrue_interest(&f.admin, &Tranche::Mezzanine, &b);
        f.d.accrue_interest(&f.admin, &Tranche::Equity, &c);
        let before = tranches(&f);
        let total: i128 = before.iter().fold(0, |x, v| x.saturating_add(*v));

        let r = f.d.try_deposit_repayment(&f.borrower, &(total + over));
        prop_assert!(is_ok(&r), "an overpayment must be accepted: {r:?}");
        prop_assert_eq!(
            tranches(&f),
            [0, 0, 0],
            "an overpayment of {} did not retire the ledger (was {before:?})",
            total + over
        );
    }

    /// The waterfall pays senior, then mezzanine, then equity — in that order,
    /// never touching a lower tranche while a higher one is still outstanding.
    #[test]
    fn waterfall_order_is_respected(
        senior in positive().prop_filter("small", |v| *v < 1_000_000),
        mezz in positive().prop_filter("small", |v| *v < 1_000_000),
        equity in positive().prop_filter("small", |v| *v < 1_000_000),
    ) {
        let f = setup();
        f.d.accrue_interest(&f.admin, &Tranche::Senior, &senior);
        f.d.accrue_interest(&f.admin, &Tranche::Mezzanine, &mezz);
        f.d.accrue_interest(&f.admin, &Tranche::Equity, &equity);

        // Repay exactly the senior amount: only senior may move.
        f.d.deposit_repayment(&f.borrower, &senior);
        prop_assert_eq!(f.d.senior_owed(), 0, "senior was not fully repaid");
        prop_assert_eq!(f.d.mezzanine_owed(), mezz, "mezzanine moved before it was senior's turn");
        prop_assert_eq!(f.d.equity_owed(), equity, "equity moved before it was its turn");

        // Repay exactly the mezzanine amount: now only mezzanine may move.
        f.d.deposit_repayment(&f.borrower, &mezz);
        prop_assert_eq!(f.d.mezzanine_owed(), 0, "mezzanine was not fully repaid");
        prop_assert_eq!(f.d.equity_owed(), equity, "equity moved before it was mezzanine's turn");

        f.d.deposit_repayment(&f.borrower, &equity);
        prop_assert_eq!(f.d.equity_owed(), 0, "equity was not fully repaid");
        prop_assert_eq!(f.d.total_owed(), 0, "the ledger is not fully retired");
    }

    /// Only the admin may accrue, and only after initialization.
    #[test]
    fn accrual_is_admin_only_and_requires_init(amount in positive()) {
        let env = Env::default();
        let id = env.register(DebtTokenAmortization, ());
        let admin = Address::generate(&env);
        let stranger = Address::generate(&env);
        let d = DebtTokenAmortizationClient::new(&env, &id).mock_all_auths();

        // Before initialize: refused with NotInitialized, not a trap.
        let early = d.try_accrue_interest(&admin, &Tranche::Senior, &amount);
        prop_assert!(!is_ok(&early), "accrual before initialize was accepted");
        prop_assert!(
            is_err(&early, DebtError::NotInitialized),
            "expected NotInitialized, got {early:?}"
        );

        d.initialize(&admin);

        let by_stranger = d.try_accrue_interest(&stranger, &Tranche::Senior, &amount);
        prop_assert!(!is_ok(&by_stranger), "a non-admin accrued {amount} of debt");
        prop_assert!(
            is_err(&by_stranger, DebtError::Unauthorized),
            "expected Unauthorized, got {by_stranger:?}"
        );
        prop_assert_eq!(f_tranches(&d), [0, 0, 0], "a refused accrual changed the ledger");

        // Replay of initialize is refused.
        let replay = d.try_initialize(&admin);
        prop_assert!(!is_ok(&replay), "initialize was accepted twice");
        prop_assert!(
            is_err(&replay, DebtError::AlreadyInitialized),
            "expected AlreadyInitialized, got {replay:?}"
        );
    }
}

fn f_tranches(d: &DebtTokenAmortizationClient<'_>) -> [i128; 3] {
    [d.senior_owed(), d.mezzanine_owed(), d.equity_owed()]
}
