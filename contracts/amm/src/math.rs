//! Fixed-point (Q64.64) tick and liquidity math for the CLMM (issue #361).
//!
//! Prices are square roots of the token1-per-token0 price, stored as
//! `sqrt_price * 2^64`. Tick `i` corresponds to a price of `1.0001^i`; the
//! functions here convert between ticks and those square roots with binary
//! exponentiation and 256-bit intermediates, so no floating point is needed
//! inside the contract.
//!
//! Liquidity `L` follows the Uniswap v3 convention. Inside a range
//! `[sqrt_lower, sqrt_upper)` the virtual reserves are
//!
//! ```text
//! x = L / sqrt_price        y = L * sqrt_price
//! amount0 = L * (sqrt_upper - sqrt_price) / (sqrt_price * sqrt_upper)
//! amount1 = L * (sqrt_price - sqrt_lower)
//! ```
//!
//! so `x * y = L^2` is constant while a swap stays inside one tick range.

use soroban_sdk::{Env, U256};

use crate::Error;

/// 1.0 in Q64.64.
pub const Q64: u128 = 1u128 << 64;
/// Smallest supported tick.
pub const MIN_TICK: i32 = -100_000;
/// Largest supported tick.
pub const MAX_TICK: i32 = 100_000;
/// Lower bound for a valid sqrt price (~tick -100 000, with headroom).
pub const MIN_SQRT_PRICE: u128 = 1u128 << 50;
/// Upper bound for a valid sqrt price (~tick +100 000, with headroom).
pub const MAX_SQRT_PRICE: u128 = 1u128 << 76;
/// `sqrt(1.0001)` in Q64.64.
pub const SQRT_1_0001_Q64: u128 = 18_447_666_387_854_807_001;

fn u256(env: &Env, value: u128) -> U256 {
    U256::from_u128(env, value)
}

fn from_u256(env: &Env, value: U256) -> u128 {
    value
        .to_u128()
        .unwrap_or_else(|| soroban_sdk::panic_with_error!(env, Error::Overflow))
}

fn ratio(env: &Env, numerator: U256, denominator: U256) -> u128 {
    if denominator == u256(env, 0) {
        soroban_sdk::panic_with_error!(env, Error::DivisionByZero);
    }
    from_u256(env, numerator.div(&denominator))
}

fn ceil_div_u256(env: &Env, numerator: U256, denominator: U256) -> u128 {
    if denominator == u256(env, 0) {
        soroban_sdk::panic_with_error!(env, Error::DivisionByZero);
    }
    let q = numerator.div(&denominator);
    let rounded = if q.mul(&denominator) == numerator { q } else { q.add(&u256(env, 1)) };
    from_u256(env, rounded)
}

fn ratio_up(env: &Env, numerator: U256, denominator: U256) -> u128 {
    ceil_div_u256(env, numerator, denominator)
}

/// `a * b / denominator` with a 256-bit intermediate product.
pub fn mul_div(env: &Env, a: u128, b: u128, denominator: u128) -> u128 {
    if denominator == 0 {
        soroban_sdk::panic_with_error!(env, Error::DivisionByZero);
    }
    ratio(env, u256(env, a).mul(&u256(env, b)), u256(env, denominator))
}

/// `a * b` in Q64.64.
pub fn mul_q64(env: &Env, a: u128, b: u128) -> u128 {
    mul_div(env, a, b, Q64)
}

/// `a / b` in Q64.64.
pub fn div_q64(env: &Env, a: u128, b: u128) -> u128 {
    mul_div(env, a, Q64, b)
}

/// Square root of a non-negative integer, floored.
pub fn isqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    // Newton iteration from a power-of-two upper bound.
    let mut x = 1u128 << ((128 - value.leading_zeros() + 1) / 2);
    loop {
        let next = (x + value / x) / 2;
        if next >= x {
            break;
        }
        x = next;
    }
    x
}

/// `sqrt(1.0001^tick)` in Q64.64.
pub fn sqrt_price_at_tick(env: &Env, tick: i32) -> u128 {
    if tick < MIN_TICK || tick > MAX_TICK {
        soroban_sdk::panic_with_error!(env, Error::InvalidTick);
    }
    let mut exponent = tick.unsigned_abs();
    let mut result = Q64;
    let mut base = SQRT_1_0001_Q64;
    while exponent > 0 {
        if exponent & 1 == 1 {
            result = mul_q64(env, result, base);
        }
        base = mul_q64(env, base, base);
        exponent >>= 1;
    }
    if tick < 0 {
        div_q64(env, Q64, result)
    } else {
        result
    }
}

/// Greatest tick whose sqrt price is `<= sqrt_price`.
pub fn tick_at_sqrt_price(env: &Env, sqrt_price: u128) -> i32 {
    let mut low = MIN_TICK;
    let mut high = MAX_TICK;
    while low < high {
        let mid = low + (high - low + 1) / 2;
        if sqrt_price_at_tick(env, mid) <= sqrt_price {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

/// Liquidity that `amount0` of token0 represents between `sqrt_a` and
/// `sqrt_b`.
pub fn liquidity_for_amount0(env: &Env, amount0: u128, sqrt_a: u128, sqrt_b: u128) -> u128 {
    let (low, high) = order(sqrt_a, sqrt_b);
    let width = high - low;
    if width == 0 {
        return 0;
    }
    // L = amount0 * Sa * Sb / (Q * (Sb - Sa))
    let numerator = u256(env, amount0).mul(&u256(env, low)).mul(&u256(env, high));
    let denominator = u256(env, Q64).mul(&u256(env, width));
    ratio(env, numerator, denominator)
}

/// Liquidity that `amount1` of token1 represents between `sqrt_a` and
/// `sqrt_b`.
pub fn liquidity_for_amount1(env: &Env, amount1: u128, sqrt_a: u128, sqrt_b: u128) -> u128 {
    let (low, high) = order(sqrt_a, sqrt_b);
    let width = high - low;
    if width == 0 {
        return 0;
    }
    // L = amount1 * Q / (Sb - Sa)
    mul_div(env, amount1, Q64, width)
}

/// Token0 required by `liquidity` between `sqrt_a` and `sqrt_b`, rounded up.
pub fn amount0_for_liquidity(
    env: &Env,
    liquidity: u128,
    sqrt_a: u128,
    sqrt_b: u128,
) -> u128 {
    let (low, high) = order(sqrt_a, sqrt_b);
    if low == high {
        return 0;
    }
    // amount0 = L * (Sb - Sa) * Q / (Sa * Sb)
    let numerator = u256(env, liquidity)
        .mul(&u256(env, high - low))
        .mul(&u256(env, Q64));
    let denominator = u256(env, low).mul(&u256(env, high));
    ratio(env, numerator, denominator)
}

/// Token1 required by `liquidity` between `sqrt_a` and `sqrt_b`, rounded up.
pub fn amount1_for_liquidity(
    env: &Env,
    liquidity: u128,
    sqrt_a: u128,
    sqrt_b: u128,
) -> u128 {
    let (low, high) = order(sqrt_a, sqrt_b);
    if low == high {
        return 0;
    }
    // amount1 = L * (Sb - Sa) / Q
    let numerator = u256(env, liquidity).mul(&u256(env, high - low));
    ratio(env, numerator, u256(env, Q64))
}

/// Price after adding `amount_in` of token0 to a range with `liquidity`.
/// `sqrt_next = L * sqrt / (L * Q + amount_in * sqrt)`.
pub fn sqrt_price_after_amount0_in(
    env: &Env,
    sqrt_price: u128,
    liquidity: u128,
    amount_in: u128,
) -> u128 {
    // S' = L * Q * S / (L * Q + in * S), rounded up so the pool never overpays.
    let lq = u256(env, liquidity).mul(&u256(env, Q64));
    let denominator = lq.add(&u256(env, amount_in).mul(&u256(env, sqrt_price)));
    ratio_up(env, lq.mul(&u256(env, sqrt_price)), denominator)
}

/// Price after adding `amount_in` of token1 to a range with `liquidity`.
pub fn sqrt_price_after_amount1_in(
    env: &Env,
    sqrt_price: u128,
    liquidity: u128,
    amount_in: u128,
) -> u128 {
    if liquidity == 0 {
        return sqrt_price;
    }
    sqrt_price.saturating_add(mul_div(env, amount_in, Q64, liquidity))
}

/// Token0 input needed to move from `sqrt_price` down to `sqrt_target`.
pub fn amount0_in_for_price_move(
    env: &Env,
    sqrt_price: u128,
    sqrt_target: u128,
    liquidity: u128,
) -> u128 {
    if sqrt_target >= sqrt_price || liquidity == 0 {
        return 0;
    }
    // L * (S - St) / (S * St) in Q64.64.
    let numerator = u256(env, liquidity)
        .mul(&u256(env, sqrt_price - sqrt_target))
        .mul(&u256(env, Q64));
    let denominator = u256(env, sqrt_price).mul(&u256(env, sqrt_target));
    ratio(env, numerator, denominator)
}

/// Token1 input needed to move from `sqrt_price` up to `sqrt_target`.
pub fn amount1_in_for_price_move(
    env: &Env,
    sqrt_price: u128,
    sqrt_target: u128,
    liquidity: u128,
) -> u128 {
    if sqrt_target <= sqrt_price {
        return 0;
    }
    ceil_div(env, liquidity.saturating_mul(sqrt_target - sqrt_price), Q64)
}

/// Token0 output released while the price moves up from `sqrt_price` to
/// `sqrt_target` (token1 in). Rounded down so the pool never overpays.
pub fn amount0_out_for_price_move(
    env: &Env,
    sqrt_price: u128,
    sqrt_target: u128,
    liquidity: u128,
) -> u128 {
    if sqrt_target <= sqrt_price || liquidity == 0 {
        return 0;
    }
    // L * (St - S) / (S * St) in Q64.64.
    let numerator = u256(env, liquidity)
        .mul(&u256(env, sqrt_target - sqrt_price))
        .mul(&u256(env, Q64));
    let denominator = u256(env, sqrt_price).mul(&u256(env, sqrt_target));
    ratio(env, numerator, denominator)
}

/// Token1 output released while the price moves down from `sqrt_price` to
/// `sqrt_target` (token0 in). Rounded down so the pool never overpays.
pub fn amount1_out_for_price_move(
    env: &Env,
    sqrt_price: u128,
    sqrt_target: u128,
    liquidity: u128,
) -> u128 {
    if sqrt_target >= sqrt_price {
        return 0;
    }
    mul_div(env, liquidity, sqrt_price - sqrt_target, Q64)
}

/// Price that sells exactly `amount_out` of token1 for `liquidity` when
/// token0 is the input (zero-for-one), moving the price down.
pub fn sqrt_price_after_amount1_out(
    env: &Env,
    sqrt_price: u128,
    liquidity: u128,
    amount_out: u128,
) -> u128 {
    if liquidity == 0 {
        soroban_sdk::panic_with_error!(env, Error::InsufficientLiquidity);
    }
    // S' = S - out * Q / L, rounded down so the trader pays enough.
    let drop = ceil_div_u256(env, u256(env, amount_out).mul(&u256(env, Q64)), u256(env, liquidity));
    if drop >= sqrt_price {
        soroban_sdk::panic_with_error!(env, Error::InsufficientLiquidity);
    }
    sqrt_price - drop
}

/// Price that sells exactly `amount_out` of token0 for `liquidity` when
/// token1 is the input (one-for-zero), moving the price up.
pub fn sqrt_price_after_amount0_out(
    env: &Env,
    sqrt_price: u128,
    liquidity: u128,
    amount_out: u128,
) -> u128 {
    // S' = L * Q * S / (L * Q - out * S), rounded up so the trader pays enough.
    let lq = u256(env, liquidity).mul(&u256(env, Q64));
    let taken = u256(env, amount_out).mul(&u256(env, sqrt_price));
    if taken >= lq {
        soroban_sdk::panic_with_error!(env, Error::InsufficientLiquidity);
    }
    ratio_up(env, lq.mul(&u256(env, sqrt_price)), lq.sub(&taken))
}

fn order(a: u128, b: u128) -> (u128, u128) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn ceil_div(env: &Env, numerator: u128, denominator: u128) -> u128 {
    if denominator == 0 {
        soroban_sdk::panic_with_error!(env, Error::DivisionByZero);
    }
    numerator / denominator + (numerator % denominator != 0) as u128
}

/// Fee growth per unit of liquidity, in Q64.64.
pub fn fee_growth_delta(env: &Env, fee_amount: u128, liquidity: u128) -> u128 {
    if liquidity == 0 {
        return 0;
    }
    mul_div(env, fee_amount, Q64, liquidity)
}
