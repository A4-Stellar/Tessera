//! Property: tax withholding splits a dividend losslessly and at the exact
//! stated rate, for every rate and every dividend.
//!
//! The bug this guards against is subtle enough to be worth spelling out. The
//! original arithmetic was
//!
//! ```text
//! tax = dividend.saturating_mul(rate) / 10_000
//! ```
//!
//! `saturating_mul` clamps at `i128::MAX` *before* the division. So a 50% rate
//! on a dividend near `i128::MAX` computed `i128::MAX / 10_000` — about 0.5% —
//! instead of 50%. No trap, no declared error, no log line: the contract simply
//! withheld almost nothing, and since `net + tax == dividend` still held, the
//! obvious conservation check passed. Silent revenue loss of ~99% on exactly the
//! largest payouts.
//!
//! Two properties together catch that. `net + tax == dividend` catches money
//! being created or destroyed. `tax == dividend * rate / 10_000` computed in
//! `u128` catches the amount being *wrong* while still conserving the total. The
//! second is the one that matters; the first alone would have shipped the bug.

use proptest::prelude::*;
use soroban_sdk::{
    testutils::Address as _, xdr::ToXdr, Address, Bytes, BytesN, Env, String as SdkString,
};
use tessera_compliance::{ComplianceContract, ComplianceContractClient};
use tessera_dividend::tax_withholding::{
    Error as TaxError, TaxWithholdingContract, TaxWithholdingContractClient, MAX_RATE_BPS,
};
use tessera_proptests::{amount, bps, non_negative};

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

/// The mathematically correct withholding for a non-negative dividend.
/// `i128::MAX * 10_000` is below 2^140, so `u128` holds the product exactly.
fn expected_tax(dividend: i128, rate: u32) -> i128 {
    ((dividend as u128) * (rate as u128) / 10_000) as i128
}

struct Fixture {
    env: Env,
    tax: TaxWithholdingContractClient<'static>,
    admin: Address,
    custody: Address,
    compliance: Address,
    resident: Address,
    /// The residency hash registered for `resident`.
    ///
    /// Held on the fixture rather than recomputed by a helper so that setup and
    /// the tests cannot disagree about which hash the contract will look up.
    /// `process_dividend` reads it via compliance's `get_tax_residency`, and a
    /// mismatch would silently fall back to the default rate.
    residency: BytesN<32>,
    /// `false` when `initialize` was refused, so callers know which entry points
    /// are expected to be reachable.
    initialized: bool,
}

fn setup(default_rate: u32) -> Fixture {
    let env = Env::default();
    let compliance_id = env.register(ComplianceContract, ());
    let tax_id = env.register(TaxWithholdingContract, ());
    let c = ComplianceContractClient::new(&env, &compliance_id).mock_all_auths();

    let admin = Address::generate(&env);
    let custody = Address::generate(&env);
    let resident = Address::generate(&env);
    c.initialize(&admin);
    c.add_to_allowlist(&admin, &resident, &SdkString::from_str(&env, "US"), 0);

    // Without this, `get_tax_residency` returns `None` and every dividend falls
    // back to `default_rate`, so `try_set_tax_rate` below would write a rate
    // that is never read and the override properties would be vacuous.
    let residency: BytesN<32> = env
        .crypto()
        .sha256(Bytes::from(resident.to_xdr(&env)))
        .into();
    c.set_tax_residency(&admin, &resident, &residency);

    let tax = TaxWithholdingContractClient::new(&env, &tax_id).mock_all_auths();
    let r = tax.try_initialize(&admin, &compliance_id, &custody, &default_rate);
    let initialized = is_ok(&r);
    assert!(
        initialized || is_err(&r, TaxError::InvalidTaxRate),
        "setup: initialize must either succeed or report InvalidTaxRate, got {r:?}"
    );

    Fixture {
        env,
        tax,
        admin,
        custody,
        compliance: compliance_id,
        resident,
        residency,
        initialized,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// For any legal rate and any non-negative dividend, the split is lossless
    /// and the withheld amount is exactly `dividend * rate / 10_000`.
    #[test]
    fn split_is_lossless_and_exact(
        default_rate in 0u32..=MAX_RATE_BPS,
        override_rate in 0u32..=MAX_RATE_BPS,
        dividend in non_negative(),
    ) {
        let f = setup(default_rate);
        prop_assert!(f.initialized, "a legal default rate must be accepted");

        // `setup` registered a residency for `resident`, so this per-hash rate
        // takes priority over the default.
        let set = f.tax.try_set_tax_rate(&f.admin, &f.residency, &override_rate);
        prop_assert!(is_ok(&set), "a legal per-hash rate must be accepted: {set:?}");

        let r = f.tax.try_process_dividend(&f.resident, &dividend);
        let Ok(Ok((net, tax_amount, custody))) = r else {
            prop_assert!(false, "process_dividend must succeed for a legal setup: {r:?}");
            return Ok(());
        };

        prop_assert_eq!(custody, f.custody, "wrong custody address returned");

        // Property 1: lossless. No units created, none destroyed.
        prop_assert_eq!(
            net + tax_amount,
            dividend,
            "net {net} + tax {tax_amount} != dividend {dividend} at rate {override_rate}"
        );

        // Property 2: exact. The part the old saturating_mul got wrong.
        prop_assert_eq!(
            tax_amount,
            expected_tax(dividend, override_rate),
            "at rate {override_rate} on {dividend}, withheld {tax_amount} but should withhold {}",
            expected_tax(dividend, override_rate)
        );

        // Property 3: bounded. Withholding can never exceed the dividend.
        prop_assert!(
            tax_amount <= dividend,
            "withheld {tax_amount} of a {dividend} dividend"
        );
        prop_assert!(net >= 0, "a negative net amount reached the investor");
    }

    /// A rate outside `0..=10_000` is refused on both the default and the
    /// per-hash path, and never traps.
    #[test]
    fn out_of_range_rates_are_refused(
        rate in bps().prop_filter("out of range", |v| *v > MAX_RATE_BPS),
    ) {
        let f = setup(0);
        prop_assert!(f.initialized, "the fixture must be initialized");

        let set = f.tax.try_set_default_tax_rate(&f.admin, &rate);
        prop_assert!(!is_ok(&set), "an out-of-range default rate {rate} was accepted");
        prop_assert!(
            is_err(&set, TaxError::InvalidTaxRate),
            "expected InvalidTaxRate for {rate}, got {set:?}"
        );

        let hash = BytesN::from_array(&f.env, &[7u8; 32]);
        let per = f.tax.try_set_tax_rate(&f.admin, &hash, &rate);
        prop_assert!(!is_ok(&per), "an out-of-range per-hash rate {rate} was accepted");
        prop_assert!(
            is_err(&per, TaxError::InvalidTaxRate),
            "expected InvalidTaxRate for {rate}, got {per:?}"
        );
    }

    /// A negative dividend is refused, whatever the rate.
    #[test]
    fn negative_dividends_are_refused(
        default_rate in 0u32..=MAX_RATE_BPS,
        dividend in amount().prop_filter("negative", |v| *v < 0),
    ) {
        let f = setup(default_rate);
        prop_assert!(f.initialized, "a legal default rate must be accepted");
        let r = f.tax.try_process_dividend(&f.resident, &dividend);
        prop_assert!(!is_ok(&r), "a negative dividend {dividend} was accepted");
        prop_assert!(
            is_err(&r, TaxError::InvalidAmount),
            "expected InvalidAmount for {dividend}, got {r:?}"
        );
    }

    /// Only the admin may set a rate, and `initialize` cannot be replayed.
    #[test]
    fn rate_writes_are_admin_only(
        rate in 0u32..=MAX_RATE_BPS,
        impostor in 0u32..=MAX_RATE_BPS,
    ) {
        let f = setup(rate);
        prop_assert!(f.initialized, "a legal default rate must be accepted");
        let hash = BytesN::from_array(&f.env, &[3u8; 32]);
        let stranger = Address::generate(&f.env);

        let by_stranger = f.tax.try_set_tax_rate(&stranger, &hash, &impostor);
        prop_assert!(!is_ok(&by_stranger), "a non-admin set a per-hash rate to {impostor}");
        prop_assert!(
            is_err(&by_stranger, TaxError::Unauthorized),
            "expected Unauthorized, got {by_stranger:?}"
        );

        let by_stranger = f.tax.try_set_default_tax_rate(&stranger, &impostor);
        prop_assert!(!is_ok(&by_stranger), "a non-admin set the default rate");
        prop_assert!(
            is_err(&by_stranger, TaxError::Unauthorized),
            "expected Unauthorized, got {by_stranger:?}"
        );

        // Replay of `initialize` must be refused and must not change state.
        let replay = f.tax.try_initialize(&f.admin, &f.compliance, &f.custody, &impostor);
        prop_assert!(!is_ok(&replay), "initialize was accepted twice");
        prop_assert!(
            is_err(&replay, TaxError::AlreadyInitialized),
            "expected AlreadyInitialized, got {replay:?}"
        );
    }

    /// Every rate boundary in range behaves exactly as documented: 0% withholds
    /// nothing, 100% withholds everything.
    #[test]
    fn rate_boundaries_are_exact(dividend in non_negative()) {
        for (rate, expect_all) in [
            (0u32, false),
            (1u32, false),
            (5_000u32, false),
            (9_999u32, false),
            (10_000u32, true),
        ] {
            let f = setup(rate);
            let r = f.tax.try_process_dividend(&f.resident, &dividend);
            let Ok(Ok((net, tax_amount, _))) = r else {
                prop_assert!(false, "boundary rate {rate} failed: {r:?}");
                continue;
            };
            prop_assert_eq!(tax_amount, expected_tax(dividend, rate), "rate {rate}");
            if expect_all {
                prop_assert_eq!(
                    net, 0,
                    "at 100% the investor must net nothing (dividend {dividend})"
                );
            } else {
                prop_assert_eq!(net + tax_amount, dividend, "rate {rate}");
            }
            if rate == 0 {
                prop_assert_eq!(tax_amount, 0, "0% must withhold nothing");
                prop_assert_eq!(net, dividend, "0% must pass the full amount");
            }
        }
    }

    /// A stored rate of 0 is a real exemption and must not fall back to the
    /// default. This is the `or(Some(default))` vs `or_else` distinction.
    #[test]
    fn zero_per_hash_rate_is_an_exemption_not_a_fallback(
        default_rate in 1u32..=MAX_RATE_BPS,
        dividend in non_negative(),
    ) {
        let f = setup(default_rate);
        prop_assert!(f.initialized, "a legal default rate must be accepted");

        let set = f.tax.try_set_tax_rate(&f.admin, &f.residency, &0u32);
        prop_assert!(is_ok(&set), "a 0% per-hash rate must be accepted: {set:?}");

        let r = f.tax.try_process_dividend(&f.resident, &dividend);
        let Ok(Ok((net, tax_amount, _))) = r else {
            prop_assert!(false, "process_dividend failed: {r:?}");
            return Ok(());
        };
        prop_assert_eq!(
            tax_amount, 0,
            "a stored 0% rate must withhold nothing, but the default {default_rate} leaked through"
        );
        prop_assert_eq!(net, dividend, "an exempt investor must receive the full dividend");
    }
}
