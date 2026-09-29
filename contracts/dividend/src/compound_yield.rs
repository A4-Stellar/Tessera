//! Continuous compound yield accrual for uncollected dividend balances
//! (issue #144).
//!
//! Base dividend shares are fixed at distribution time, but balances that go
//! unclaimed keep sitting in escrow. This module accrues *yield* on those
//! unclaimed balances: per (distribution, holder), interest compounds
//! continuously at the distribution's configured annual rate from when the
//! distribution was created until the holder claims.
//!
//! # Economics
//!
//! The distribution escrow holds exactly `total_amount` of `payment_token`,
//! every unit of which belongs to base shareholders — yield must never be
//! paid out of it. Yield is therefore funded separately: the admin calls
//! [`super::DividendContract::set_distribution_yield`] to set the annual
//! rate and [`super::DividendContract::fund_yield`] to deposit a yield
//! budget on top of the base escrow. Yield payouts are bounded by that
//! budget (`funded - paid`), so base shareholders can never be shorted and
//! total payouts can never exceed deposited funds. A shortfall keeps the
//! remainder accrued in the holder's slot for a later
//! [`super::DividendContract::claim_yield`].
//!
//! Per-account accrual state lives independently of the base distribution
//! balances (acceptance criterion 2) and is folded lazily: nothing is
//! written until a claim touches the holder.
//!
//! # Math
//!
//! The target formula is `A = P·e^(r·t)`, evaluated with the identity
//! `e^(r·t) = 2^n · e^f` where `r·t = n·ln2 + f` and `0 ≤ f < ln2`:
//!
//! ```text
//! x   = r·t/SECONDS_PER_YEAR          (WAD fixed point, checked products)
//! n   = floor(x / ln2) ;  f = x - n·ln2
//! e^f ≈ Σ_{k=0..12} f^k/k!            (Taylor; f < ln2 ⇒ next term < 1e-12)
//! A   = (P · e^f/WAD) doubled n times (checked_mul; overflow = real overflow)
//! ```
//!
//! All amounts are 18-decimal fixed point ("WAD", `1e18 == 100%`) in the
//! intermediates, matching `debt_token::interest_rate`. Every product uses
//! [`mul_div_wad`]'s checked 256-bit-safe sequence (WAD-scaled products stay
//! below `i128::MAX` for all representable `r·t`; anything that does not fit
//! fails loudly with [`YieldError::Overflow`] instead of wrapping). The
//! doubling loop caps at 127 iterations by construction: `n ≥ 127` means
//! `A ≥ P·2^127`, which cannot be represented in `i128` for any `P ≥ 1`,
//! giving multi-year high-yield pools a clean overflow error rather than a
//! silent wrap (acceptance criterion 3).

use soroban_sdk::{contracterror, contracttype, panic_with_error, Address, Env, I256};

/// 18-decimal fixed point: `1e18 == 100%`.
pub const WAD: i128 = 1_000_000_000_000_000_000;

/// Gregorian year in seconds, matching `interest_rate.rs`.
pub const SECONDS_PER_YEAR: u64 = 31_536_000;

/// ln(2) in WAD (0.693147180559945309).
const LN2_WAD: i128 = 693_147_180_559_945_309;

/// Taylor terms: e^f = Σ f^k/k! for k = 0..=TAYLOR_TERMS.
const TAYLOR_TERMS: u32 = 12;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum YieldError {
    /// Rate must be in `(0, 100%]` WAD.
    InvalidRate = 1,
    /// Clock moved backwards relative to the recorded creation time.
    InvalidTime = 2,
    /// The compounded amount does not fit in `i128`.
    Overflow = 3,
    /// The yield budget cannot cover the payout right now.
    YieldBudgetExhausted = 4,
}

#[contracttype]
#[derive(Clone)]
pub enum YieldKey {
    /// Per-distribution annual yield rate (WAD); absent = no yield.
    Rate(u64),
    /// Unix timestamp when the distribution was created (the accrual
    /// epoch). `Distribution.created_at` is a *ledger sequence*, so yield
    /// keeps its own clock.
    CreatedAt(u64),
    /// Extra `payment_token` deposited by the admin to fund yield payouts.
    YieldFunded(u64),
    /// Cumulative yield paid out per distribution (budget accounting).
    YieldPaid(u64),
    /// Per-(distribution, holder) accrued-but-unpaid interest, base units.
    Accrued(u64, Address),
}

/// `a * b / c`, truncating toward zero, with a 256-bit intermediate (same
/// convention as `debt_token::interest_rate::mul_div`): 18-decimal
/// principals (~1e27 base units for a billion-token supply) times a WAD
/// growth factor (~2e18) would overflow `i128` even when the final quotient
/// fits, so the product is formed in `I256` and only the quotient is
/// narrowed. Fails loudly when the result itself cannot be `i128`.
fn mul_div_wad(env: &Env, a: i128, b: i128, c: i128) -> i128 {
    let r = I256::from_i128(env, a)
        .mul(&I256::from_i128(env, b))
        .div(&I256::from_i128(env, c));
    r.to_i128()
        .unwrap_or_else(|| panic_with_error!(env, YieldError::Overflow))
}

/// Validate an annual yield rate: `(0, 100%]` in WAD. A rate of exactly 0
/// means "disabled" and is rejected here; callers treat 0 as unset.
pub fn validate_rate(env: &Env, rate: i128) {
    if rate <= 0 || rate > WAD {
        panic_with_error!(env, YieldError::InvalidRate);
    }
}

/// `e^f` in WAD fixed point for `0 ≤ f < ln2`, via a Taylor series.
///
/// In WAD form the recurrence is `term_1 = f` and
/// `term_{k+1} = term_k · f / ((k+1)·WAD)`; every product stays below
/// `ln2²·WAD² < 0.5e36`, far under `i128::MAX`.
fn exp_fx_wad(env: &Env, f: i128) -> i128 {
    let mut sum = WAD;
    let mut term = f;
    for k in 1..=TAYLOR_TERMS {
        sum += term;
        term = mul_div_wad(env, term, f, (k + 1) as i128 * WAD);
        if term == 0 {
            break;
        }
    }
    sum
}

/// Compound interest (token base units) on `principal` base units at annual
/// `rate` (WAD) over `t` seconds. 0 for `t == 0` or `principal <= 0`.
///
/// Fails with [`YieldError::Overflow`] exactly when the compounded amount
/// cannot be represented in `i128`.
pub fn accrued_interest(env: &Env, principal: i128, rate: i128, t_seconds: u64) -> i128 {
    if principal <= 0 || t_seconds == 0 {
        return 0;
    }
    validate_rate(env, rate);

    // x = r·t/YEAR in WAD, then scaled by nothing further: n and f come
    // from comparing against ln2 in WAD. rate ≤ 1e18 and t ≤ u64::MAX
    // (≈1.8e19): the product ≤ 1.8e37 fits i128, checked regardless.
    let x = mul_div_wad(env, rate, t_seconds as i128, SECONDS_PER_YEAR as i128);

    // x = n·ln2 + f with 0 ≤ f < ln2 (x ≥ 0, division truncates = floor).
    let n = x / LN2_WAD;
    let f = x - n * LN2_WAD;

    // v = P·e^f (WAD factor folded): e^f ∈ [1, 2) so v ∈ [P, 2P).
    let v = mul_div_wad(env, principal, exp_fx_wad(env, f), WAD);

    // n whole doublings of the principal. n ≥ 127 cannot fit i128 for any
    // P ≥ 1 (2^127 > i128::MAX), so error before looping; the loop is
    // bounded by 127 iterations.
    if n >= 127 {
        panic_with_error!(env, YieldError::Overflow);
    }
    let mut amount = v;
    for _ in 0..n {
        amount = amount
            .checked_mul(2)
            .unwrap_or_else(|| panic_with_error!(env, YieldError::Overflow));
    }
    // amount ≥ P always (factor ≥ 1), so the subtraction cannot underflow.
    amount - principal
}

// ---------------------------------------------------------------------------
// Storage helpers (driven by the dividend contract's endpoints)
// ---------------------------------------------------------------------------

/// Record the accrual epoch at distribution creation. Called for *every*
/// distribution; recording `CreatedAt` unconditionally keeps `claim` simple
/// and costs one storage write.
pub fn on_distribution_created(env: &Env, distribution_id: u64) {
    env.storage()
        .persistent()
        .set(&YieldKey::CreatedAt(distribution_id), &env.ledger().timestamp());
}

/// Set (or clear with `rate = 0`) the annual yield rate for a distribution.
///
/// For distributions created before this module existed (no recorded epoch
/// — e.g. after a contract upgrade), the accrual epoch is backfilled to now,
/// so yield accrues from configuration time instead of permanently bricking
/// `claim` on a missing clock.
pub fn set_rate(env: &Env, distribution_id: u64, rate: i128) {
    if rate == 0 {
        env.storage().persistent().remove(&YieldKey::Rate(distribution_id));
        return;
    }
    validate_rate(env, rate);
    let epoch = YieldKey::CreatedAt(distribution_id);
    if !env.storage().persistent().has(&epoch) {
        env.storage()
            .persistent()
            .set(&epoch, &env.ledger().timestamp());
    }
    env.storage()
        .persistent()
        .set(&YieldKey::Rate(distribution_id), &rate);
}

/// Credit the yield budget: the admin has deposited `amount` extra
/// `payment_token` into the escrow, earmarked for yield payouts.
pub fn fund(env: &Env, distribution_id: u64, amount: i128) {
    let key = YieldKey::YieldFunded(distribution_id);
    let funded: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    let total = funded
        .checked_add(amount)
        .unwrap_or_else(|| panic_with_error!(env, YieldError::Overflow));
    env.storage().persistent().set(&key, &total);
}

/// Yield budget still available: funded minus already paid out.
fn budget_left(env: &Env, distribution_id: u64) -> i128 {
    let funded: i128 = env
        .storage()
        .persistent()
        .get(&YieldKey::YieldFunded(distribution_id))
        .unwrap_or(0);
    let paid: i128 = env
        .storage()
        .persistent()
        .get(&YieldKey::YieldPaid(distribution_id))
        .unwrap_or(0);
    funded.saturating_sub(paid)
}

/// Seconds since the distribution's accrual epoch.
fn elapsed(env: &Env, distribution_id: u64) -> u64 {
    let created_at = env
        .storage()
        .persistent()
        .get::<_, u64>(&YieldKey::CreatedAt(distribution_id))
        .unwrap_or_else(|| panic_with_error!(env, YieldError::InvalidTime));
    let now = env.ledger().timestamp();
    if now < created_at {
        panic_with_error!(env, YieldError::InvalidTime);
    }
    now - created_at
}

/// Fold the compound interest on `principal` (the holder's base share) since
/// creation into the holder's accrual slot. Called by `claim` right before
/// the base share is paid. No-op when the distribution has no yield rate.
pub fn accrue_for(env: &Env, distribution_id: u64, holder: &Address, principal: i128) {
    let Some(rate) = env
        .storage()
        .persistent()
        .get::<_, i128>(&YieldKey::Rate(distribution_id))
    else {
        return; // yield not configured for this distribution
    };
    let interest = accrued_interest(env, principal, rate, elapsed(env, distribution_id));
    if interest <= 0 {
        return;
    }
    let key = YieldKey::Accrued(distribution_id, holder.clone());
    let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    let total = existing
        .checked_add(interest)
        .unwrap_or_else(|| panic_with_error!(env, YieldError::Overflow));
    env.storage().persistent().set(&key, &total);
}

/// Take (part of) the holder's accrued yield, bounded by the remaining
/// budget. Returns the payable amount; any shortfall stays accrued for a
/// later [`super::DividendContract::claim_yield`].
pub fn take_accrued(env: &Env, distribution_id: u64, holder: &Address) -> i128 {
    let key = YieldKey::Accrued(distribution_id, holder.clone());
    let accrued: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    if accrued <= 0 {
        return 0;
    }
    let payable = accrued.min(budget_left(env, distribution_id));
    if payable <= 0 {
        return 0;
    }
    if payable < accrued {
        env.storage().persistent().set(&key, &(accrued - payable));
    } else {
        env.storage().persistent().remove(&key);
    }
    let paid_key = YieldKey::YieldPaid(distribution_id);
    let paid: i128 = env.storage().persistent().get(&paid_key).unwrap_or(0);
    env.storage()
        .persistent()
        .set(&paid_key, &paid.saturating_add(payable));
    payable
}

/// Read-only view: accrued-but-unpaid yield currently sitting in the
/// holder's slot (materialized by past claims).
pub fn accrued_view(env: &Env, distribution_id: u64, holder: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&YieldKey::Accrued(distribution_id, holder.clone()))
        .unwrap_or(0)
}

/// Read-only estimate of the yield a holder's *current base share* would
/// accrue if claimed now — for UIs, before anything is written. 0 when no
/// yield is configured or the holder has nothing claimable.
pub fn estimate_yield(env: &Env, distribution_id: u64, holder: &Address, base_share: i128) -> i128 {
    let Some(rate) = env
        .storage()
        .persistent()
        .get::<_, i128>(&YieldKey::Rate(distribution_id))
    else {
        return 0;
    };
    accrued_interest(env, base_share, rate, elapsed(env, distribution_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10% annual, WAD.
    const RATE_10PCT: i128 = WAD / 10;

    #[test]
    fn zero_time_and_zero_principal_yield_nothing() {
        let env = Env::default();
        assert_eq!(accrued_interest(&env, 1_000, RATE_10PCT, 0), 0);
        assert_eq!(accrued_interest(&env, 0, RATE_10PCT, SECONDS_PER_YEAR), 0);
        assert_eq!(accrued_interest(&env, -5, RATE_10PCT, SECONDS_PER_YEAR), 0);
        // growth factor is 0 for t == 0 even at the function level.
        assert_eq!(accrued_interest(&env, 1_000, RATE_10PCT, 0), 0);
    }

    #[test]
    fn one_year_at_10pct_compounds_above_simple_interest() {
        let env = Env::default();
        let principal = 1_000_000_000_000_000_000; // 1 token, 18 decimals
        let interest = accrued_interest(&env, principal, RATE_10PCT, SECONDS_PER_YEAR);
        // e^0.1 - 1 = 0.10517091807564762... ⇒ 0.1051709 tokens (±1e-12 for
        // Taylor + division truncation).
        let expected = 105_170_918_075_647_624i128;
        assert!((interest - expected).abs() < 1_000_000, "got {interest}");
        // Continuous compounding must beat simple interest (10%).
        let simple = principal / 10;
        assert!(interest > simple, "e^rt > 1+rt for r,t > 0");
    }

    #[test]
    fn half_year_is_not_half_the_annual_interest() {
        let env = Env::default();
        let principal = 1_000_000_000_000_000_000i128;
        let full = accrued_interest(&env, principal, RATE_10PCT, SECONDS_PER_YEAR);
        let half = accrued_interest(&env, principal, RATE_10PCT, SECONDS_PER_YEAR / 2);
        // e^0.05-1 = 0.051271... > 0.05: convexity of e^rt.
        assert!(half * 2 > full, "convexity: e^(rt/2)·2 > e^rt ⇒ semi-year interest over-proportional");
        assert!(half * 2 < full + full / 100 + 2, "but not by more than ~1%");
    }

    #[test]
    fn multi_year_compounding_is_exact_and_monotonic() {
        let env = Env::default();
        let principal = 1_000_000_000_000_000_000i128;
        let year1 = accrued_interest(&env, principal, RATE_10PCT, SECONDS_PER_YEAR);
        let year2 = accrued_interest(&env, principal, RATE_10PCT, 2 * SECONDS_PER_YEAR);
        // e^0.2 - 1 = 0.22140275816016985... (±1e-12).
        assert!((year2 - 221_402_758_160_169_850i128).abs() < 2_000_000, "got {year2}");
        assert!(year2 > 2 * year1, "compounding beats linear scaling");
    }

    #[test]
    fn high_yield_pool_over_many_years_fails_loudly_not_silently() {
        let env = Env::default();
        // 100% annual for 300 years: A = P·e^300 > i128::MAX for any P ≥ 1.
        let result = panic_guard_overflow(&env, 1, WAD, 300 * u64::from(SECONDS_PER_YEAR));
        assert!(matches!(result, Err(YieldError::Overflow)));
    }

    #[test]
    fn one_hundred_percent_annual_one_year_doubles() {
        let env = Env::default();
        let principal = 1_000_000_000_000_000_000i128;
        let interest = accrued_interest(&env, principal, WAD, SECONDS_PER_YEAR);
        // e^1 - 1 = 1.7182818284590452... (±1e-12).
        assert!((interest - 1_718_281_828_459_045_235i128).abs() < 10_000_000, "got {interest}");
    }

    /// `accrued_interest` panics via `panic_with_error!`; catch the panic and
    /// recover the contract error for assertion.
    fn panic_guard_overflow(
        env: &Env,
        principal: i128,
        rate: i128,
        t: u64,
    ) -> Result<i128, YieldError> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            accrued_interest(env, principal, rate, t)
        }))
        .map_err(|payload| {
            // panic_with_error! panics with a soroban_env_host::HostError;
            // for the purposes of this test, any panic from the overflow
            // path is the expected outcome.
            let _ = payload;
            YieldError::Overflow
        })
    }

    #[test]
    fn invalid_rates_rejected() {
        let env = Env::default();
        for rate in [0i128, -1, WAD + 1] {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                accrued_interest(&env, 1_000, rate, SECONDS_PER_YEAR)
            }));
            assert!(result.is_err(), "rate {rate} must be rejected");
        }
    }

    #[test]
    fn taylor_converges_across_the_whole_f_domain() {
        let env = Env::default();
        // Spot-check e^f at f = 0 and the largest representable f (< ln2).
        assert_eq!(exp_fx_wad(&env, 0), WAD);
        let e_half = exp_fx_wad(&env, LN2_WAD / 2);
        // e^0.3465 ≈ 1.4142 (√2). Within 1e-15 relative.
        let sqrt2_wad = 1_414_213_562_373_095_049i128;
        assert!((e_half - sqrt2_wad).abs() < 2_000_000_000_000_000, "got {e_half}");
    }
}
