//! Multi-Currency Dynamic FX Hedging Pool (Issue #221).
//!
//! Protects international real estate investors receiving dividends in volatile currencies
//! by auto-executing FX forward hedges using DEX liquidity pools:
//! - Ingests FX rate oracle feeds (e.g., EUR/USD, GBP/USD, JPY/USD).
//! - Calculates required FX forward hedge ratios based on projected annual dividend yields.
//! - Executes automated rebalancing swaps when currency fluctuations exceed variance limits.
#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, panic_with_error,
    symbol_short, Address, Env, IntoVal, Symbol,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidRate = 4,
    InvalidVariance = 5,
    PairNotFound = 6,
    VarianceWithinLimit = 7,
    ZeroProjectedYield = 8,
    Overflow = 9,
    // ── V2 FX hedge position engine ────────────────────────────────────────
    /// No V2 position exists for the requested id.
    PositionNotFound = 10,
    /// Locked collateral is insufficient to cover the requested operation.
    InsufficientCollateral = 11,
    /// Notional must be strictly positive.
    InvalidNotional = 12,
    /// Position duration must be strictly positive.
    InvalidDuration = 13,
    /// Reserved for a future designated-keeper allow-list mode (today any address
    /// may rebalance for a fee, so no caller can currently be "not a keeper").
    NotKeeper = 14,
    /// The position has already been closed.
    HedgeClosed = 15,
    /// A cross-contract call into the vaults contract failed.
    VaultCallFailed = 16,
    /// Fixed-point arithmetic overflowed its i128 accumulator.
    MathOverflow = 17,
}

// ── V2 FX hedge position engine ────────────────────────────────────────────

/// Minimal interface the configured `vaults` contract must implement.
///
/// The vaults contract is a *defined interface* for this engine: it must expose
/// exactly these three callable methods so collateral can be locked, released
/// and settled atomically with the hedge lifecycle. See `docs/content/docs/fx-risk.mdx`.
///
/// * `lock_collateral(owner, position_id, amount)` — lock `amount` of `owner`'s
///   collateral against `position_id`.
/// * `release_collateral(position_id, amount)` — release `amount` back to the
///   position owner.
/// * `settle_pnl(position_id, pnl)` — credit (positive) or debit (negative)
///   realised PnL / fees against the position's collateral.
#[contractclient(name = "VaultsClient")]
pub trait Vaults {
    fn lock_collateral(env: &Env, owner: Address, position_id: u64, amount: i128);
    fn release_collateral(env: &Env, position_id: u64, amount: i128);
    fn settle_pnl(env: &Env, position_id: u64, pnl: i128);
}

/// A delta-hedged FX forward position (V2).
///
/// All prices (`strike`, `entry_spot`) use the repo's 10_000-scaled FX rate
/// convention (e.g. `10850` = 1.0850 USD per EUR). `notional`, `collateral`,
/// `hedge_size` are in the vault's base units. `implied_vol_bps` and
/// `current_delta_bps` are basis points (10_000 = 100%).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HedgePositionV2 {
    pub id: u64,
    pub owner: Address,
    pub base_asset: Symbol,
    pub quote_currency: Symbol,
    pub notional: i128,
    pub collateral: i128,
    pub strike: i128,
    pub implied_vol_bps: i128,
    pub duration_ledgers: u32,
    pub entry_ledger: u32,
    pub entry_spot: i128,
    pub current_delta_bps: i128,
    pub hedge_size: i128,
    pub closed: bool,
    pub keeper_fee_bps: i128,
}

/// Fixed-point scale for the Black-Scholes integer math (9 decimal places).
/// Input prices are 10_000-scaled; vol/time are bps. Internally every real
/// number is represented as an i128 `value * FP`.
const FP: i128 = 1_000_000_000;
/// ln(2) in FP.
const LN2_FP: i128 = 693_147_181;
/// e in FP.
const E_FP: i128 = 2_718_281_828;
/// 1 / sqrt(2*pi) in FP.
const INV_SQRT_2PI_FP: i128 = 398_942_280;
/// Abramowitz & Stegun 7.1.26 coefficients in FP.
const AS_P_FP: i128 = 231_641_900;
const AS_B1_FP: i128 = 319_381_530;
const AS_B2_FP: i128 = -356_563_782;
const AS_B3_FP: i128 = 1_781_477_937;
const AS_B4_FP: i128 = -1_821_255_978;
const AS_B5_FP: i128 = 1_330_274_429;

/// Natural logarithm of a positive FP number, returned in FP.
///
/// Range-reduces by powers of two into `[1, 2]` then uses the rapidly
/// convergent series `ln(x) = 2 * (z + z^3/3 + z^5/5 + ...)`,
/// `z = (x-1)/(x+1)`, with `|z| <= 1/3`. Twenty terms give well beyond
/// 1e-9 absolute precision. Extreme inputs (`> 2^200` ratio) are rejected.
fn ln_fixed(mut x: i128) -> i128 {
    if x <= 0 {
        panic!("MathOverflow");
    }
    let mut twos = 0i128;
    while x >= 2 * FP {
        x /= 2; // integer halving loses < 1 FP-unit of relative precision
        twos += 1;
        if twos > 200 {
            panic!("MathOverflow");
        }
    }
    while x < FP {
        x = x.checked_mul(2).expect("MathOverflow");
        twos -= 1;
        if twos < -200 {
            panic!("MathOverflow");
        }
    }
    let z = (x - FP) * FP / (x + FP);
    let z2 = z * z / FP;
    let mut power = z;
    let mut sum = z;
    let mut n = 1i128;
    while n <= 20 {
        power = power * z2 / FP;
        sum += power / (2 * n + 1);
        n += 1;
    }
    sum.checked_mul(2)
        .and_then(|v| v.checked_add(twos.checked_mul(LN2_FP)?))
        .expect("MathOverflow")
}

/// Integer Newton square root of a non-negative FP number, returned in FP.
fn sqrt_fixed(x: i128) -> i128 {
    if x <= 0 {
        return 0;
    }
    let target = x.checked_mul(FP).expect("MathOverflow");
    let mut y = target;
    loop {
        let next = y.checked_add(target / y).expect("MathOverflow") / 2;
        if next >= y {
            return y;
        }
        y = next;
    }
}

/// `exp(-x)` for `x >= 0`, in FP. Uses `e^(-n-f) = 1/(e^n * e^f)` with a
/// Taylor series for the fractional part. Inputs beyond 30 clamp to zero
/// (the value is below 1e-9 there, i.e. below FP resolution).
fn exp_neg_fixed(x: i128) -> i128 {
    if x <= 0 {
        return FP;
    }
    if x > 30 * FP {
        return 0;
    }
    let n = x / FP;
    let f = x % FP;
    let mut term = FP;
    let mut sum = FP;
    let mut k = 1i128;
    while k <= 20 {
        term = term * f / FP / k;
        sum += term;
        k += 1;
    }
    let mut denom = sum;
    let mut i = 0i128;
    while i < n {
        denom = denom.checked_mul(E_FP).expect("MathOverflow") / FP;
        i += 1;
    }
    if denom <= 0 {
        0
    } else {
        FP * FP / denom
    }
}

/// Standard normal CDF `N(x)` for `x >= 0`, in FP, using Abramowitz & Stegun
/// 7.1.26 (max absolute error ~7.5e-8, far below one basis point).
fn norm_cdf_pos_fixed(x: i128) -> i128 {
    if x >= 8 * FP {
        return FP;
    }
    let px = AS_P_FP * x / FP;
    let t = FP * FP / (FP + px);
    let mut poly = AS_B5_FP;
    poly = AS_B4_FP + poly * t / FP;
    poly = AS_B3_FP + poly * t / FP;
    poly = AS_B2_FP + poly * t / FP;
    poly = AS_B1_FP + poly * t / FP;
    // Horner above yields b1 + b2*t + ... + b5*t^4; the A&S series is
    // b1*t + b2*t^2 + ... + b5*t^5, so multiply by one final t.
    poly = poly * t / FP;
    let half_x2 = x * x / FP / 2;
    let phi = INV_SQRT_2PI_FP * exp_neg_fixed(half_x2) / FP;
    let q = phi * poly / FP; // Q(x) = 1 - N(x)
    FP - q
}

/// Represents an FX rate oracle feed quote with fixed-point decimal precision (scaled by 10,000, e.g. 1.0850 -> 10850).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FxRateQuote {
    pub rate: i128, // e.g. 10850 for 1.0850 USD per EUR
    pub last_updated_ledger: u32,
    pub oracle: Address,
}

/// Dynamic hedge configuration and position state for a specific currency pair.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HedgePosition {
    pub currency_pair: Symbol,        // e.g. EUR_USD, GBP_USD, JPY_USD
    pub base_rate: i128,              // Rate when hedge was locked
    pub current_rate: i128,           // Latest ingested oracle rate
    pub projected_annual_yield: i128, // Projected dividend yield in base currency
    pub hedge_ratio_bps: i128,        // Hedge ratio in basis points (10000 = 100%)
    pub variance_limit_bps: i128, // Max allowed fluctuation before rebalancing (e.g. 200 = 2.0%)
    pub hedged_amount: i128,      // Capital allocated in hedge
    pub last_rebalance_ledger: u32,
}

#[contracttype]
enum DataKey {
    Admin,
    Oracle,
    /// Legacy yield-based positions — preserved unchanged.
    Pair(Symbol),
    /// V2 engine: vaults contract implementing the `Vaults` interface.
    Vaults,
    /// V2 engine: keeper reward in basis points.
    KeeperFeeBps,
    /// V2 engine: monotonically increasing position id counter.
    NextPositionId,
    /// V2 engine: persistent per-position state.
    Position(u64),
}

#[contract]
pub struct FxHedgePool;

#[contractimpl]
impl FxHedgePool {
    /// Initialize the FX hedging pool with an admin and an authorized oracle address.
    pub fn initialize(env: Env, admin: Address, oracle: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Oracle, &oracle);
    }

    /// Set or update the authorized oracle feed provider.
    pub fn set_oracle(env: Env, caller: Address, new_oracle: Address) {
        caller.require_auth();
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));

        if caller != admin {
            panic_with_error!(env, Error::Unauthorized);
        }
        env.storage().instance().set(&DataKey::Oracle, &new_oracle);
    }

    /// Ingest FX rate oracle feeds (e.g., EUR/USD, GBP/USD, JPY/USD).
    /// Rates scaled by 10,000 (e.g. 10850 = 1.0850).
    pub fn ingest_oracle_rate(env: Env, caller: Address, currency_pair: Symbol, rate: i128) {
        caller.require_auth();
        let oracle: Address = env
            .storage()
            .instance()
            .get(&DataKey::Oracle)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));

        if caller != oracle {
            panic_with_error!(env, Error::Unauthorized);
        }
        if rate <= 0 {
            panic_with_error!(env, Error::InvalidRate);
        }

        let ledger = env.ledger().sequence();

        // If pair already exists, update its current_rate
        if let Some(mut pos) = env
            .storage()
            .instance()
            .get::<_, HedgePosition>(&DataKey::Pair(currency_pair.clone()))
        {
            pos.current_rate = rate;
            env.storage()
                .instance()
                .set(&DataKey::Pair(currency_pair.clone()), &pos);
        }

        #[allow(deprecated)]
        env.events()
            .publish((symbol_short!("rate_upd"), currency_pair), (rate, ledger));
    }

    /// Register or configure a currency forward hedge for an international real estate asset.
    /// Calculates the initial required forward hedge ratio based on projected annual dividend yield.
    pub fn setup_hedge(
        env: Env,
        caller: Address,
        currency_pair: Symbol,
        projected_annual_yield: i128,
        variance_limit_bps: i128,
        initial_rate: i128,
    ) -> HedgePosition {
        caller.require_auth();
        if projected_annual_yield <= 0 {
            panic_with_error!(env, Error::ZeroProjectedYield);
        }
        if variance_limit_bps <= 0 || variance_limit_bps > 10_000 {
            panic_with_error!(env, Error::InvalidVariance);
        }
        if initial_rate <= 0 {
            panic_with_error!(env, Error::InvalidRate);
        }

        let hedge_ratio_bps = Self::calculate_hedge_ratio(projected_annual_yield);
        let hedged_amount = (projected_annual_yield * hedge_ratio_bps) / 10_000;
        let ledger = env.ledger().sequence();

        let position = HedgePosition {
            currency_pair: currency_pair.clone(),
            base_rate: initial_rate,
            current_rate: initial_rate,
            projected_annual_yield,
            hedge_ratio_bps,
            variance_limit_bps,
            hedged_amount,
            last_rebalance_ledger: ledger,
        };

        env.storage()
            .instance()
            .set(&DataKey::Pair(currency_pair.clone()), &position);

        #[allow(deprecated)]
        env.events().publish(
            (symbol_short!("hedge_set"), currency_pair),
            (hedged_amount, hedge_ratio_bps, initial_rate),
        );

        position
    }

    /// Calculate required FX forward hedge ratio (in basis points) based on projected annual dividend yields.
    /// Model: Higher dividend yields require scaled coverage to insulate cashflow against downside FX swings.
    /// - Yield <= 5% (500 bps) -> 80% coverage (8000 bps)
    /// - Yield between 5% and 10% -> 90% coverage (9000 bps)
    /// - Yield > 10% -> 100% coverage (10000 bps)
    pub fn calculate_hedge_ratio(projected_annual_yield: i128) -> i128 {
        if projected_annual_yield <= 50_000 {
            8_000 // 80%
        } else if projected_annual_yield <= 100_000 {
            9_000 // 90%
        } else {
            10_000 // 100%
        }
    }

    /// Execute automated rebalancing swaps when currency fluctuations exceed variance limits.
    /// Returns the rebalanced swap amount and new hedged position.
    pub fn execute_rebalance(
        env: Env,
        caller: Address,
        currency_pair: Symbol,
    ) -> (i128, HedgePosition) {
        caller.require_auth();

        let mut pos: HedgePosition = env
            .storage()
            .instance()
            .get(&DataKey::Pair(currency_pair.clone()))
            .unwrap_or_else(|| panic_with_error!(env, Error::PairNotFound));

        // Calculate rate variance in basis points: |current_rate - base_rate| / base_rate * 10,000
        let diff = if pos.current_rate >= pos.base_rate {
            pos.current_rate - pos.base_rate
        } else {
            pos.base_rate - pos.current_rate
        };

        let variance_bps = (diff * 10_000) / pos.base_rate;

        if variance_bps < pos.variance_limit_bps {
            panic_with_error!(env, Error::VarianceWithinLimit);
        }

        // Calculate new required hedged amount reflecting the updated FX rate
        let new_hedge_ratio = Self::calculate_hedge_ratio(pos.projected_annual_yield);
        // Adjustment swap amount in base currency
        let target_hedged_amount = (pos.projected_annual_yield * new_hedge_ratio) / 10_000;
        let swap_rebalance_amount = (target_hedged_amount * pos.current_rate) / pos.base_rate;

        // Reset base rate to current rate and record rebalance
        pos.base_rate = pos.current_rate;
        pos.hedged_amount = target_hedged_amount;
        pos.hedge_ratio_bps = new_hedge_ratio;
        pos.last_rebalance_ledger = env.ledger().sequence();

        env.storage()
            .instance()
            .set(&DataKey::Pair(currency_pair.clone()), &pos);

        #[allow(deprecated)]
        env.events().publish(
            (symbol_short!("rebal_dex"), currency_pair),
            (swap_rebalance_amount, variance_bps, pos.current_rate),
        );

        (swap_rebalance_amount, pos)
    }

    /// Read hedge position for a given pair (legacy V1 storage).
    pub fn get_hedge_position(env: Env, currency_pair: Symbol) -> Option<HedgePosition> {
        env.storage().instance().get(&DataKey::Pair(currency_pair))
    }

    // ── V2 FX hedge position engine ─────────────────────────────────────────

    /// Optional V2 initialization: wires the vaults contract and keeper fee.
    ///
    /// Existing V1 deployments can call this once to upgrade in place. If the
    /// admin has already been set, a subsequent call by the same admin may
    /// rotate the oracle/vaults addresses or update the keeper fee.
    pub fn initialize_v2(
        env: Env,
        admin: Address,
        oracle: Address,
        vaults: Address,
        keeper_fee_bps: i128,
    ) {
        admin.require_auth();
        if let Some(current) = env.storage().instance().get::<_, Address>(&DataKey::Admin) {
            if current != admin {
                panic_with_error!(env, Error::Unauthorized);
            }
        } else {
            env.storage().instance().set(&DataKey::Admin, &admin);
        }
        if keeper_fee_bps < 0 || keeper_fee_bps > 10_000 {
            panic_with_error!(env, Error::InvalidVariance);
        }
        env.storage().instance().set(&DataKey::Oracle, &oracle);
        env.storage().instance().set(&DataKey::Vaults, &vaults);
        env.storage().instance().set(&DataKey::KeeperFeeBps, &keeper_fee_bps);

        #[allow(deprecated)]
        env.events().publish(
            (Symbol::new(&env, "init_v2"), admin),
            (oracle, vaults, keeper_fee_bps),
        );
    }

    /// Open a delta-hedged FX forward position.
    ///
    /// The owner must authenticate. The oracle is expected to implement
    /// `get_implied_vol(base: Symbol, quote: Symbol) -> i128` (bps) and
    /// `get_price(base: Symbol, quote: Symbol) -> i128` (10_000-scaled spot).
    /// 10% margin is locked in the vaults contract before state is written.
    pub fn open_hedge(
        env: Env,
        owner: Address,
        base_asset: Symbol,
        quote_currency: Symbol,
        notional: i128,
        duration_ledgers: u32,
    ) -> u64 {
        owner.require_auth();
        if notional <= 0 {
            panic_with_error!(env, Error::InvalidNotional);
        }
        if duration_ledgers == 0 {
            panic_with_error!(env, Error::InvalidDuration);
        }

        let oracle = Self::oracle_address(&env);
        let vaults = Self::vaults_address(&env);
        let implied_vol_bps = Self::read_implied_vol(&env, &oracle, &base_asset, &quote_currency);
        let entry_spot = Self::read_spot(&env, &oracle, &base_asset, &quote_currency);
        if entry_spot <= 0 {
            panic_with_error!(env, Error::InvalidRate);
        }

        // 10% margin. Keep the scaling identical to `notional` (vault base units).
        let collateral = match notional.checked_div(10) {
            Some(c) if c > 0 => c,
            _ => panic_with_error!(env, Error::InsufficientCollateral),
        };
        let keeper_fee_bps = env
            .storage()
            .instance()
            .get::<_, i128>(&DataKey::KeeperFeeBps)
            .unwrap_or(0);

        let id = env
            .storage()
            .persistent()
            .get::<_, u64>(&DataKey::NextPositionId)
            .unwrap_or(1);
        let next_id = id
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow));

        // Lock collateral first — if the vault call fails the whole frame reverts.
        let vaults_client = VaultsClient::new(&env, &vaults);
        if vaults_client
            .try_lock_collateral(&owner, &id, &collateral)
            .is_err()
        {
            panic_with_error!(env, Error::VaultCallFailed);
        }

        // At entry T = 100% of the configured duration (10_000 bps).
        let delta_bps = FxHedgePool::black_scholes_delta(entry_spot, entry_spot, implied_vol_bps, 10_000);
        let hedge_size = Self::hedge_size(notional, delta_bps, &env);
        let entry_ledger = env.ledger().sequence();

        let position = HedgePositionV2 {
            id,
            owner: owner.clone(),
            base_asset: base_asset.clone(),
            quote_currency: quote_currency.clone(),
            notional,
            collateral,
            strike: entry_spot,
            implied_vol_bps,
            duration_ledgers,
            entry_ledger,
            entry_spot,
            current_delta_bps: delta_bps,
            hedge_size,
            closed: false,
            keeper_fee_bps,
        };

        env.storage()
            .persistent()
            .set(&DataKey::NextPositionId, &next_id);
        Self::store_position(&env, &position);

        #[allow(deprecated)]
        env.events().publish(
            (Symbol::new(&env, "hedge_open"), owner),
            (id, notional, collateral),
        );
        id
    }

    /// Black-Scholes *call* delta in basis points (0..10_000).
    ///
    /// Pure, deterministic, integer-only function — no `Env` required, so it
    /// can be used off-chain (API, simulators) and in unit tests directly.
    ///
    /// * `spot`, `strike`: prices scaled by 10_000 (e.g. 10_850 = 1.0850).
    /// * `vol_bps`: annualised implied volatility in bps (1_500 = 15%).
    /// * `time_bps`: fraction of the option lifetime remaining in bps
    ///   (10_000 = full duration).
    ///
    /// `d1 = (ln(S/K) + 0.5*sigma^2*T) / (sigma*sqrt(T))`, delta = `N(d1)`.
    /// Risk-free rate is assumed zero (FX forward carry is ignored). All math
    /// is fixed-point with 1e9 scale: ln via range-reduced power series, sqrt
    /// via Newton's method, `N(x)` via Abramowitz & Stegun 7.1.26. Absolute
    /// precision is ~1e-7 in probability terms (~0.001 bp), far tighter than
    /// the 1 bp output granularity. Degenerate inputs (vol<=0, T<=0) fall back
    /// to the deep-ITM limit. Extreme intermediate values panic with
    /// `MathOverflow` rather than silently wrapping.
    pub fn black_scholes_delta(spot: i128, strike: i128, vol_bps: i128, time_bps: i128) -> i128 {
        if vol_bps <= 0 || time_bps <= 0 {
            return if spot >= strike { 10_000 } else { 0 };
        }
        if spot <= 0 {
            return 0;
        }
        if strike <= 0 {
            return 10_000;
        }

        let spot_fp = spot.checked_mul(FP).expect("MathOverflow");
        let ln_s_k = ln_fixed(spot_fp / strike);
        let vol = vol_bps.checked_mul(FP).expect("MathOverflow") / 10_000;
        let t = time_bps.checked_mul(FP).expect("MathOverflow") / 10_000;
        let vol_sq = vol.checked_mul(vol).expect("MathOverflow") / FP;
        let drift = vol_sq.checked_mul(t).expect("MathOverflow") / FP / 2;
        let denom = vol.checked_mul(sqrt_fixed(t)).expect("MathOverflow") / FP;
        if denom <= 0 {
            return if spot >= strike { 10_000 } else { 0 };
        }
        let d1 = ln_s_k
            .checked_add(drift)
            .expect("MathOverflow")
            .checked_mul(FP)
            .expect("MathOverflow")
            / denom;
        let cdf = if d1 >= 0 {
            norm_cdf_pos_fixed(d1)
        } else {
            FP - norm_cdf_pos_fixed(-d1)
        };
        let delta = cdf.checked_mul(10_000).expect("MathOverflow") / FP;
        if delta < 0 {
            0
        } else if delta > 10_000 {
            10_000
        } else {
            delta
        }
    }

    /// Recompute and persist the delta/hedge size for an open position from a
    /// fresh spot price. Permissionless: anyone may call (typically the oracle
    /// keeper) — the new spot is supplied by the caller and is expected to be
    /// sourced from the configured oracle.
    pub fn update_delta(env: Env, position_id: u64, new_spot: i128) {
        let mut position = Self::load_position(&env, position_id);
        if position.closed {
            panic_with_error!(env, Error::HedgeClosed);
        }
        if new_spot <= 0 {
            panic_with_error!(env, Error::InvalidRate);
        }
        let time_bps = Self::remaining_time_bps(&env, &position);
        let delta_bps =
            FxHedgePool::black_scholes_delta(new_spot, position.strike, position.implied_vol_bps, time_bps);
        position.current_delta_bps = delta_bps;
        position.hedge_size = Self::hedge_size(position.notional, delta_bps, &env);
        Self::store_position(&env, &position);

        #[allow(deprecated)]
        env.events().publish(
            (Symbol::new(&env, "delta_upd"), position_id),
            (delta_bps, position.hedge_size),
        );
    }

    /// Rebalance the hedge to the target delta, paying the caller a keeper fee.
    ///
    /// Any address may call; `keeper.require_auth()` is enforced. The fee is
    /// `max(1, abs(adjust) * keeper_fee_bps / 10_000)`, debited from collateral
    /// via `Vaults::settle_pnl` with a negative amount.
    pub fn rebalance_hedge(env: Env, position_id: u64, keeper: Address) -> i128 {
        keeper.require_auth();
        let mut position = Self::load_position(&env, position_id);
        if position.closed {
            panic_with_error!(env, Error::HedgeClosed);
        }

        let oracle = Self::oracle_address(&env);
        let spot = Self::read_spot(&env, &oracle, &position.base_asset, &position.quote_currency);
        let time_bps = Self::remaining_time_bps(&env, &position);
        let target_delta = FxHedgePool::black_scholes_delta(
            spot,
            position.strike,
            position.implied_vol_bps,
            time_bps,
        );
        let target_size = Self::hedge_size(position.notional, target_delta, &env);
        let adjust = target_size
            .checked_sub(position.hedge_size)
            .unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow));

        let vaults = Self::vaults_address(&env);
        let vaults_client = VaultsClient::new(&env, &vaults);
        if adjust > 0 {
            if vaults_client
                .try_lock_collateral(&position.owner, &position_id, &adjust)
                .is_err()
            {
                panic_with_error!(env, Error::VaultCallFailed);
            }
        } else if adjust < 0 {
            let release = adjust.checked_neg().unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow));
            if vaults_client
                .try_release_collateral(&position_id, &release)
                .is_err()
            {
                panic_with_error!(env, Error::VaultCallFailed);
            }
        }

        let magnitude = if adjust < 0 {
            adjust.checked_neg().unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow))
        } else {
            adjust
        };
        let mut fee = magnitude
            .checked_mul(position.keeper_fee_bps)
            .unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow))
            / 10_000;
        if fee < 1 {
            fee = 1;
        }
        if fee > position.collateral {
            panic_with_error!(env, Error::InsufficientCollateral);
        }
        if vaults_client
            .try_settle_pnl(&position_id, &fee.checked_neg().unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow)))
            .is_err()
        {
            panic_with_error!(env, Error::VaultCallFailed);
        }

        position.collateral -= fee;
        position.hedge_size = target_size;
        position.current_delta_bps = target_delta;
        Self::store_position(&env, &position);

        #[allow(deprecated)]
        env.events().publish(
            (Symbol::new(&env, "rebalanced"), position_id),
            (adjust, fee, target_delta),
        );
        fee
    }

    /// Close a hedge, settle realised PnL and release the remaining collateral.
    ///
    /// Only the position owner or the pool admin may close. The PnL model is
    /// deterministic: for a portfolio short the FX exposure, the hedge is long
    /// FX, so `pnl = (spot - entry_spot) * hedge_size / entry_spot`. A 30%
    /// adverse move is therefore offset by `0.30 * delta` of the notional.
    pub fn close_hedge(env: Env, position_id: u64, caller: Address) {
        caller.require_auth();
        let mut position = Self::load_position(&env, position_id);
        if position.closed {
            panic_with_error!(env, Error::HedgeClosed);
        }
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if caller != position.owner && caller != admin {
            panic_with_error!(env, Error::Unauthorized);
        }
        if position.entry_spot <= 0 {
            panic_with_error!(env, Error::InvalidRate);
        }

        let oracle = Self::oracle_address(&env);
        let spot = Self::read_spot(&env, &oracle, &position.base_asset, &position.quote_currency);
        let pnl = spot
            .checked_sub(position.entry_spot)
            .unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow))
            .checked_mul(position.hedge_size)
            .unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow))
            / position.entry_spot;

        let vaults = Self::vaults_address(&env);
        let vaults_client = VaultsClient::new(&env, &vaults);
        if vaults_client
            .try_settle_pnl(&position_id, &pnl)
            .is_err()
        {
            panic_with_error!(env, Error::VaultCallFailed);
        }
        if vaults_client
            .try_release_collateral(&position_id, &position.collateral)
            .is_err()
        {
            panic_with_error!(env, Error::VaultCallFailed);
        }

        position.closed = true;
        Self::store_position(&env, &position);

        #[allow(deprecated)]
        env.events().publish(
            (Symbol::new(&env, "hedge_close"), position_id),
            (pnl, caller),
        );
    }

    /// Read a V2 position by id, or `None` if it does not exist.
    pub fn get_hedge_position_v2(env: Env, id: u64) -> Option<HedgePositionV2> {
        env.storage().persistent().get(&DataKey::Position(id))
    }

    /// Read the latest delta (bps) of a V2 position.
    pub fn get_delta_bps(env: Env, id: u64) -> i128 {
        Self::load_position(&env, id).current_delta_bps
    }

    // ── V2 internal helpers (not contract entry points) ────────────────────

    fn oracle_address(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Oracle)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    fn vaults_address(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Vaults)
            .unwrap_or_else(|| panic_with_error!(env, Error::VaultCallFailed))
    }

    fn load_position(env: &Env, id: u64) -> HedgePositionV2 {
        env.storage()
            .persistent()
            .get(&DataKey::Position(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::PositionNotFound))
    }

    fn store_position(env: &Env, position: &HedgePositionV2) {
        env.storage()
            .persistent()
            .set(&DataKey::Position(position.id), position);
    }

    fn hedge_size(notional: i128, delta_bps: i128, env: &Env) -> i128 {
        if delta_bps <= 0 {
            return 0;
        }
        notional
            .checked_mul(delta_bps)
            .unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow))
            / 10_000
    }

    /// Fraction of the original duration remaining, in bps (clamped to 10_000).
    fn remaining_time_bps(env: &Env, position: &HedgePositionV2) -> i128 {
        let end = position
            .entry_ledger
            .checked_add(position.duration_ledgers)
            .unwrap_or(u32::MAX);
        let remaining = end.saturating_sub(env.ledger().sequence());
        let bps = (i128::from(remaining) * 10_000) / i128::from(position.duration_ledgers);
        if bps > 10_000 { 10_000 } else { bps }
    }

    /// Oracle contract must implement `get_implied_vol(base, quote) -> i128` bps.
    fn read_implied_vol(env: &Env, oracle: &Address, base: &Symbol, quote: &Symbol) -> i128 {
        let args = (base.clone(), quote.clone()).into_val(env);
        match env.try_invoke_contract::<i128, soroban_sdk::Error>(
            oracle,
            &Symbol::new(env, "get_implied_vol"),
            args,
        ) {
            Ok(Ok(v)) => v,
            _ => panic_with_error!(env, Error::InvalidRate),
        }
    }

    /// Oracle contract must implement `get_price(base, quote) -> i128`, a
    /// 10_000-scaled spot quote (e.g. 10_850 = 1.0850 USD per EUR).
    fn read_spot(env: &Env, oracle: &Address, base: &Symbol, quote: &Symbol) -> i128 {
        let args = (base.clone(), quote.clone()).into_val(env);
        match env.try_invoke_contract::<i128, soroban_sdk::Error>(
            oracle,
            &Symbol::new(env, "get_price"),
            args,
        ) {
            Ok(Ok(v)) => v,
            _ => panic_with_error!(env, Error::InvalidRate),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{symbol_short, testutils::Address as _, Env};

    #[test]
    fn test_fx_hedging_pool_lifecycle_and_automated_rebalance() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(FxHedgePool, ());
        let client = FxHedgePoolClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);

        // 1. Initialize
        client.initialize(&admin, &oracle);

        // 2. Setup EUR/USD forward hedge:
        // Projected yield: 100,000 EUR
        // Variance limit: 200 bps (2.0%)
        // Initial EUR/USD rate: 1.0800 (10800)
        let pair = symbol_short!("EUR_USD");
        let pos = client.setup_hedge(
            &admin, &pair, &100_000, &200, // 2% variance limit
            &10_800,
        );

        assert_eq!(pos.hedge_ratio_bps, 9_000); // 90%
        assert_eq!(pos.hedged_amount, 90_000);
        assert_eq!(pos.base_rate, 10_800);

        // 3. Oracle updates rate within variance limit (e.g. 1.0850 -> ~0.46% change)
        client.ingest_oracle_rate(&oracle, &pair, &10_850);
        let updated_pos = client.get_hedge_position(&pair).unwrap();
        assert_eq!(updated_pos.current_rate, 10_850);

        // Rebalance should fail if within limit
        // (will panic with VarianceWithinLimit)

        // 4. Oracle updates rate with high fluctuation exceeding 2% limit (e.g. 1.1200 -> 3.7% change)
        client.ingest_oracle_rate(&oracle, &pair, &11_200);

        // 5. Execute automated rebalancing swap
        let (swap_amount, rebalanced_pos) = client.execute_rebalance(&admin, &pair);
        assert!(swap_amount > 0);
        assert_eq!(rebalanced_pos.base_rate, 11_200);
        assert_eq!(rebalanced_pos.current_rate, 11_200);

        // Verify GBP/USD and JPY/USD pairs can also be ingested
        let gbp_pair = symbol_short!("GBP_USD");
        client.setup_hedge(&admin, &gbp_pair, &50_000, &150, &12_500);
        client.ingest_oracle_rate(&oracle, &gbp_pair, &12_800);
        let gbp_pos = client.get_hedge_position(&gbp_pair).unwrap();
        assert_eq!(gbp_pos.current_rate, 12_800);
        assert_eq!(gbp_pos.hedge_ratio_bps, 8_000); // 80% for <= 50,000
    }

    // ── V2 test doubles ───────────────────────────────────────────────────

    /// Minimal oracle implementing the two methods the V2 engine documents.
    #[contract]
    struct MockOracle;

    #[contractimpl]
    impl MockOracle {
        pub fn set(env: Env, price: i128, vol: i128) {
            env.storage().instance().set(&symbol_short!("px"), &price);
            env.storage().instance().set(&symbol_short!("iv"), &vol);
        }

        pub fn get_price(env: Env, _base: Symbol, _quote: Symbol) -> i128 {
            env.storage()
                .instance()
                .get(&symbol_short!("px"))
                .unwrap_or(10_000)
        }

        pub fn get_implied_vol(env: Env, _base: Symbol, _quote: Symbol) -> i128 {
            env.storage()
                .instance()
                .get(&symbol_short!("iv"))
                .unwrap_or(1_500)
        }
    }

    /// Mock of the required vaults interface; accumulates call amounts.
    #[contract]
    struct MockVaults;

    #[contractimpl]
    impl MockVaults {
        pub fn lock_collateral(env: Env, _owner: Address, _position_id: u64, amount: i128) {
            let cur: i128 = env
                .storage()
                .instance()
                .get(&symbol_short!("lockd"))
                .unwrap_or(0);
            env.storage()
                .instance()
                .set(&symbol_short!("lockd"), &(cur + amount));
        }

        pub fn release_collateral(env: Env, _position_id: u64, amount: i128) {
            let cur: i128 = env
                .storage()
                .instance()
                .get(&symbol_short!("reld"))
                .unwrap_or(0);
            env.storage()
                .instance()
                .set(&symbol_short!("reld"), &(cur + amount));
        }

        pub fn settle_pnl(env: Env, _position_id: u64, pnl: i128) {
            let cur: i128 = env
                .storage()
                .instance()
                .get(&symbol_short!("setl"))
                .unwrap_or(0);
            env.storage()
                .instance()
                .set(&symbol_short!("setl"), &(cur + pnl));
        }

        pub fn totals(env: Env) -> (i128, i128, i128) {
            (
                env.storage()
                    .instance()
                    .get(&symbol_short!("lockd"))
                    .unwrap_or(0),
                env.storage()
                    .instance()
                    .get(&symbol_short!("reld"))
                    .unwrap_or(0),
                env.storage()
                    .instance()
                    .get(&symbol_short!("setl"))
                    .unwrap_or(0),
            )
        }
    }

    // ── V2 Black-Scholes tests ────────────────────────────────────────────

    #[test]
    fn black_scholes_delta_is_monotonic_and_bounded() {
        let strike = 10_000;
        let mut previous = 0i128;
        for spot in (8_000..=13_000).step_by(250) {
            let delta = FxHedgePool::black_scholes_delta(spot, strike, 1_500, 10_000);
            assert!(
                delta >= previous,
                "delta decreased as spot rose from {spot}: {delta} < {previous}"
            );
            assert!((0..=10_000).contains(&delta), "delta out of range: {delta}");
            previous = delta;
        }
        // Deep OTM / deep ITM limits.
        assert!(FxHedgePool::black_scholes_delta(8_000, strike, 1_500, 10_000) < 1_000);
        assert!(previous > 9_000);
    }

    #[test]
    fn black_scholes_delta_atm_is_near_half() {
        // With negligible vol/time the ATM call delta is essentially 0.5.
        let tiny = FxHedgePool::black_scholes_delta(10_000, 10_000, 100, 100);
        assert!((4_990..=5_010).contains(&tiny), "ATM tiny delta = {tiny}");
        // Moderate vol (10%) and 3 months to expiry: d1 = 0.025, N(d1) ~ 0.5100.
        let moderate = FxHedgePool::black_scholes_delta(10_000, 10_000, 1_000, 2_500);
        assert!((5_000..=5_300).contains(&moderate), "ATM moderate delta = {moderate}");
    }

    #[test]
    fn black_scholes_delta_degenerate_limits() {
        // Zero vol or zero time collapses to the intrinsic (deep-ITM) value.
        assert_eq!(FxHedgePool::black_scholes_delta(11_000, 10_000, 0, 10_000), 10_000);
        assert_eq!(FxHedgePool::black_scholes_delta(9_000, 10_000, 0, 10_000), 0);
        assert_eq!(FxHedgePool::black_scholes_delta(11_000, 10_000, 1_500, 0), 10_000);
        assert_eq!(FxHedgePool::black_scholes_delta(9_000, 10_000, 1_500, 0), 0);
    }

    // ── V2 lifecycle test ─────────────────────────────────────────────────

    #[test]
    fn v2_open_rebalance_and_close_lifecycle() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let keeper = Address::generate(&env);
        let base = symbol_short!("EUR");
        let quote = symbol_short!("USD");

        let oracle_id = env.register(MockOracle, ());
        let oracle = MockOracleClient::new(&env, &oracle_id);
        oracle.set(&10_000, &1_500); // spot 1.0000, vol 15%

        let vaults_id = env.register(MockVaults, ());
        let vaults = MockVaultsClient::new(&env, &vaults_id);

        let pool_id = env.register(FxHedgePool, ());
        let client = FxHedgePoolClient::new(&env, &pool_id);
        client.initialize(&admin, &oracle_id);
        client.initialize_v2(&admin, &oracle_id, &vaults_id, &10);

        // Open: 10% collateral, strike == entry spot, delta from Black-Scholes.
        let id = client.open_hedge(&owner, &base, &quote, &1_000_000, &1_000);
        assert_eq!(id, 1);
        let opened = client.get_hedge_position_v2(&id).unwrap();
        assert_eq!(opened.owner, owner);
        assert_eq!(opened.collateral, 100_000);
        assert_eq!(opened.strike, 10_000);
        assert_eq!(opened.entry_spot, 10_000);
        assert!(!opened.closed);
        assert_eq!(opened.hedge_size, 1_000_000 * opened.current_delta_bps / 10_000);
        assert_eq!(client.get_delta_bps(&id), opened.current_delta_bps);

        // Owner can update delta permissionlessly with a fresh spot.
        client.update_delta(&id, &10_500);
        let updated = client.get_hedge_position_v2(&id).unwrap();
        assert!(updated.hedge_size > opened.hedge_size);

        // Move the oracle spot and rebalance by an unrelated keeper (fee paid).
        oracle.set(&11_000, &1_500);
        let fee = client.rebalance_hedge(&id, &keeper);
        assert!(fee >= 1);
        let rebalanced = client.get_hedge_position_v2(&id).unwrap();
        assert_eq!(rebalanced.collateral, opened.collateral - fee);
        assert!(rebalanced.hedge_size > opened.hedge_size);

        // Unauthorized third party cannot close.
        let intruder = Address::generate(&env);
        assert!(client.try_close_hedge(&id, &intruder).is_err());

        // Owner closes; PnL settled and collateral released via the vaults API.
        client.close_hedge(&id, &owner);
        let closed = client.get_hedge_position_v2(&id).unwrap();
        assert!(closed.closed);
        let (locked, released, settled) = vaults.totals();
        assert!(locked >= released, "released more collateral than locked");
        assert!(settled != 0, "expected PnL/fee settlement");
    }
}
