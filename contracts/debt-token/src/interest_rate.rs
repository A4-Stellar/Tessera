//! Issue #85 - kinked (Aave-style) dynamic interest-rate model.
//!
//! All rates and utilization are 18-decimal fixed point ("WAD", `1e18 == 100%`).
//! Rates are annual. With `U` utilization and `U_k` the kink:
//!   `U <= U_k`: `R = R0 + (U / U_k) * R1`
//!   `U >  U_k`: `R = R0 + R1 + ((U - U_k) / (1 - U_k)) * R2`
//! Multiplications go through 256-bit intermediates so 18-decimal token
//! amounts (up to ~1e38 base units) never overflow or lose precision before
//! the single final truncating division.
use soroban_sdk::{contracterror, contracttype, panic_with_error, Address, Env, Symbol, I256};

pub const WAD: i128 = 1_000_000_000_000_000_000;
pub const SECONDS_PER_YEAR: u64 = 31_536_000;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateParams {
    /// R0: base annual rate at zero utilization (WAD).
    pub base_rate: i128,
    /// R1: slope up to the kink (WAD).
    pub slope1: i128,
    /// R2: slope past the kink (WAD).
    pub slope2: i128,
    /// U_kink: optimal utilization, in (0, WAD) (WAD).
    pub kink: i128,
}

#[contracttype]
#[derive(Clone)]
pub enum RateKey {
    Params,
    Index,
    LastAccrual,
    RateAdmin,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RateError {
    InvalidParams = 1,
    InvalidUtilization = 2,
    NotConfigured = 3,
    Unauthorized = 4,
    Overflow = 5,
}

/// `a * b / c` with a 256-bit intermediate, truncating toward zero.
pub fn mul_div(env: &Env, a: i128, b: i128, c: i128) -> i128 {
    let r = I256::from_i128(env, a)
        .mul(&I256::from_i128(env, b))
        .div(&I256::from_i128(env, c));
    r.to_i128()
        .unwrap_or_else(|| panic_with_error!(env, RateError::Overflow))
}

pub fn validate(env: &Env, p: &RateParams) {
    if p.base_rate < 0 || p.slope1 < 0 || p.slope2 < 0 || p.kink <= 0 || p.kink >= WAD {
        panic_with_error!(env, RateError::InvalidParams);
    }
}

/// Utilization `borrowed / (borrowed + available)` in WAD; 0 for an empty pool.
pub fn utilization(env: &Env, borrowed: i128, available: i128) -> i128 {
    if borrowed < 0 || available < 0 {
        panic_with_error!(env, RateError::InvalidUtilization);
    }
    let total = borrowed
        .checked_add(available)
        .unwrap_or_else(|| panic_with_error!(env, RateError::Overflow));
    if total == 0 {
        return 0;
    }
    mul_div(env, borrowed, WAD, total)
}

/// Annual borrow rate (WAD) for utilization `u` (WAD, `0..=WAD`).
pub fn borrow_rate(env: &Env, p: &RateParams, u: i128) -> i128 {
    if !(0..=WAD).contains(&u) {
        panic_with_error!(env, RateError::InvalidUtilization);
    }
    if u <= p.kink {
        p.base_rate + mul_div(env, u, p.slope1, p.kink)
    } else {
        p.base_rate + p.slope1 + mul_div(env, u - p.kink, p.slope2, WAD - p.kink)
    }
}

/// Grow a WAD-scaled index by simple interest over `dt` seconds at annual `rate`.
pub fn accrue(env: &Env, index: i128, rate: i128, dt: u64) -> i128 {
    let growth = mul_div(env, rate, dt as i128, SECONDS_PER_YEAR as i128);
    index + mul_div(env, index, growth, WAD)
}

pub fn do_set_rate_params(env: &Env, admin: Address, params: RateParams) {
    admin.require_auth();
    match env
        .storage()
        .instance()
        .get::<_, Address>(&RateKey::RateAdmin)
    {
        Some(a) if a != admin => panic_with_error!(env, RateError::Unauthorized),
        Some(_) => {}
        None => env.storage().instance().set(&RateKey::RateAdmin, &admin),
    }
    validate(env, &params);
    env.storage().instance().set(&RateKey::Params, &params);
    env.events()
        .publish((Symbol::new(env, "rate_params"),), params);
}

pub fn get_params(env: &Env) -> RateParams {
    env.storage()
        .instance()
        .get(&RateKey::Params)
        .unwrap_or_else(|| panic_with_error!(env, RateError::NotConfigured))
}

pub fn do_accrue_index(env: &Env, borrowed: i128, available: i128) -> i128 {
    let now = env.ledger().timestamp();
    let index: i128 = env.storage().instance().get(&RateKey::Index).unwrap_or(WAD);
    let last: u64 = env
        .storage()
        .instance()
        .get(&RateKey::LastAccrual)
        .unwrap_or(now);
    let rate = borrow_rate(env, &get_params(env), utilization(env, borrowed, available));
    let new_index = accrue(env, index, rate, now.saturating_sub(last));
    env.storage().instance().set(&RateKey::Index, &new_index);
    env.storage().instance().set(&RateKey::LastAccrual, &now);
    env.events()
        .publish((Symbol::new(env, "index_accrued"),), (rate, new_index));
    new_index
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::DebtTokenAmortizationClient;
    use soroban_sdk::testutils::{Address as _, Ledger};

    fn params() -> RateParams {
        RateParams {
            base_rate: WAD / 100,   // 1%
            slope1: 4 * WAD / 100,  // 4%
            slope2: 75 * WAD / 100, // 75%
            kink: 80 * WAD / 100,   // 80%
        }
    }

    #[test]
    fn curve_hits_exact_points() {
        let env = Env::default();
        let p = params();
        assert_eq!(borrow_rate(&env, &p, 0), WAD / 100);
        assert_eq!(borrow_rate(&env, &p, p.kink), 5 * WAD / 100);
        assert_eq!(borrow_rate(&env, &p, WAD), 80 * WAD / 100);
        assert_eq!(borrow_rate(&env, &p, 40 * WAD / 100), 3 * WAD / 100);
        assert_eq!(
            borrow_rate(&env, &p, 90 * WAD / 100),
            5 * WAD / 100 + 375 * WAD / 1000
        );
    }

    #[test]
    fn curve_is_monotonic() {
        let env = Env::default();
        let p = params();
        let mut prev = 0;
        for i in 0..=1000 {
            let r = borrow_rate(&env, &p, WAD * i / 1000);
            assert!(r >= prev);
            prev = r;
        }
    }

    #[test]
    fn utilization_is_exact_for_18_decimal_amounts() {
        let env = Env::default();
        let unit: i128 = 1_000_000_000_000_000_000_000_000_000;
        assert_eq!(utilization(&env, unit, 3 * unit), WAD / 4);
        assert_eq!(utilization(&env, 0, 0), 0);
        assert_eq!(utilization(&env, unit, 0), WAD);
        // Products exceed 128 bits here but the 256-bit path stays exact.
        let big: i128 = 50_000_000_000_000_000_000_000_000_000_000_000_000;
        assert_eq!(utilization(&env, big, big), WAD / 2);
    }

    #[test]
    fn zero_rate_index_has_zero_drift_over_decades() {
        let env = Env::default();
        let mut idx = WAD;
        for _ in 0..(40 * 12) {
            idx = accrue(&env, idx, 0, SECONDS_PER_YEAR / 12);
        }
        assert_eq!(idx, WAD);
    }

    #[test]
    fn accrual_over_whole_years_matches_exact_decimal_compounding() {
        let env = Env::default();
        let rate = 5 * WAD / 100;
        assert_eq!(accrue(&env, WAD, rate, SECONDS_PER_YEAR), 105 * WAD / 100);
        // 1.05^10 = 1.628894626777441406... exactly representable to 18 dp
        // (1.05^10 has 20 decimals; truncation error <= 1e-18 per step).
        let mut idx = WAD;
        for _ in 0..10 {
            idx = accrue(&env, idx, rate, SECONDS_PER_YEAR);
        }
        let exact: i128 = 1_628_894_626_777_441_406;
        assert!((idx - exact).abs() <= 10, "drift {}", idx - exact);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1)")]
    fn invalid_params_rejected() {
        let env = Env::default();
        let mut p = params();
        p.kink = WAD;
        validate(&env, &p);
    }

    #[test]
    fn contract_flow() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000);
        let id = env.register(crate::DebtTokenAmortization, ());
        let c = DebtTokenAmortizationClient::new(&env, &id);
        let admin = Address::generate(&env);
        c.set_rate_params(&admin, &params());
        assert_eq!(c.get_rate_params(), params());
        assert_eq!(c.pool_utilization(&80, &20), 80 * WAD / 100);
        assert_eq!(c.current_borrow_rate(&80, &20), 5 * WAD / 100);
        assert_eq!(c.accrue_rate_index(&80, &20), WAD);
        env.ledger().set_timestamp(1_000 + SECONDS_PER_YEAR);
        assert_eq!(c.accrue_rate_index(&80, &20), 105 * WAD / 100);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #4)")]
    fn only_rate_admin_can_update() {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register(crate::DebtTokenAmortization, ());
        let c = DebtTokenAmortizationClient::new(&env, &id);
        c.set_rate_params(&Address::generate(&env), &params());
        c.set_rate_params(&Address::generate(&env), &params());
    }
}

// ─── #386: Liquidation & total_debt invariant ──────────────────────────────

/// Liquidation parameters (#386).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidationParams {
    /// Liquidation incentive in WAD (e.g. 1.05e18 = 5% bonus).
    pub liquidation_bonus: i128,
    /// Max fraction of a position that can be liquidated in one call (WAD).
    pub close_factor: i128,
}

/// Compute the repay amount for a liquidation, capped by close_factor.
///
/// Returns `(repay_amount, seize_collateral)` where:
///   repay_amount = min(debt * close_factor, repay_amount_requested)
///   seize_collateral = repay_amount * (WAD + liquidation_bonus) / WAD
pub fn liquidate_amount(
    env: &Env,
    debt: i128,
    collateral_price: i128,
    collateral_decimals: i128,
    requested_repay: i128,
    liq: &LiquidationParams,
) -> (i128, i128) {
    if debt <= 0 || collateral_price <= 0 || collateral_decimals <= 0 {
        return (0, 0);
    }
    let max_repay = mul_div(env, debt, liq.close_factor, WAD);
    let repay = requested_repay.min(max_repay);
    if repay <= 0 {
        return (0, 0);
    }
    // Seize collateral = repay * (1 + bonus) / price, adjusted for decimals
    let seize_value = mul_div(env, repay, liq.liquidation_bonus + WAD, WAD);
    let seize_collateral = mul_div(env, seize_value, collateral_decimals, collateral_price);
    (repay, seize_collateral)
}

/// Property: total_debt >= total_borrows invariant (#386).
/// After accruing interest, the index grows, so each borrower's share of
/// total_debt (sum of individual debts scaled by index) is always >= the
/// raw sum of principal borrows. This function verifies the invariant.
pub fn verify_total_debt_invariant(
    _env: &Env,
    total_debt: i128,
    total_borrows: i128,
) -> bool {
    total_debt >= total_borrows
}

#[cfg(test)]
mod liquidation_tests {
    use super::*;

    #[test]
    fn liquidate_caps_at_close_factor() {
        let env = Env::default();
        let liq = LiquidationParams {
            liquidation_bonus: 5 * WAD / 100, // 5%
            close_factor: 50 * WAD / 100,     // 50%
        };
        let debt = 1_000 * WAD;
        let price = WAD; // 1:1
        let decimals = WAD;
        let (repay, seize) = liquidate_amount(&env, debt, price, decimals, debt, &liq);
        assert_eq!(repay, 500 * WAD); // capped at 50%
        // seize = 500 * 1.05 / 1 = 525
        assert_eq!(seize, 525 * WAD);
    }

    #[test]
    fn liquidate_full_request_under_close_factor() {
        let env = Env::default();
        let liq = LiquidationParams {
            liquidation_bonus: 10 * WAD / 100,
            close_factor: WAD, // 100%
        };
        let debt = 100 * WAD;
        let (repay, seize) = liquidate_amount(&env, debt, WAD, WAD, 50 * WAD, &liq);
        assert_eq!(repay, 50 * WAD);
        assert_eq!(seize, 55 * WAD); // 50 * 1.10
    }

    #[test]
    fn total_debt_invariant_holds_after_accrual() {
        let env = Env::default();
        let borrows = 1_000 * WAD;
        // After one year at 5% the debt index grows to 1.05
        let index = accrue(&env, WAD, 5 * WAD / 100, SECONDS_PER_YEAR);
        let total_debt = mul_div(&env, borrows, index, WAD);
        assert!(verify_total_debt_invariant(&env, total_debt, borrows));
    }

    #[test]
    fn zero_debt_returns_zero() {
        let env = Env::default();
        let liq = LiquidationParams {
            liquidation_bonus: WAD / 10,
            close_factor: WAD,
        };
        let (repay, seize) = liquidate_amount(&env, 0, WAD, WAD, 100, &liq);
        assert_eq!((repay, seize), (0, 0));
    }
}
