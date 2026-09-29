//! Synthetic RWA index token (issue #93).
//!
//! An index token (e.g. `RWA-Top10`) is backed 1:1 by a basket of tokenized
//! real-world assets held by this contract.
//!
//! * **Mint / redeem.** Index tokens are minted only by depositing the exact
//!   basket ratio of every component, and redeemed by burning index tokens for
//!   the pro-rata share of every component reserve. Before the first mint the
//!   ratio is the configured `units` per whole index token; afterwards it is
//!   the live reserve ratio, so the basket stays fully backed.
//! * **Composite price.** [`IndexToken::index_price`] values one whole index
//!   token as `Σ units_per_index_i × price_i / 10^decimals_i` using a SEP-40
//!   style price oracle, rejecting stale feeds.
//! * **Streaming management fee.** An annual fee in basis points accrues
//!   continuously, per second, by moving the fee share of every component
//!   reserve into an owed bucket the index maintainer can claim at any time.
//!   Holders are diluted in underlying terms, never in index-token supply.

#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, panic_with_error,
    symbol_short, token, Address, Env, String, Symbol, Vec,
};

/// Base units in one whole index token (7 decimals, Stellar convention).
pub const INDEX_UNIT: i128 = 10_000_000;
pub const INDEX_DECIMALS: u32 = 7;
pub const BPS_DENOMINATOR: i128 = 10_000;
/// Seconds per 365-day year, the fee accrual period.
pub const SECONDS_PER_YEAR: i128 = 31_536_000;
/// Hard cap on the annual management fee: 5%.
pub const MAX_FEE_BPS: u32 = 500;
pub const MAX_COMPONENTS: u32 = 20;

const INSTANCE_BUMP: u32 = 518_400;
const INSTANCE_THRESHOLD: u32 = 17_280;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum IndexError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    InvalidBasket = 3,
    InvalidAmount = 4,
    InsufficientBalance = 5,
    InsufficientAllowance = 6,
    FeeTooHigh = 7,
    Overflow = 8,
    PriceUnavailable = 9,
    StalePrice = 10,
    EmptyIndex = 11,
}

/// One basket component: `units` base units of `token` back one whole index
/// token at launch.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Component {
    pub token: Address,
    pub units: i128,
    pub decimals: u32,
}

/// SEP-40 asset identifier.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Asset {
    Stellar(Address),
    Other(Symbol),
}

/// SEP-40 price record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceData {
    pub price: i128,
    pub timestamp: u64,
}

/// Minimal SEP-40 price-feed interface.
#[contractclient(name = "PriceOracleClient")]
pub trait PriceOracle {
    fn lastprice(env: Env, asset: Asset) -> Option<PriceData>;
    fn decimals(env: Env) -> u32;
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowanceValue {
    pub amount: i128,
    pub expiration_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexConfig {
    pub admin: Address,
    pub maintainer: Address,
    pub oracle: Address,
    pub fee_bps: u32,
    pub max_price_age: u64,
    pub name: String,
    pub symbol: String,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
enum DataKey {
    Config,
    Basket,
    TotalSupply,
    LastAccrual,
    Reserve(Address),
    FeesOwed(Address),
    Balance(Address),
    Allowance(Address, Address),
}

#[contract]
pub struct IndexToken;

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_THRESHOLD, INSTANCE_BUMP);
}

fn config(env: &Env) -> IndexConfig {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, IndexError::NotInitialized))
}

fn basket(env: &Env) -> Vec<Component> {
    env.storage()
        .instance()
        .get(&DataKey::Basket)
        .unwrap_or_else(|| panic_with_error!(env, IndexError::NotInitialized))
}

fn supply(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalSupply)
        .unwrap_or(0)
}

fn reserve(env: &Env, token: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::Reserve(token.clone()))
        .unwrap_or(0)
}

fn fees_owed(env: &Env, token: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::FeesOwed(token.clone()))
        .unwrap_or(0)
}

fn balance_of(env: &Env, id: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::Balance(id.clone()))
        .unwrap_or(0)
}

fn set_balance(env: &Env, id: &Address, amount: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::Balance(id.clone()), &amount);
}

fn mul_div_floor(env: &Env, a: i128, b: i128, denominator: i128) -> i128 {
    a.checked_mul(b)
        .and_then(|product| product.checked_div(denominator))
        .unwrap_or_else(|| panic_with_error!(env, IndexError::Overflow))
}

fn mul_div_ceil(env: &Env, a: i128, b: i128, denominator: i128) -> i128 {
    let product = a
        .checked_mul(b)
        .unwrap_or_else(|| panic_with_error!(env, IndexError::Overflow));
    let quotient = product / denominator;
    if product % denominator == 0 {
        quotient
    } else {
        quotient + 1
    }
}

fn pow10(env: &Env, exponent: u32) -> i128 {
    10i128
        .checked_pow(exponent)
        .unwrap_or_else(|| panic_with_error!(env, IndexError::Overflow))
}

/// Management fee owed on `reserve` for `elapsed` seconds at `fee_bps`/year.
fn streamed_fee(env: &Env, reserve: i128, fee_bps: u32, elapsed: u64) -> i128 {
    if reserve <= 0 || fee_bps == 0 || elapsed == 0 {
        return 0;
    }
    let rate_time = (fee_bps as i128)
        .checked_mul(elapsed as i128)
        .unwrap_or_else(|| panic_with_error!(env, IndexError::Overflow));
    let fee = mul_div_floor(env, reserve, rate_time, BPS_DENOMINATOR * SECONDS_PER_YEAR);
    // A fee can never drain more than the reserve itself.
    fee.min(reserve)
}

/// Moves every fee accrued since the last accrual from the reserves into
/// the maintainer's owed bucket.
fn accrue(env: &Env) {
    let now = env.ledger().timestamp();
    let last: u64 = env
        .storage()
        .instance()
        .get(&DataKey::LastAccrual)
        .unwrap_or(now);
    let elapsed = now.saturating_sub(last);
    if elapsed > 0 && supply(env) > 0 {
        let cfg = config(env);
        for component in basket(env).iter() {
            let held = reserve(env, &component.token);
            let fee = streamed_fee(env, held, cfg.fee_bps, elapsed);
            if fee > 0 {
                env.storage()
                    .persistent()
                    .set(&DataKey::Reserve(component.token.clone()), &(held - fee));
                env.storage().persistent().set(
                    &DataKey::FeesOwed(component.token.clone()),
                    &(fees_owed(env, &component.token) + fee),
                );
            }
        }
    }
    env.storage().instance().set(&DataKey::LastAccrual, &now);
}

/// Reserves after fees that have streamed but not yet been accrued.
fn live_reserves(env: &Env) -> Vec<i128> {
    let cfg = config(env);
    let now = env.ledger().timestamp();
    let last: u64 = env
        .storage()
        .instance()
        .get(&DataKey::LastAccrual)
        .unwrap_or(now);
    let elapsed = if supply(env) > 0 { now.saturating_sub(last) } else { 0 };
    let mut out = Vec::new(env);
    for component in basket(env).iter() {
        let held = reserve(env, &component.token);
        out.push_back(held - streamed_fee(env, held, cfg.fee_bps, elapsed));
    }
    out
}

fn validate_basket(env: &Env, components: &Vec<Component>) {
    let count = components.len();
    if count == 0 || count > MAX_COMPONENTS {
        panic_with_error!(env, IndexError::InvalidBasket);
    }
    for i in 0..count {
        let component = components.get_unchecked(i);
        if component.units <= 0 || component.decimals > 18 {
            panic_with_error!(env, IndexError::InvalidBasket);
        }
        for j in (i + 1)..count {
            if components.get_unchecked(j).token == component.token {
                panic_with_error!(env, IndexError::InvalidBasket);
            }
        }
    }
}

fn spend_allowance(env: &Env, from: &Address, spender: &Address, amount: i128) {
    let key = DataKey::Allowance(from.clone(), spender.clone());
    let current: AllowanceValue = env.storage().temporary().get(&key).unwrap_or(AllowanceValue {
        amount: 0,
        expiration_ledger: 0,
    });
    let available = if current.expiration_ledger < env.ledger().sequence() {
        0
    } else {
        current.amount
    };
    if available < amount {
        panic_with_error!(env, IndexError::InsufficientAllowance);
    }
    env.storage().temporary().set(
        &key,
        &AllowanceValue {
            amount: available - amount,
            expiration_ledger: current.expiration_ledger,
        },
    );
}

fn move_balance(env: &Env, from: &Address, to: &Address, amount: i128) {
    if amount < 0 {
        panic_with_error!(env, IndexError::InvalidAmount);
    }
    let from_balance = balance_of(env, from);
    if from_balance < amount {
        panic_with_error!(env, IndexError::InsufficientBalance);
    }
    set_balance(env, from, from_balance - amount);
    set_balance(env, to, balance_of(env, to) + amount);
}

#[contractimpl]
impl IndexToken {
    /// Configures the index. `fee_bps` is the annual management fee and
    /// `max_price_age` the oldest oracle price (seconds) accepted for NAV.
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        env: Env,
        admin: Address,
        maintainer: Address,
        oracle: Address,
        components: Vec<Component>,
        fee_bps: u32,
        max_price_age: u64,
        name: String,
        symbol: String,
    ) {
        if env.storage().instance().has(&DataKey::Config) {
            panic_with_error!(&env, IndexError::AlreadyInitialized);
        }
        admin.require_auth();
        if fee_bps > MAX_FEE_BPS {
            panic_with_error!(&env, IndexError::FeeTooHigh);
        }
        validate_basket(&env, &components);
        env.storage().instance().set(
            &DataKey::Config,
            &IndexConfig {
                admin,
                maintainer,
                oracle,
                fee_bps,
                max_price_age,
                name,
                symbol,
            },
        );
        env.storage().instance().set(&DataKey::Basket, &components);
        env.storage().instance().set(&DataKey::TotalSupply, &0i128);
        env.storage()
            .instance()
            .set(&DataKey::LastAccrual, &env.ledger().timestamp());
        bump(&env);
    }

    // ── Basket mint / redeem ───────────────────────────────────────────────

    /// Underlying amounts (basket order) that must be deposited to mint
    /// `amount` index base units right now. Rounded up in the index's favour.
    pub fn quote_mint(env: Env, amount: i128) -> Vec<i128> {
        if amount <= 0 {
            panic_with_error!(&env, IndexError::InvalidAmount);
        }
        let total = supply(&env);
        let reserves = live_reserves(&env);
        let mut out = Vec::new(&env);
        for (i, component) in basket(&env).iter().enumerate() {
            let required = if total == 0 {
                mul_div_ceil(&env, component.units, amount, INDEX_UNIT)
            } else {
                mul_div_ceil(&env, reserves.get_unchecked(i as u32), amount, total)
            };
            out.push_back(required);
        }
        out
    }

    /// Underlying amounts (basket order) returned for burning `amount` index
    /// base units right now. Rounded down in the index's favour.
    pub fn quote_redeem(env: Env, amount: i128) -> Vec<i128> {
        let total = supply(&env);
        if amount <= 0 || amount > total {
            panic_with_error!(&env, IndexError::InvalidAmount);
        }
        let reserves = live_reserves(&env);
        let mut out = Vec::new(&env);
        for held in reserves.iter() {
            out.push_back(mul_div_floor(&env, held, amount, total));
        }
        out
    }

    /// Mints `amount` index base units to `to` against a deposit of the
    /// exact basket ratio of every component. Returns the deposits made.
    pub fn mint(env: Env, to: Address, amount: i128) -> Vec<i128> {
        to.require_auth();
        accrue(&env);
        let deposits = Self::quote_mint(env.clone(), amount);
        let this = env.current_contract_address();
        for (i, component) in basket(&env).iter().enumerate() {
            let deposit = deposits.get_unchecked(i as u32);
            if deposit <= 0 {
                panic_with_error!(&env, IndexError::InvalidAmount);
            }
            token::Client::new(&env, &component.token).transfer(&to, &this, &deposit);
            env.storage().persistent().set(
                &DataKey::Reserve(component.token.clone()),
                &(reserve(&env, &component.token) + deposit),
            );
        }
        set_balance(&env, &to, balance_of(&env, &to) + amount);
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &(supply(&env) + amount));
        bump(&env);
        #[allow(deprecated)]
        env.events()
            .publish((symbol_short!("mint"), to), (amount, deposits.clone()));
        deposits
    }

    /// Burns `amount` index base units from `from` and returns the pro-rata
    /// share of every component reserve. Returns the withdrawals made.
    pub fn redeem(env: Env, from: Address, amount: i128) -> Vec<i128> {
        from.require_auth();
        accrue(&env);
        let held = balance_of(&env, &from);
        if held < amount {
            panic_with_error!(&env, IndexError::InsufficientBalance);
        }
        let withdrawals = Self::quote_redeem(env.clone(), amount);
        set_balance(&env, &from, held - amount);
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &(supply(&env) - amount));
        let this = env.current_contract_address();
        for (i, component) in basket(&env).iter().enumerate() {
            let out = withdrawals.get_unchecked(i as u32);
            env.storage().persistent().set(
                &DataKey::Reserve(component.token.clone()),
                &(reserve(&env, &component.token) - out),
            );
            if out > 0 {
                token::Client::new(&env, &component.token).transfer(&this, &from, &out);
            }
        }
        bump(&env);
        #[allow(deprecated)]
        env.events()
            .publish((symbol_short!("redeem"), from), (amount, withdrawals.clone()));
        withdrawals
    }

    // ── Oracle valuation ───────────────────────────────────────────────────

    /// Composite price of one whole index token, in oracle quote units with
    /// the oracle's decimals. Uses the live (post-fee) basket composition.
    pub fn index_price(env: Env) -> i128 {
        let cfg = config(&env);
        let oracle = PriceOracleClient::new(&env, &cfg.oracle);
        let now = env.ledger().timestamp();
        let total = supply(&env);
        let reserves = live_reserves(&env);
        let mut price: i128 = 0;
        for (i, component) in basket(&env).iter().enumerate() {
            let quote = oracle
                .lastprice(&Asset::Stellar(component.token.clone()))
                .unwrap_or_else(|| panic_with_error!(&env, IndexError::PriceUnavailable));
            if quote.price <= 0 {
                panic_with_error!(&env, IndexError::PriceUnavailable);
            }
            if now.saturating_sub(quote.timestamp) > cfg.max_price_age {
                panic_with_error!(&env, IndexError::StalePrice);
            }
            let units_per_index = if total == 0 {
                component.units
            } else {
                mul_div_floor(&env, reserves.get_unchecked(i as u32), INDEX_UNIT, total)
            };
            let value = mul_div_floor(
                &env,
                units_per_index,
                quote.price,
                pow10(&env, component.decimals),
            );
            price = price
                .checked_add(value)
                .unwrap_or_else(|| panic_with_error!(&env, IndexError::Overflow));
        }
        price
    }

    /// Net asset value of the whole index (all outstanding tokens), in
    /// oracle quote units.
    pub fn nav(env: Env) -> i128 {
        let total = supply(&env);
        if total == 0 {
            return 0;
        }
        let price = Self::index_price(env.clone());
        mul_div_floor(&env, price, total, INDEX_UNIT)
    }

    pub fn price_decimals(env: Env) -> u32 {
        PriceOracleClient::new(&env, &config(&env).oracle).decimals()
    }

    // ── Streaming management fee ───────────────────────────────────────────

    /// Fees owed to the maintainer, per component, including the portion
    /// streamed since the last accrual.
    pub fn accrued_fees(env: Env) -> Vec<i128> {
        let reserves = live_reserves(&env);
        let mut out = Vec::new(&env);
        for (i, component) in basket(&env).iter().enumerate() {
            let pending = reserve(&env, &component.token) - reserves.get_unchecked(i as u32);
            out.push_back(fees_owed(&env, &component.token) + pending);
        }
        out
    }

    /// Accrues fees and transfers everything owed to the maintainer.
    pub fn claim_fees(env: Env) -> Vec<i128> {
        let cfg = config(&env);
        cfg.maintainer.require_auth();
        accrue(&env);
        let this = env.current_contract_address();
        let mut out = Vec::new(&env);
        for component in basket(&env).iter() {
            let owed = fees_owed(&env, &component.token);
            if owed > 0 {
                env.storage()
                    .persistent()
                    .set(&DataKey::FeesOwed(component.token.clone()), &0i128);
                token::Client::new(&env, &component.token).transfer(
                    &this,
                    &cfg.maintainer,
                    &owed,
                );
            }
            out.push_back(owed);
        }
        bump(&env);
        #[allow(deprecated)]
        env.events()
            .publish((symbol_short!("fees"), cfg.maintainer), out.clone());
        out
    }

    /// Changes the annual fee. Fees streamed so far settle at the old rate.
    pub fn set_fee_bps(env: Env, fee_bps: u32) {
        let mut cfg = config(&env);
        cfg.admin.require_auth();
        if fee_bps > MAX_FEE_BPS {
            panic_with_error!(&env, IndexError::FeeTooHigh);
        }
        accrue(&env);
        cfg.fee_bps = fee_bps;
        env.storage().instance().set(&DataKey::Config, &cfg);
    }

    pub fn set_maintainer(env: Env, maintainer: Address) {
        let mut cfg = config(&env);
        cfg.admin.require_auth();
        accrue(&env);
        cfg.maintainer = maintainer;
        env.storage().instance().set(&DataKey::Config, &cfg);
    }

    pub fn set_oracle(env: Env, oracle: Address, max_price_age: u64) {
        let mut cfg = config(&env);
        cfg.admin.require_auth();
        cfg.oracle = oracle;
        cfg.max_price_age = max_price_age;
        env.storage().instance().set(&DataKey::Config, &cfg);
    }

    // ── Views ──────────────────────────────────────────────────────────────

    pub fn config(env: Env) -> IndexConfig {
        config(&env)
    }

    pub fn basket(env: Env) -> Vec<Component> {
        basket(&env)
    }

    /// Component reserves backing the index, net of streamed fees.
    pub fn reserves(env: Env) -> Vec<i128> {
        live_reserves(&env)
    }

    // ── SEP-41 token interface ─────────────────────────────────────────────

    pub fn name(env: Env) -> String {
        config(&env).name
    }

    pub fn symbol(env: Env) -> String {
        config(&env).symbol
    }

    pub fn decimals(_env: Env) -> u32 {
        INDEX_DECIMALS
    }

    pub fn total_supply(env: Env) -> i128 {
        supply(&env)
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        balance_of(&env, &id)
    }

    pub fn allowance(env: Env, from: Address, spender: Address) -> i128 {
        let value: Option<AllowanceValue> = env
            .storage()
            .temporary()
            .get(&DataKey::Allowance(from, spender));
        match value {
            Some(v) if v.expiration_ledger >= env.ledger().sequence() => v.amount,
            _ => 0,
        }
    }

    pub fn approve(env: Env, from: Address, spender: Address, amount: i128, expiration_ledger: u32) {
        from.require_auth();
        if amount < 0 || (amount > 0 && expiration_ledger < env.ledger().sequence()) {
            panic_with_error!(&env, IndexError::InvalidAmount);
        }
        let key = DataKey::Allowance(from.clone(), spender.clone());
        env.storage().temporary().set(
            &key,
            &AllowanceValue {
                amount,
                expiration_ledger,
            },
        );
        if amount > 0 {
            let live_for = expiration_ledger
                .saturating_sub(env.ledger().sequence())
                .max(1);
            env.storage().temporary().extend_ttl(&key, live_for, live_for);
        }
        #[allow(deprecated)]
        env.events().publish(
            (Symbol::new(&env, "approve"), from, spender),
            (amount, expiration_ledger),
        );
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        move_balance(&env, &from, &to, amount);
        #[allow(deprecated)]
        env.events()
            .publish((symbol_short!("transfer"), from, to), amount);
    }

    pub fn transfer_from(env: Env, spender: Address, from: Address, to: Address, amount: i128) {
        spender.require_auth();
        spend_allowance(&env, &from, &spender, amount);
        move_balance(&env, &from, &to, amount);
        #[allow(deprecated)]
        env.events()
            .publish((symbol_short!("transfer"), from, to), amount);
    }
}

#[cfg(test)]
mod test;
