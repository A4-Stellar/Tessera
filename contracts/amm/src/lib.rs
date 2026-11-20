//! Tessera concentrated-liquidity AMM (issue #361).
//!
//! This contract replaces the previous constant-product pool with a Uniswap
//! v3-style tick-indexed model adapted to Soroban storage:
//!
//! - Each initialised tick is a `PersistentStorage` entry keyed by its `i32`
//!   tick index, holding gross and net liquidity.
//! - A position is a liquidity range `[tick_lower, tick_upper)`. When a
//!   position-NFT contract is configured, `add_liquidity` mints one NFT per
//!   position through a cross-contract call to `mint_position`.
//! - `swap` walks initialised ticks in the direction of the trade, crossing
//!   each one, and takes a `max_ticks_crossed` bound. Exceeding it reverts the
//!   whole swap instead of silently returning a worse price.
//! - Within any single tick range the virtual reserves satisfy
//!   `x * y = L^2`, so the constant-product invariant holds while the price
//!   stays in that range (see [`math`]).
//!
//! Fees are charged in the input token on every step and accumulated in
//! Q64.64 per unit of liquidity, so positions collect pro-rata via
//! `collect_fees`.

#![no_std]

pub mod math;
#[cfg(test)]
mod test;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, Env, IntoVal, Symbol, Val, Vec,
};

use math::{
    amount0_for_liquidity, amount0_in_for_price_move, amount0_out_for_price_move,
    amount1_for_liquidity, amount1_in_for_price_move, amount1_out_for_price_move, fee_growth_delta, liquidity_for_amount0, liquidity_for_amount1,
    mul_div, sqrt_price_at_tick, sqrt_price_after_amount0_in, sqrt_price_after_amount0_out,
    sqrt_price_after_amount1_in, sqrt_price_after_amount1_out, tick_at_sqrt_price, MAX_SQRT_PRICE,
    MAX_TICK, MIN_SQRT_PRICE, MIN_TICK, Q64,
};

/// 100 % in basis points.
const BPS_SCALE: u128 = 10_000;
/// Hard cap on the number of ticks a single swap may inspect, independent of
/// the caller's limit. Keeps the loop bounded even on a hostile pool.
// Each scanned tick is a ledger read; Soroban caps a call's footprint at 100 entries.
const MAX_SCAN_TICKS: u32 = 64;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidToken = 4,
    InvalidAmount = 5,
    InvalidTick = 6,
    InvalidTickRange = 7,
    TickNotAligned = 8,
    InvalidSqrtPrice = 9,
    InvalidPriceLimit = 10,
    InsufficientLiquidity = 11,
    /// The swap would need to cross more ticks than `max_ticks_crossed`.
    TooManyTicksCrossed = 12,
    ComplianceCheckFailed = 13,
    PositionNotFound = 14,
    NotPositionOwner = 15,
    Overflow = 16,
    DivisionByZero = 17,
    /// The price limit was reached before the requested amount was filled.
    PriceLimitReached = 18,
    ZeroLiquidity = 19,
    FeeTooHigh = 20,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickInfo {
    /// Total liquidity referencing this tick as a boundary.
    pub liquidity_gross: i128,
    /// Liquidity added when the tick is crossed upwards (negative for an
    /// upper boundary).
    pub liquidity_net: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    pub owner: Address,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub liquidity: i128,
    pub fee_growth_inside_0: u128,
    pub fee_growth_inside_1: u128,
    pub tokens_owed_0: i128,
    pub tokens_owed_1: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolState {
    pub sqrt_price: u128,
    pub tick: i32,
    pub liquidity: i128,
    pub reserve_a: i128,
    pub reserve_b: i128,
    pub fee_growth_0: u128,
    pub fee_growth_1: u128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwapResult {
    pub amount_in: i128,
    pub amount_out: i128,
    pub sqrt_price_after: u128,
    pub ticks_crossed: u32,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    TokenA,
    TokenB,
    Compliance,
    FeeBps,
    TickSpacing,
    MaxTicksCrossed,
    PositionNft,
    SqrtPrice,
    CurrentTick,
    Liquidity,
    ReserveA,
    ReserveB,
    FeeGrowth0,
    FeeGrowth1,
    Tick(i32),
    Position(u64),
    NextPositionId,
}

#[contract]
pub struct ConcentratedLiquidityAmm;

#[contractimpl]
impl ConcentratedLiquidityAmm {
    /// Configure the pool. `initial_tick` must be a multiple of
    /// `tick_spacing`; the opening sqrt price is derived from it.
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        env: Env,
        admin: Address,
        token_a: Address,
        token_b: Address,
        compliance_contract: Address,
        fee_bps: u32,
        tick_spacing: i32,
        max_ticks_crossed: u32,
        position_nft: Option<Address>,
        initial_tick: i32,
    ) {
        if env.storage().instance().has(&DataKey::TokenA) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        if token_a == token_b {
            panic_with_error!(env, Error::InvalidToken);
        }
        if tick_spacing <= 0 || initial_tick % tick_spacing != 0 {
            panic_with_error!(env, Error::TickNotAligned);
        }
        if fee_bps as u128 >= BPS_SCALE {
            panic_with_error!(env, Error::FeeTooHigh);
        }
        if max_ticks_crossed == 0 {
            panic_with_error!(env, Error::TooManyTicksCrossed);
        }
        let sqrt_price = sqrt_price_at_tick(&env, initial_tick);
        if sqrt_price < MIN_SQRT_PRICE || sqrt_price > MAX_SQRT_PRICE {
            panic_with_error!(env, Error::InvalidSqrtPrice);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::TokenA, &token_a);
        env.storage().instance().set(&DataKey::TokenB, &token_b);
        env.storage()
            .instance()
            .set(&DataKey::Compliance, &compliance_contract);
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        env.storage()
            .instance()
            .set(&DataKey::TickSpacing, &tick_spacing);
        env.storage()
            .instance()
            .set(&DataKey::MaxTicksCrossed, &max_ticks_crossed);
        env.storage()
            .instance()
            .set(&DataKey::PositionNft, &position_nft);
        env.storage()
            .instance()
            .set(&DataKey::SqrtPrice, &sqrt_price);
        env.storage()
            .instance()
            .set(&DataKey::CurrentTick, &initial_tick);
        env.storage().instance().set(&DataKey::Liquidity, &0i128);
        env.storage().instance().set(&DataKey::ReserveA, &0i128);
        env.storage().instance().set(&DataKey::ReserveB, &0i128);
        env.storage().instance().set(&DataKey::FeeGrowth0, &0u128);
        env.storage().instance().set(&DataKey::FeeGrowth1, &0u128);
        env.storage()
            .instance()
            .set(&DataKey::NextPositionId, &0u64);
    }

    /// Set the position-NFT contract. Positions are minted there as NFTs on
    /// every `add_liquidity` call.
    pub fn set_position_nft(env: Env, admin: Address, position_nft: Address) {
        Self::require_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::PositionNft, &Some(position_nft));
    }

    // ------------------------------------------------------------------
    // Liquidity
    // ------------------------------------------------------------------

    /// Add liquidity to `[tick_lower, tick_upper)` and mint a position.
    ///
    /// Only the amounts actually required at the current price are pulled
    /// from `to`; any remainder stays in their wallet. Returns the position id
    /// and the liquidity minted.
    pub fn add_liquidity(
        env: Env,
        to: Address,
        tick_lower: i32,
        tick_upper: i32,
        amount_a: i128,
        amount_b: i128,
    ) -> (u64, i128) {
        to.require_auth();
        Self::check_compliance(&env, &to);
        if amount_a <= 0 && amount_b <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        let spacing = Self::tick_spacing(&env);
        let lower = Self::align_tick(&env, tick_lower, spacing);
        let upper = Self::align_tick(&env, tick_upper, spacing);
        if lower >= upper {
            panic_with_error!(env, Error::InvalidTickRange);
        }
        if lower < MIN_TICK || upper > MAX_TICK {
            panic_with_error!(env, Error::InvalidTick);
        }

        let sqrt_current = Self::sqrt_price(&env);
        let sqrt_lower = sqrt_price_at_tick(&env, lower);
        let sqrt_upper = sqrt_price_at_tick(&env, upper);
        let amount_a = amount_a.max(0) as u128;
        let amount_b = amount_b.max(0) as u128;

        let liquidity = if sqrt_current <= sqrt_lower {
            liquidity_for_amount0(&env, amount_a, sqrt_lower, sqrt_upper)
        } else if sqrt_current >= sqrt_upper {
            liquidity_for_amount1(&env, amount_b, sqrt_lower, sqrt_upper)
        } else {
            let from_a = liquidity_for_amount0(&env, amount_a, sqrt_current, sqrt_upper);
            let from_b = liquidity_for_amount1(&env, amount_b, sqrt_lower, sqrt_current);
            from_a.min(from_b)
        };
        if liquidity == 0 {
            panic_with_error!(env, Error::ZeroLiquidity);
        }

        let required_a = if sqrt_current < sqrt_upper {
            amount0_for_liquidity(
                &env,
                liquidity,
                sqrt_current.max(sqrt_lower),
                sqrt_upper,
            )
        } else {
            0
        };
        let required_b = if sqrt_current > sqrt_lower {
            amount1_for_liquidity(
                &env,
                liquidity,
                sqrt_lower,
                sqrt_current.min(sqrt_upper),
            )
        } else {
            0
        };

        let token_a = Self::token_a(&env);
        let token_b = Self::token_b(&env);
        if required_a > 0 {
            token::Client::new(&env, &token_a).transfer(
                &to,
                &env.current_contract_address(),
                &i128::try_from(required_a)
                    .unwrap_or_else(|_| panic_with_error!(env, Error::Overflow)),
            );
        }
        if required_b > 0 {
            token::Client::new(&env, &token_b).transfer(
                &to,
                &env.current_contract_address(),
                &i128::try_from(required_b)
                    .unwrap_or_else(|_| panic_with_error!(env, Error::Overflow)),
            );
        }
        let deposited_a = i128::try_from(required_a)
            .unwrap_or_else(|_| panic_with_error!(env, Error::Overflow));
        let deposited_b = i128::try_from(required_b)
            .unwrap_or_else(|_| panic_with_error!(env, Error::Overflow));
        Self::adjust_reserves(&env, deposited_a, deposited_b);

        let liquidity_i128 = i128::try_from(liquidity)
            .unwrap_or_else(|_| panic_with_error!(env, Error::Overflow));
        let mut lower_info = Self::read_tick(&env, lower);
        lower_info.liquidity_gross += liquidity_i128;
        lower_info.liquidity_net += liquidity_i128;
        Self::set_tick_info(&env, lower, &lower_info);
        let mut upper_info = Self::read_tick(&env, upper);
        upper_info.liquidity_gross += liquidity_i128;
        upper_info.liquidity_net -= liquidity_i128;
        Self::set_tick_info(&env, upper, &upper_info);

        if sqrt_current > sqrt_lower && sqrt_current < sqrt_upper {
            let total = Self::liquidity(&env) + liquidity_i128;
            env.storage().instance().set(&DataKey::Liquidity, &total);
        }

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextPositionId)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextPositionId, &(id + 1));
        let position = Position {
            owner: to.clone(),
            tick_lower: lower,
            tick_upper: upper,
            liquidity: liquidity_i128,
            fee_growth_inside_0: Self::fee_growth_0(&env),
            fee_growth_inside_1: Self::fee_growth_1(&env),
            tokens_owed_0: 0,
            tokens_owed_1: 0,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Position(id), &position);
        Self::bump_position_ttl(&env, id);

        // Mint the position as an NFT in the nft-deed crate's position-NFT
        // contract, when one is configured.
        if let Some(nft) = Self::position_nft(&env) {
            let args: Vec<Val> = (to.clone(), id, lower, upper, liquidity_i128).into_val(&env);
            env.invoke_contract::<u64>(&nft, &Symbol::new(&env, "mint_position"), args);
        }

        env.events().publish(
            (symbol_short!("add_liq"), to),
            (id, lower, upper, liquidity_i128),
        );
        (id, liquidity_i128)
    }

    /// Burn `liquidity` from a position and return both tokens to its owner.
    /// Fees accrued so far are paid in the same call.
    pub fn remove_liquidity(env: Env, owner: Address, position_id: u64, liquidity: i128) -> (i128, i128) {
        owner.require_auth();
        let mut position = Self::load_position(&env, position_id);
        if position.owner != owner {
            panic_with_error!(env, Error::NotPositionOwner);
        }
        if liquidity <= 0 || liquidity > position.liquidity {
            panic_with_error!(env, Error::InvalidAmount);
        }

        let sqrt_current = Self::sqrt_price(&env);
        let sqrt_lower = sqrt_price_at_tick(&env, position.tick_lower);
        let sqrt_upper = sqrt_price_at_tick(&env, position.tick_upper);
        let (amount_a, amount_b) = if sqrt_current <= sqrt_lower {
            (
                amount0_for_liquidity(&env, liquidity as u128, sqrt_lower, sqrt_upper),
                0,
            )
        } else if sqrt_current >= sqrt_upper {
            (
                0,
                amount1_for_liquidity(&env, liquidity as u128, sqrt_lower, sqrt_upper),
            )
        } else {
            (
                amount0_for_liquidity(&env, liquidity as u128, sqrt_current, sqrt_upper),
                amount1_for_liquidity(&env, liquidity as u128, sqrt_lower, sqrt_current),
            )
        };
        let amount_a = i128::try_from(amount_a)
            .unwrap_or_else(|_| panic_with_error!(env, Error::Overflow));
        let amount_b = i128::try_from(amount_b)
            .unwrap_or_else(|_| panic_with_error!(env, Error::Overflow));

        let mut lower_info = Self::read_tick(&env, position.tick_lower);
        lower_info.liquidity_gross -= liquidity;
        lower_info.liquidity_net -= liquidity;
        Self::set_tick_info(&env, position.tick_lower, &lower_info);
        let mut upper_info = Self::read_tick(&env, position.tick_upper);
        upper_info.liquidity_gross -= liquidity;
        upper_info.liquidity_net += liquidity;
        Self::set_tick_info(&env, position.tick_upper, &upper_info);

        if sqrt_current > sqrt_lower && sqrt_current < sqrt_upper {
            let total = Self::liquidity(&env) - liquidity;
            env.storage().instance().set(&DataKey::Liquidity, &total);
        }

        position.liquidity -= liquidity;
        let owed = Self::fees_owed(&env, &position);
        position.tokens_owed_0 = 0;
        position.tokens_owed_1 = 0;
        position.fee_growth_inside_0 = Self::fee_growth_0(&env);
        position.fee_growth_inside_1 = Self::fee_growth_1(&env);
        env.storage()
            .persistent()
            .set(&DataKey::Position(position_id), &position);
        Self::bump_position_ttl(&env, position_id);

        let token_a = Self::token_a(&env);
        let token_b = Self::token_b(&env);
        if amount_a + owed.0 > 0 {
            token::Client::new(&env, &token_a).transfer(
                &env.current_contract_address(),
                &owner,
                &(amount_a + owed.0),
            );
        }
        if amount_b + owed.1 > 0 {
            token::Client::new(&env, &token_b).transfer(
                &env.current_contract_address(),
                &owner,
                &(amount_b + owed.1),
            );
        }
        Self::adjust_reserves(&env, -(amount_a + owed.0), -(amount_b + owed.1));
        env.events().publish(
            (symbol_short!("rm_liq"), owner),
            (position_id, amount_a, amount_b, owed.0, owed.1),
        );
        (amount_a + owed.0, amount_b + owed.1)
    }

    /// Withdraw accrued fees without touching the liquidity.
    pub fn collect_fees(env: Env, owner: Address, position_id: u64) -> (i128, i128) {
        owner.require_auth();
        let mut position = Self::load_position(&env, position_id);
        if position.owner != owner {
            panic_with_error!(env, Error::NotPositionOwner);
        }
        let owed = Self::fees_owed(&env, &position);
        position.tokens_owed_0 = 0;
        position.tokens_owed_1 = 0;
        position.fee_growth_inside_0 = Self::fee_growth_0(&env);
        position.fee_growth_inside_1 = Self::fee_growth_1(&env);
        env.storage()
            .persistent()
            .set(&DataKey::Position(position_id), &position);
        Self::bump_position_ttl(&env, position_id);
        if owed.0 > 0 {
            token::Client::new(&env, &Self::token_a(&env)).transfer(
                &env.current_contract_address(),
                &owner,
                &owed.0,
            );
        }
        if owed.1 > 0 {
            token::Client::new(&env, &Self::token_b(&env)).transfer(
                &env.current_contract_address(),
                &owner,
                &owed.1,
            );
        }
        Self::adjust_reserves(&env, -owed.0, -owed.1);
        env.events()
            .publish((symbol_short!("fees"), owner), (position_id, owed.0, owed.1));
        owed
    }

    // ------------------------------------------------------------------
    // Swap
    // ------------------------------------------------------------------

    /// Swap along the tick curve.
    ///
    /// `amount_specified > 0` is the exact input amount; a negative value
    /// requests that much output. `sqrt_price_limit` bounds the price the
    /// trade may reach; `max_ticks_crossed` bounds how many initialised ticks
    /// the loop may cross before the call reverts with
    /// `TooManyTicksCrossed`.
    pub fn swap(
        env: Env,
        to: Address,
        zero_for_one: bool,
        amount_specified: i128,
        sqrt_price_limit: u128,
        max_ticks_crossed: u32,
    ) -> SwapResult {
        to.require_auth();
        Self::check_compliance(&env, &to);
        if amount_specified == 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        let configured = Self::max_ticks_crossed(&env);
        let limit = max_ticks_crossed.min(configured);
        if limit == 0 {
            panic_with_error!(env, Error::TooManyTicksCrossed);
        }
        let mut sqrt_price = Self::sqrt_price(&env);
        if zero_for_one {
            if sqrt_price_limit >= sqrt_price || sqrt_price_limit < MIN_SQRT_PRICE {
                panic_with_error!(env, Error::InvalidPriceLimit);
            }
        } else if sqrt_price_limit <= sqrt_price || sqrt_price_limit > MAX_SQRT_PRICE {
            panic_with_error!(env, Error::InvalidPriceLimit);
        }

        let fee_bps = Self::fee_bps(&env) as u128;
        let spacing = Self::tick_spacing(&env);
        let mut liquidity = Self::liquidity(&env);
        if liquidity <= 0 {
            panic_with_error!(env, Error::InsufficientLiquidity);
        }
        let mut current_tick = Self::current_tick(&env);
        let mut fee_growth_0 = Self::fee_growth_0(&env);
        let mut fee_growth_1 = Self::fee_growth_1(&env);
        let exact_in = amount_specified > 0;
        let mut remaining = amount_specified.unsigned_abs();
        let mut total_in: u128 = 0;
        let mut total_out: u128 = 0;
        let mut ticks_crossed: u32 = 0;

        while remaining > 0 {
            let boundary_tick = Self::next_boundary_tick(
                &env,
                current_tick,
                spacing,
                zero_for_one,
                tick_at_sqrt_price(&env, sqrt_price_limit),
            );
            let mut target_tick = boundary_tick;
            let mut target_sqrt = sqrt_price_at_tick(&env, boundary_tick);
            let mut limit_hit = false;
            if zero_for_one {
                if target_sqrt < sqrt_price_limit {
                    target_sqrt = sqrt_price_limit;
                    target_tick = tick_at_sqrt_price(&env, sqrt_price_limit);
                    limit_hit = true;
                }
            } else if target_sqrt > sqrt_price_limit {
                target_sqrt = sqrt_price_limit;
                target_tick = tick_at_sqrt_price(&env, sqrt_price_limit);
                limit_hit = true;
            }
            if target_sqrt == sqrt_price {
                break;
            }

            let liquidity_u = liquidity.max(0) as u128;
            let (net_in_to_target, out_to_target) = if zero_for_one {
                (
                    amount0_in_for_price_move(&env, sqrt_price, target_sqrt, liquidity_u),
                    amount1_out_for_price_move(&env, sqrt_price, target_sqrt, liquidity_u),
                )
            } else {
                (
                    amount1_in_for_price_move(&env, sqrt_price, target_sqrt, liquidity_u),
                    amount0_out_for_price_move(&env, sqrt_price, target_sqrt, liquidity_u),
                )
            };
            let gross_to_target = Self::add_fee(&env, net_in_to_target, fee_bps);

            if exact_in && remaining <= gross_to_target {
                // The trade ends inside this range.
                let fee = Self::fee_part(&env, remaining, fee_bps);
                let net_in = remaining - fee;
                let new_sqrt = if zero_for_one {
                    sqrt_price_after_amount0_in(&env, sqrt_price, liquidity_u, net_in)
                } else {
                    sqrt_price_after_amount1_in(&env, sqrt_price, liquidity_u, net_in)
                };
                let out = if zero_for_one {
                    amount1_out_for_price_move(&env, sqrt_price, new_sqrt, liquidity_u)
                } else {
                    amount0_out_for_price_move(&env, sqrt_price, new_sqrt, liquidity_u)
                };
                total_in += remaining;
                total_out += out;
                if zero_for_one {
                    fee_growth_0 = fee_growth_0.saturating_add(fee_growth_delta(&env, fee, liquidity_u));
                } else {
                    fee_growth_1 = fee_growth_1.saturating_add(fee_growth_delta(&env, fee, liquidity_u));
                }
                sqrt_price = new_sqrt;
                remaining = 0;
            } else if !exact_in && remaining <= out_to_target {
                // Exact output that completes inside this range.
                let new_sqrt = if zero_for_one {
                    sqrt_price_after_amount1_out(&env, sqrt_price, liquidity_u, remaining)
                } else {
                    sqrt_price_after_amount0_out(&env, sqrt_price, liquidity_u, remaining)
                };
                let net_in = if zero_for_one {
                    amount0_in_for_price_move(&env, sqrt_price, new_sqrt, liquidity_u)
                } else {
                    amount1_in_for_price_move(&env, sqrt_price, new_sqrt, liquidity_u)
                };
                let gross_in = Self::add_fee(&env, net_in, fee_bps);
                let fee = gross_in - net_in;
                total_in += gross_in;
                total_out += remaining;
                if zero_for_one {
                    fee_growth_0 = fee_growth_0.saturating_add(fee_growth_delta(&env, fee, liquidity_u));
                } else {
                    fee_growth_1 = fee_growth_1.saturating_add(fee_growth_delta(&env, fee, liquidity_u));
                }
                sqrt_price = new_sqrt;
                remaining = 0;
            } else {
                // Consume the whole step and cross the boundary.
                let net_in = net_in_to_target;
                let gross_in = gross_to_target;
                let fee = gross_in - net_in;
                if gross_in > remaining {
                    // Rounding pushed the step past the remaining input; cap
                    // it at the remaining gross amount and stop here.
                    let fee = Self::fee_part(&env, remaining, fee_bps);
                    let net_in = remaining - fee;
                    let new_sqrt = if zero_for_one {
                        sqrt_price_after_amount0_in(&env, sqrt_price, liquidity_u, net_in)
                    } else {
                        sqrt_price_after_amount1_in(&env, sqrt_price, liquidity_u, net_in)
                    };
                    let out = if zero_for_one {
                        amount1_out_for_price_move(&env, sqrt_price, new_sqrt, liquidity_u)
                    } else {
                        amount0_out_for_price_move(&env, sqrt_price, new_sqrt, liquidity_u)
                    };
                    total_in += remaining;
                    total_out += out;
                    if zero_for_one {
                        fee_growth_0 = fee_growth_0
                            .saturating_add(fee_growth_delta(&env, fee, liquidity_u));
                    } else {
                        fee_growth_1 = fee_growth_1
                            .saturating_add(fee_growth_delta(&env, fee, liquidity_u));
                    }
                    sqrt_price = new_sqrt;
                    remaining = 0;
                    break;
                }
                let step_out = if exact_in { out_to_target } else { remaining.min(out_to_target) };
                total_in += gross_in;
                total_out += step_out;
                if exact_in {
                    remaining -= gross_in;
                } else {
                    remaining -= step_out;
                }
                if zero_for_one {
                    fee_growth_0 = fee_growth_0.saturating_add(fee_growth_delta(&env, fee, liquidity_u));
                } else {
                    fee_growth_1 = fee_growth_1.saturating_add(fee_growth_delta(&env, fee, liquidity_u));
                }
                sqrt_price = target_sqrt;
                current_tick = target_tick;
                if limit_hit {
                    break;
                }
                // Cross the boundary: moving down removes net liquidity,
                // moving up adds it.
                let info = Self::read_tick(&env, boundary_tick);
                if zero_for_one {
                    liquidity -= info.liquidity_net;
                    current_tick = boundary_tick - 1;
                } else {
                    liquidity += info.liquidity_net;
                }
                ticks_crossed += 1;
                if ticks_crossed >= limit && remaining > 0 {
                    panic_with_error!(env, Error::TooManyTicksCrossed);
                }
                if liquidity <= 0 && remaining > 0 {
                    // Nothing left to trade against.
                    panic_with_error!(env, Error::InsufficientLiquidity);
                }
            }
        }

        if total_in == 0 || total_out == 0 {
            panic_with_error!(env, Error::InsufficientLiquidity);
        }
        let total_in_i = i128::try_from(total_in)
            .unwrap_or_else(|_| panic_with_error!(env, Error::Overflow));
        let total_out_i = i128::try_from(total_out)
            .unwrap_or_else(|_| panic_with_error!(env, Error::Overflow));
        if total_out_i > Self::reserve_out(&env, zero_for_one) {
            panic_with_error!(env, Error::InsufficientLiquidity);
        }

        let (token_in, token_out) = if zero_for_one {
            (Self::token_a(&env), Self::token_b(&env))
        } else {
            (Self::token_b(&env), Self::token_a(&env))
        };
        token::Client::new(&env, &token_in).transfer(
            &to,
            &env.current_contract_address(),
            &total_in_i,
        );
        token::Client::new(&env, &token_out).transfer(
            &env.current_contract_address(),
            &to,
            &total_out_i,
        );

        env.storage()
            .instance()
            .set(&DataKey::SqrtPrice, &sqrt_price);
        env.storage()
            .instance()
            .set(&DataKey::CurrentTick, &tick_at_sqrt_price(&env, sqrt_price));
        env.storage().instance().set(&DataKey::Liquidity, &liquidity);
        env.storage()
            .instance()
            .set(&DataKey::FeeGrowth0, &fee_growth_0);
        env.storage()
            .instance()
            .set(&DataKey::FeeGrowth1, &fee_growth_1);
        if zero_for_one {
            Self::adjust_reserves(&env, total_in_i, -total_out_i);
        } else {
            Self::adjust_reserves(&env, -total_out_i, total_in_i);
        }

        let result = SwapResult {
            amount_in: total_in_i,
            amount_out: total_out_i,
            sqrt_price_after: sqrt_price,
            ticks_crossed,
        };
        env.events().publish(
            (symbol_short!("swap"), to),
            (
                zero_for_one,
                total_in_i,
                total_out_i,
                sqrt_price,
                ticks_crossed,
            ),
        );
        result
    }

    // ------------------------------------------------------------------
    // Views
    // ------------------------------------------------------------------

    pub fn pool_state(env: Env) -> PoolState {
        PoolState {
            sqrt_price: Self::sqrt_price(&env),
            tick: Self::current_tick(&env),
            liquidity: Self::liquidity(&env),
            reserve_a: env.storage().instance().get(&DataKey::ReserveA).unwrap_or(0),
            reserve_b: env.storage().instance().get(&DataKey::ReserveB).unwrap_or(0),
            fee_growth_0: Self::fee_growth_0(&env),
            fee_growth_1: Self::fee_growth_1(&env),
        }
    }

    pub fn get_position(env: Env, position_id: u64) -> Position {
        Self::load_position(&env, position_id)
    }

    pub fn tick_info(env: Env, tick: i32) -> TickInfo {
        Self::read_tick(&env, tick)
    }

    fn sqrt_price(env: &Env) -> u128 {
        env.storage()
            .instance()
            .get(&DataKey::SqrtPrice)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    /// The tick range a position with `liquidity` covers at the current price.
    pub fn position_amounts(env: Env, position_id: u64) -> (i128, i128) {
        let position = Self::load_position(&env, position_id);
        let sqrt_current = Self::sqrt_price(&env);
        let sqrt_lower = sqrt_price_at_tick(&env, position.tick_lower);
        let sqrt_upper = sqrt_price_at_tick(&env, position.tick_upper);
        let liquidity = position.liquidity.max(0) as u128;
        let (amount_a, amount_b) = if sqrt_current <= sqrt_lower {
            (amount0_for_liquidity(&env, liquidity, sqrt_lower, sqrt_upper), 0)
        } else if sqrt_current >= sqrt_upper {
            (0, amount1_for_liquidity(&env, liquidity, sqrt_lower, sqrt_upper))
        } else {
            (
                amount0_for_liquidity(&env, liquidity, sqrt_current, sqrt_upper),
                amount1_for_liquidity(&env, liquidity, sqrt_lower, sqrt_current),
            )
        };
        (
            i128::try_from(amount_a).unwrap_or(i128::MAX),
            i128::try_from(amount_b).unwrap_or(i128::MAX),
        )
    }

    // ------------------------------------------------------------------
    // Internal
    // ------------------------------------------------------------------

    fn require_admin(env: &Env, admin: &Address) {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        admin.require_auth();
        if stored != *admin {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    fn check_compliance(env: &Env, address: &Address) {
        let compliance: Address = env
            .storage()
            .instance()
            .get(&DataKey::Compliance)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        let args: Vec<Val> = (address.clone(),).into_val(env);
        let allowed: bool =
            env.invoke_contract(&compliance, &Symbol::new(env, "is_allowed"), args);
        if !allowed {
            panic_with_error!(env, Error::ComplianceCheckFailed);
        }
    }

    fn align_tick(env: &Env, tick: i32, spacing: i32) -> i32 {
        let mut aligned = tick - (tick % spacing);
        if tick < 0 && tick % spacing != 0 {
            aligned -= spacing;
        }
        let _ = env;
        aligned
    }

    fn read_tick(env: &Env, tick: i32) -> TickInfo {
        env.storage()
            .persistent()
            .get(&DataKey::Tick(tick))
            .unwrap_or(TickInfo {
                liquidity_gross: 0,
                liquidity_net: 0,
            })
    }

    fn set_tick_info(env: &Env, tick: i32, info: &TickInfo) {
        env.storage()
            .persistent()
            .set(&DataKey::Tick(tick), info);
    }

    fn load_position(env: &Env, position_id: u64) -> Position {
        env.storage()
            .persistent()
            .get(&DataKey::Position(position_id))
            .unwrap_or_else(|| panic_with_error!(env, Error::PositionNotFound))
    }

    fn bump_position_ttl(env: &Env, position_id: u64) {
        let key = DataKey::Position(position_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, 17_280, 535_680);
    }

    fn fees_owed(env: &Env, position: &Position) -> (i128, i128) {
        let growth_0 = Self::fee_growth_0(env).wrapping_sub(position.fee_growth_inside_0);
        let growth_1 = Self::fee_growth_1(env).wrapping_sub(position.fee_growth_inside_1);
        let liquidity = position.liquidity.max(0) as u128;
        let owed_0 = mul_div(env, growth_0, liquidity, Q64);
        let owed_1 = mul_div(env, growth_1, liquidity, Q64);
        (
            position.tokens_owed_0 + i128::try_from(owed_0).unwrap_or(i128::MAX),
            position.tokens_owed_1 + i128::try_from(owed_1).unwrap_or(i128::MAX),
        )
    }

    /// Next tick the swap steps to: the nearest initialised tick, or, if none is
    /// found before the price limit or within `MAX_SCAN_TICKS`, the last tick
    /// scanned (crossing an uninitialised tick is a no-op), so no initialised
    /// tick is ever skipped.
    fn next_boundary_tick(
        env: &Env,
        from: i32,
        spacing: i32,
        zero_for_one: bool,
        limit_tick: i32,
    ) -> i32 {
        // Initialised ticks are multiples of `spacing`; the current tick is
        // the floor of the price and may not be aligned after a partial swap.
        let mut tick = if zero_for_one {
            floor_div(from, spacing) * spacing
        } else {
            ceil_div(from, spacing) * spacing
        };
        if zero_for_one && tick >= from {
            tick -= spacing;
        }
        if !zero_for_one && tick <= from {
            tick += spacing;
        }
        for _ in 0..MAX_SCAN_TICKS {
            if tick <= MIN_TICK {
                return MIN_TICK;
            }
            if tick >= MAX_TICK {
                return MAX_TICK;
            }
            let past_limit = if zero_for_one { tick <= limit_tick } else { tick >= limit_tick };
            if past_limit || Self::read_tick(env, tick).liquidity_gross > 0 {
                return tick;
            }
            tick = if zero_for_one { tick - spacing } else { tick + spacing };
        }
        tick
    }

    fn fee_part(env: &Env, amount: u128, fee_bps: u128) -> u128 {
        mul_div(env, amount, fee_bps, BPS_SCALE)
    }

    fn add_fee(env: &Env, net: u128, fee_bps: u128) -> u128 {
        if fee_bps == 0 {
            return net;
        }
        // gross = net * 10000 / (10000 - fee_bps), rounded up.
        let denominator = BPS_SCALE - fee_bps;
        let scaled = net.saturating_mul(BPS_SCALE);
        scaled / denominator + (scaled % denominator != 0) as u128
    }

    fn adjust_reserves(env: &Env, delta_a: i128, delta_b: i128) {
        let reserve_a: i128 = env.storage().instance().get(&DataKey::ReserveA).unwrap_or(0);
        let reserve_b: i128 = env.storage().instance().get(&DataKey::ReserveB).unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::ReserveA, &(reserve_a + delta_a));
        env.storage()
            .instance()
            .set(&DataKey::ReserveB, &(reserve_b + delta_b));
    }

    fn reserve_out(env: &Env, zero_for_one: bool) -> i128 {
        let key = if zero_for_one {
            DataKey::ReserveB
        } else {
            DataKey::ReserveA
        };
        env.storage().instance().get(&key).unwrap_or(0)
    }

    fn token_a(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::TokenA)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    fn token_b(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::TokenB)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    fn compliance(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Compliance)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    fn tick_spacing(env: &Env) -> i32 {
        env.storage()
            .instance()
            .get(&DataKey::TickSpacing)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    fn fee_bps(env: &Env) -> u32 {
        env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0)
    }

    fn max_ticks_crossed(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MaxTicksCrossed)
            .unwrap_or(0)
    }

    fn position_nft(env: &Env) -> Option<Address> {
        env.storage()
            .instance()
            .get::<_, Option<Address>>(&DataKey::PositionNft)
            .unwrap_or(None)
    }

    fn liquidity(env: &Env) -> i128 {
        env.storage().instance().get(&DataKey::Liquidity).unwrap_or(0)
    }

    fn current_tick(env: &Env) -> i32 {
        env.storage().instance().get(&DataKey::CurrentTick).unwrap_or(0)
    }

    fn fee_growth_0(env: &Env) -> u128 {
        env.storage().instance().get(&DataKey::FeeGrowth0).unwrap_or(0)
    }

    fn fee_growth_1(env: &Env) -> u128 {
        env.storage().instance().get(&DataKey::FeeGrowth1).unwrap_or(0)
    }
}

/// Floor division for a positive divisor, unlike Rust's truncating `%`.
fn floor_div(a: i32, b: i32) -> i32 {
    let q = a / b;
    if a % b != 0 && (a < 0) != (b < 0) {
        q - 1
    } else {
        q
    }
}

/// Ceiling division for a positive divisor.
fn ceil_div(a: i32, b: i32) -> i32 {
    let q = a / b;
    if a % b != 0 && (a < 0) == (b < 0) {
        q + 1
    } else {
        q
    }
}
