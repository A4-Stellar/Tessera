//! Scenario and property tests for the concentrated-liquidity AMM
//! (issue #361).

use proptest::prelude::*;
use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, Address, Env,
};

use super::*;
use math::{sqrt_price_at_tick, Q64};

#[contract]
struct MockCompliance;

#[contractimpl]
impl MockCompliance {
    pub fn is_allowed(_env: Env, _address: Address) -> bool {
        true
    }
}

#[derive(Clone)]
#[contracttype]
enum TokenKey {
    Balance(Address),
}

#[contract]
struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn mint(env: Env, to: Address, amount: i128) {
        let key = TokenKey::Balance(to);
        let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(balance + amount));
    }

    pub fn balance(env: Env, who: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&TokenKey::Balance(who))
            .unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        let from_key = TokenKey::Balance(from);
        let from_balance: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        if from_balance < amount {
            panic!("insufficient balance");
        }
        env.storage()
            .persistent()
            .set(&from_key, &(from_balance - amount));
        let to_key = TokenKey::Balance(to);
        let to_balance: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&to_key, &(to_balance + amount));
    }
}

struct Setup<'a> {
    env: Env,
    client: ConcentratedLiquidityAmmClient<'a>,
    token_a: MockTokenClient<'a>,
    token_b: MockTokenClient<'a>,
    admin: Address,
    lp: Address,
    trader: Address,
}

fn setup<'a>() -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let lp = Address::generate(&env);
    let trader = Address::generate(&env);

    let compliance_id = env.register(MockCompliance, ());
    let a_id = env.register(MockToken, ());
    let b_id = env.register(MockToken, ());
    let token_a = MockTokenClient::new(&env, &a_id);
    let token_b = MockTokenClient::new(&env, &b_id);
    token_a.mint(&lp, &10_000_000);
    token_b.mint(&lp, &10_000_000);
    token_a.mint(&trader, &10_000_000);
    token_b.mint(&trader, &10_000_000);

    let id = env.register(ConcentratedLiquidityAmm, ());
    let client = ConcentratedLiquidityAmmClient::new(&env, &id);
    client.initialize(
        &admin,
        &a_id,
        &b_id,
        &compliance_id,
        &30,
        &10,
        &20,
        &None,
        &0,
    );
    Setup {
        env,
        client,
        token_a,
        token_b,
        admin,
        lp,
        trader,
    }
}

#[test]
fn initialises_at_tick_zero() {
    let s = setup();
    let state = s.client.pool_state();
    assert_eq!(state.sqrt_price, Q64);
    assert_eq!(state.tick, 0);
    assert_eq!(state.liquidity, 0);
}

#[test]
fn add_liquidity_pulls_only_required_amounts() {
    let s = setup();
    let (position_id, liquidity) =
        s.client
            .add_liquidity(&s.lp, &-100, &100, &1_000_000, &1_000_000);
    assert!(liquidity > 0);
    let position = s.client.get_position(&position_id);
    assert_eq!(position.liquidity, liquidity);
    assert_eq!(position.tick_lower, -100);
    assert_eq!(position.tick_upper, 100);

    let used_a = 10_000_000 - s.token_a.balance(&s.lp);
    let used_b = 10_000_000 - s.token_b.balance(&s.lp);
    assert!(used_a > 0 && used_a <= 1_000_000);
    assert!(used_b > 0 && used_b <= 1_000_000);
    assert!(s.client.pool_state().liquidity >= liquidity);
}

#[test]
fn swap_moves_price_down_and_charges_fee() {
    let s = setup();
    s.client
        .add_liquidity(&s.lp, &-1000, &1000, &5_000_000, &5_000_000);
    let before = s.client.pool_state();
    let limit = sqrt_price_at_tick(&s.env, -100);
    let result = s.client.swap(&s.trader, &true, &10_000, &limit, &10);

    assert_eq!(result.amount_in, 10_000);
    assert!(result.amount_out > 0);
    assert!(result.amount_out < 10_000, "fee must be withheld");
    assert!(result.sqrt_price_after < before.sqrt_price);
    assert_eq!(s.token_a.balance(&s.trader), 10_000_000 - 10_000);
    assert_eq!(s.token_b.balance(&s.trader), 10_000_000 + result.amount_out);
    assert_eq!(s.client.pool_state().sqrt_price, result.sqrt_price_after);
    assert!(s.client.pool_state().fee_growth_0 > 0);
}

#[test]
fn swap_crosses_initialised_ticks() {
    let s = setup();
    s.client
        .add_liquidity(&s.lp, &-1000, &-10, &5_000_000, &5_000_000);
    s.client
        .add_liquidity(&s.lp, &-10, &1000, &5_000_000, &5_000_000);

    // Buying token B far enough to move past tick -10.
    let limit = sqrt_price_at_tick(&s.env, -500);
    let result = s.client.swap(&s.trader, &true, &1_000_000, &limit, &20);
    assert!(result.ticks_crossed >= 1);
    assert!(s.client.pool_state().tick < 0);
    assert_eq!(result.sqrt_price_after, s.client.pool_state().sqrt_price);
}

#[test]
fn swap_reverts_when_tick_limit_is_too_low() {
    let s = setup();
    s.client
        .add_liquidity(&s.lp, &-1000, &-10, &5_000_000, &5_000_000);
    s.client
        .add_liquidity(&s.lp, &-10, &1000, &5_000_000, &5_000_000);
    let limit = sqrt_price_at_tick(&s.env, -500);
    assert_eq!(
        s.client.try_swap(&s.trader, &true, &1_000_000, &limit, &1),
        Err(Ok(err(Error::TooManyTicksCrossed)))
    );
}

#[test]
fn remove_liquidity_returns_both_tokens() {
    let s = setup();
    let (position_id, liquidity) =
        s.client
            .add_liquidity(&s.lp, &-100, &100, &1_000_000, &1_000_000);
    let (out_a, out_b) = s.client.remove_liquidity(&s.lp, &position_id, &liquidity);
    assert!(out_a > 0 && out_b > 0);
    assert_eq!(s.client.get_position(&position_id).liquidity, 0);
    assert_eq!(s.client.pool_state().liquidity, 0);
}

#[test]
fn fee_growth_is_collectable() {
    let s = setup();
    let (position_id, _) = s.client.add_liquidity(
        &s.lp,
        &-10_000,
        &10_000,
        &9_000_000,
        &9_000_000,
    );
    let limit = sqrt_price_at_tick(&s.env, -100);
    s.client.swap(&s.trader, &true, &100_000, &limit, &10);
    let (fee_a, fee_b) = s.client.collect_fees(&s.lp, &position_id);
    let _ = fee_b;
    assert!(fee_a > 0, "fees must accrue in the input token");
}

#[test]
fn swap_requires_compliance_approval() {
    let s = setup();
    // Deploy a pool pointing at a compliance contract that denies everyone.
    let denying_id = s.env.register(DenyAll, ());
    let pool_id = s.env.register(ConcentratedLiquidityAmm, ());
    let client = ConcentratedLiquidityAmmClient::new(&s.env, &pool_id);
    let token_a = s.env.register(MockToken, ());
    let token_b = s.env.register(MockToken, ());
    client.initialize(
        &s.admin,
        &token_a,
        &token_b,
        &denying_id,
        &30,
        &10,
        &20,
        &None,
        &0,
    );
    assert_eq!(
        client.try_add_liquidity(&s.lp, &-10, &10, &1_000, &1_000),
        Err(Ok(err(Error::ComplianceCheckFailed)))
    );
}

#[contract]
struct DenyAll;

#[contractimpl]
impl DenyAll {
    pub fn is_allowed(_env: Env, _address: Address) -> bool {
        false
    }
}

fn err(e: Error) -> soroban_sdk::Error {
    e.into()
}

// ----------------------------------------------------------------------
// Property tests over the pure tick math
// ----------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Tick-to-sqrt-price is strictly monotonic across the supported range.
    #[test]
    fn tick_sqrt_price_is_monotonic(low in -5_000i32..5_000, delta in 1i32..5_000) {
        let env = Env::default();
        let high = low + delta;
        if high <= math::MAX_TICK {
            prop_assert!(sqrt_price_at_tick(&env, low) < sqrt_price_at_tick(&env, high));
        }
    }

    /// A tick round-trips through its sqrt price.
    #[test]
    fn tick_round_trips(tick in -5_000i32..5_000) {
        let env = Env::default();
        let price = sqrt_price_at_tick(&env, tick);
        prop_assert_eq!(math::tick_at_sqrt_price(&env, price), tick);
    }

    /// Liquidity derived from an amount never requires more than that amount
    /// when converting back, so the deposited token is not over-charged.
    #[test]
    fn liquidity_round_trip_does_not_overcharge(
        amount in 1u128..1_000_000_000_000,
        width in 1i32..5_000,
    ) {
        let env = Env::default();
        let lower = 0;
        let upper = lower + width;
        let sa = sqrt_price_at_tick(&env, lower);
        let sb = sqrt_price_at_tick(&env, upper);
        let liquidity = math::liquidity_for_amount1(&env, amount, sa, sb);
        let required = math::amount1_for_liquidity(&env, liquidity, sa, sb);
        prop_assert!(required <= amount);
        prop_assert!(required + 1 >= amount);
    }

    /// Virtual reserves in a range multiply to L^2 (the invariant holds
    /// within a tick range).
    #[test]
    fn virtual_reserves_preserve_constant_product(
        liquidity in 1u128..1_000_000_000_000,
        tick in -1_000i32..1_000,
    ) {
        let env = Env::default();
        let sqrt = sqrt_price_at_tick(&env, tick);
        // x = L / S, y = L * S in Q64.64.
        let x = math::mul_div(&env, liquidity, Q64, sqrt);
        let y = math::mul_div(&env, liquidity, sqrt, Q64);
        let product = math::mul_div(&env, x, y, 1);
        let l_squared = math::mul_div(&env, liquidity, liquidity, 1);
        // Allow 0.01 % for integer rounding in the Q64 conversions.
        let tolerance = l_squared / 10_000;
        prop_assert!(product + tolerance >= l_squared);
        prop_assert!(product <= l_squared + tolerance);
    }
}
