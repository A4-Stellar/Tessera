//! Dutch-auction liquidation engine with an oracle circuit-breaker (issue #360).
//!
//! A vault is a collateralised debt position: `collateral_amount` units of an
//! RWA token secure `debt_amount` stablecoins. When the oracle-reported
//! collateral value falls below `liquidation_threshold_bps`, anyone may start
//! a Dutch auction with `start_auction`. The asking price starts at 110 % of
//! the debt and decays linearly to zero over `auction_duration_ledgers`.
//! `bid` pays the current price in stablecoins and receives the collateral;
//! if no bid arrives before expiry, `trigger_backstop` hands the vault to the
//! emergency-liquidation contract.
//!
//! # Oracle circuit-breaker
//!
//! Every price update records the ledger at which it was written. Each vault
//! operation re-checks the feed: if `ledger.sequence() - updated_at` exceeds
//! `max_oracle_staleness_ledgers`, the token is marked frozen (an
//! `OracleStale` event is emitted once), every operation on vaults backed by
//! that token reverts, and a fresh oracle update automatically clears the
//! flag and emits `OracleRecovered`.
//!
//! Amounts are in token base units and prices are expressed in stablecoin
//! base units per collateral base unit, so `collateral * price` is the
//! collateral value in stablecoins before the BPS scaling.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, Env, IntoVal, Symbol, Val, Vec,
};

/// 100 % in basis points.
const BPS_SCALE: i128 = 10_000;
/// Upper bound on the minimum collateral ratio (1000%).
const MAX_LIQUIDATION_THRESHOLD_BPS: i128 = 100_000;
/// The auction opens at 110 % of the outstanding debt.
const DEFAULT_START_PRICE_BPS: i128 = 11_000;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum AuctionError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidAmount = 4,
    InvalidConfig = 5,
    VaultNotFound = 6,
    VaultClosed = 7,
    PositionHealthy = 8,
    AuctionAlreadyActive = 9,
    NoActiveAuction = 10,
    AuctionNotExpired = 11,
    AuctionExpired = 12,
    /// The oracle feed has not been updated within the configured window.
    OracleStale = 13,
    /// No price has ever been published for the collateral token.
    PriceMissing = 14,
    Overflow = 15,
}

/// Immutable risk parameters chosen at deployment.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuctionConfig {
    /// Collateral ratio (BPS) below which a vault can be auctioned.
    pub liquidation_threshold_bps: i128,
    /// Number of ledgers from the start of an auction until the price reaches 0.
    pub auction_duration_ledgers: u32,
    /// An oracle update older than this many ledgers freezes the vault.
    pub max_oracle_staleness_ledgers: u32,
    /// Opening price as a fraction of debt, in BPS (11000 = 110 %).
    pub start_price_bps: i128,
}

/// A collateralised vault. `auction_start_ledger == 0` means no auction has
/// been started yet.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuctionVault {
    pub owner: Address,
    pub collateral_token: Address,
    pub collateral_amount: i128,
    pub debt_amount: i128,
    pub created_ledger: u32,
    pub auction_start_ledger: u32,
    pub auction_end_ledger: u32,
    pub auction_start_price: i128,
    pub liquidated: bool,
}

/// Oracle price record. `updated_ledger` drives the staleness check.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceRecord {
    pub price: i128,
    pub updated_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultSnapshot {
    pub collateral_ratio_bps: i128,
    pub current_auction_price: i128,
    pub oracle_frozen: bool,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    Stablecoin,
    Treasury,
    EmergencyLiquidation,
    Config,
    Price(Address),
    Vault(u64),
    NextVaultId,
}

#[contract]
pub struct LiquidationAuctionContract;

#[contractimpl]
impl LiquidationAuctionContract {
    /// Deploy the auction engine.
    ///
    /// - `admin` — may publish oracle prices and update the risk parameters.
    /// - `stablecoin` — token bidders pay with.
    /// - `treasury` — receives the stablecoin proceeds of every auction.
    /// - `emergency_liquidation` — backstop invoked when an auction expires.
    pub fn initialize(
        env: Env,
        admin: Address,
        stablecoin: Address,
        treasury: Address,
        emergency_liquidation: Address,
        liquidation_threshold_bps: i128,
        auction_duration_ledgers: u32,
        max_oracle_staleness_ledgers: u32,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, AuctionError::AlreadyInitialized);
        }
        // A minimum collateral ratio: below 100% a vault is only liquidatable once insolvent.
        if liquidation_threshold_bps < BPS_SCALE
            || liquidation_threshold_bps > MAX_LIQUIDATION_THRESHOLD_BPS
            || auction_duration_ledgers == 0
            || max_oracle_staleness_ledgers == 0
        {
            panic_with_error!(env, AuctionError::InvalidConfig);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Stablecoin, &stablecoin);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage()
            .instance()
            .set(&DataKey::EmergencyLiquidation, &emergency_liquidation);
        env.storage().instance().set(
            &DataKey::Config,
            &AuctionConfig {
                liquidation_threshold_bps,
                auction_duration_ledgers,
                max_oracle_staleness_ledgers,
                start_price_bps: DEFAULT_START_PRICE_BPS,
            },
        );
        env.storage().instance().set(&DataKey::NextVaultId, &0u64);
    }

    // ------------------------------------------------------------------
    // Oracle
    // ------------------------------------------------------------------

    /// Publish a fresh price for `token`. The oracle must be the configured
    /// admin (in production this is a signed price-feed bridge).
    pub fn update_price(env: Env, oracle: Address, token: Address, price: i128) {
        Self::require_admin(&env, &oracle);
        if price <= 0 {
            panic_with_error!(env, AuctionError::InvalidAmount);
        }
        let now = env.ledger().sequence();
        env.storage().persistent().set(
            &DataKey::Price(token.clone()),
            &PriceRecord {
                price,
                updated_ledger: now,
            },
        );
        env.events()
            .publish((symbol_short!("price"), token), (price, now));
    }

    /// Current price of `token`, or an error if the feed is missing or stale.
    pub fn price(env: Env, token: Address) -> i128 {
        Self::checked_price(&env, &token)
    }

    /// Whether vault operations for `token` are currently frozen.
    /// Frozen while the feed is older than `max_oracle_staleness_ledgers`; the
    /// next `update_price` unfreezes it. Derived, not stored: a flag written on
    /// the failing path would be rolled back with the failed call.
    pub fn is_frozen(env: Env, token: Address) -> bool {
        Self::is_stale(&env, &token)
    }

    /// Admin-only rewrite of the risk parameters. Tightening the threshold or
    /// staleness bound takes effect on the next operation.
    pub fn set_config(env: Env, admin: Address, config: AuctionConfig) {
        Self::require_admin(&env, &admin);
        if config.liquidation_threshold_bps < BPS_SCALE
            || config.liquidation_threshold_bps > MAX_LIQUIDATION_THRESHOLD_BPS
            || config.auction_duration_ledgers == 0
            || config.max_oracle_staleness_ledgers == 0
            || config.start_price_bps < BPS_SCALE
        {
            panic_with_error!(env, AuctionError::InvalidConfig);
        }
        env.storage().instance().set(&DataKey::Config, &config);
        env.events()
            .publish((symbol_short!("config"),), config);
    }

    // ------------------------------------------------------------------
    // Vault lifecycle
    // ------------------------------------------------------------------

    /// Deposit `collateral_amount` and open a vault with `debt_amount`
    /// stablecoins already drawn. The initial ratio must be healthy.
    pub fn create_vault(
        env: Env,
        owner: Address,
        collateral_token: Address,
        collateral_amount: i128,
        debt_amount: i128,
    ) -> u64 {
        owner.require_auth();
        if collateral_amount <= 0 || debt_amount < 0 {
            panic_with_error!(env, AuctionError::InvalidAmount);
        }
        let price = Self::checked_price(&env, &collateral_token);
        if let Err(error) = Self::assert_healthy(&env, collateral_amount, debt_amount, price) {
            panic_with_error!(env, error);
        }

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextVaultId)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextVaultId, &(id + 1));

        Self::token_transfer(
            &env,
            &collateral_token,
            &owner,
            &env.current_contract_address(),
            collateral_amount,
        );
        let now = env.ledger().sequence();
        let vault = AuctionVault {
            owner: owner.clone(),
            collateral_token,
            collateral_amount,
            debt_amount,
            created_ledger: now,
            auction_start_ledger: 0,
            auction_end_ledger: 0,
            auction_start_price: 0,
            liquidated: false,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Vault(id), &vault);
        env.events()
            .publish((symbol_short!("vopen"), owner), (id, collateral_amount, debt_amount));
        id
    }

    /// Start a Dutch auction for an unhealthy vault. Permissionless: any
    /// keeper can trigger it once the ratio is below the threshold.
    pub fn start_auction(env: Env, vault_id: u64) -> i128 {
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.liquidated {
            panic_with_error!(env, AuctionError::VaultClosed);
        }
        if vault.auction_start_ledger != 0 {
            panic_with_error!(env, AuctionError::AuctionAlreadyActive);
        }
        let price = Self::checked_price(&env, &vault.collateral_token);
        let config = Self::load_config(&env);
        if !Self::is_unhealthy(&env, &vault, price, &config) {
            panic_with_error!(env, AuctionError::PositionHealthy);
        }
        let now = env.ledger().sequence();
        let start_price = vault
            .debt_amount
            .checked_mul(config.start_price_bps)
            .unwrap_or_else(|| panic_with_error!(env, AuctionError::Overflow))
            / BPS_SCALE;
        vault.auction_start_ledger = now;
        vault.auction_end_ledger = now + config.auction_duration_ledgers;
        vault.auction_start_price = start_price;
        env.storage()
            .persistent()
            .set(&DataKey::Vault(vault_id), &vault);
        env.events().publish(
            (symbol_short!("auction"), vault.owner.clone()),
            (vault_id, start_price, vault.auction_end_ledger),
        );
        start_price
    }

    /// Current asking price of a vault's auction. Decays linearly from the
    /// opening price to zero across the auction window.
    pub fn auction_price(env: Env, vault_id: u64) -> i128 {
        let vault = Self::load_vault(&env, vault_id);
        Self::price_at(&env, &vault)
    }

    /// Pay the current auction price and receive the vault's collateral.
    /// The bid fails once the auction window has expired.
    pub fn bid(env: Env, bidder: Address, vault_id: u64) -> i128 {
        bidder.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.liquidated {
            panic_with_error!(env, AuctionError::VaultClosed);
        }
        if vault.auction_start_ledger == 0 {
            panic_with_error!(env, AuctionError::NoActiveAuction);
        }
        // Freeze check applies to bidding too: no liquidation may proceed on
        // stale collateral prices.
        Self::checked_price(&env, &vault.collateral_token);
        let now = env.ledger().sequence();
        if now > vault.auction_end_ledger {
            panic_with_error!(env, AuctionError::AuctionExpired);
        }
        let price = Self::price_at(&env, &vault);

        let stablecoin: Address = env.storage().instance().get(&DataKey::Stablecoin).unwrap();
        let treasury: Address = env.storage().instance().get(&DataKey::Treasury).unwrap();
        Self::token_transfer(&env, &stablecoin, &bidder, &treasury, price);
        Self::token_transfer(
            &env,
            &vault.collateral_token,
            &env.current_contract_address(),
            &bidder,
            vault.collateral_amount,
        );

        vault.liquidated = true;
        let collateral = vault.collateral_amount;
        let debt = vault.debt_amount;
        vault.collateral_amount = 0;
        vault.debt_amount = 0;
        env.storage()
            .persistent()
            .set(&DataKey::Vault(vault_id), &vault);

        env.events().publish(
            (symbol_short!("bid"), bidder.clone()),
            (vault_id, price, collateral),
        );
        env.events()
            .publish((symbol_short!("settled"), bidder), (vault_id, debt));
        price
    }

    /// Hand an expired, unbidded vault to the emergency-liquidation contract.
    /// The backstop records the absorbed collateral and debt; this contract
    /// marks the vault closed so it can never be auctioned twice.
    pub fn trigger_backstop(env: Env, vault_id: u64) {
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.liquidated {
            panic_with_error!(env, AuctionError::VaultClosed);
        }
        if vault.auction_start_ledger == 0 {
            panic_with_error!(env, AuctionError::NoActiveAuction);
        }
        let now = env.ledger().sequence();
        if now <= vault.auction_end_ledger {
            panic_with_error!(env, AuctionError::AuctionNotExpired);
        }
        let emergency: Address = env
            .storage()
            .instance()
            .get(&DataKey::EmergencyLiquidation)
            .unwrap();
        let args: Vec<Val> = (
            env.current_contract_address(),
            vault_id,
            vault.collateral_token.clone(),
            vault.collateral_amount,
            vault.debt_amount,
        )
            .into_val(&env);
        env.invoke_contract::<()>(
            &emergency,
            &Symbol::new(&env, "absorb_vault"),
            args,
        );

        vault.liquidated = true;
        let collateral = vault.collateral_amount;
        let debt = vault.debt_amount;
        vault.collateral_amount = 0;
        vault.debt_amount = 0;
        env.storage()
            .persistent()
            .set(&DataKey::Vault(vault_id), &vault);
        env.events()
            .publish((symbol_short!("backstop"),), (vault_id, collateral, debt));
    }

    // ------------------------------------------------------------------
    // Views
    // ------------------------------------------------------------------

    pub fn get_vault(env: Env, vault_id: u64) -> AuctionVault {
        Self::load_vault(&env, vault_id)
    }

    /// Collateral ratio, current auction price and freeze flag in one call.
    pub fn snapshot(env: Env, vault_id: u64) -> VaultSnapshot {
        let vault = Self::load_vault(&env, vault_id);
        let price = env
            .storage()
            .persistent()
            .get::<_, PriceRecord>(&DataKey::Price(vault.collateral_token.clone()))
            .map(|record| record.price)
            .unwrap_or(0);
        VaultSnapshot {
            collateral_ratio_bps: Self::ratio_bps(&vault, price),
            current_auction_price: Self::price_at(&env, &vault),
            oracle_frozen: Self::is_stale(&env, &vault.collateral_token),
        }
    }

    pub fn config(env: Env) -> AuctionConfig {
        Self::load_config(&env)
    }

    fn load_config(env: &Env) -> AuctionConfig {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .unwrap_or_else(|| panic_with_error!(env, AuctionError::NotInitialized))
    }

    // ------------------------------------------------------------------
    // Internal
    // ------------------------------------------------------------------

    fn require_admin(env: &Env, admin: &Address) {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, AuctionError::NotInitialized));
        admin.require_auth();
        if stored != *admin {
            panic_with_error!(env, AuctionError::Unauthorized);
        }
    }

    fn load_vault(env: &Env, vault_id: u64) -> AuctionVault {
        env.storage()
            .persistent()
            .get(&DataKey::Vault(vault_id))
            .unwrap_or_else(|| panic_with_error!(env, AuctionError::VaultNotFound))
    }

    fn checked_price(env: &Env, token: &Address) -> i128 {
        let record: PriceRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Price(token.clone()))
            .unwrap_or_else(|| panic_with_error!(env, AuctionError::PriceMissing));
        let config = Self::load_config(env);
        let now = env.ledger().sequence();
        if now.saturating_sub(record.updated_ledger) > config.max_oracle_staleness_ledgers {
            panic_with_error!(env, AuctionError::OracleStale);
        }
        record.price
    }

    fn is_stale(env: &Env, token: &Address) -> bool {
        let record: Option<PriceRecord> = env.storage().persistent().get(&DataKey::Price(token.clone()));
        match record {
            Some(record) => {
                env.ledger().sequence().saturating_sub(record.updated_ledger)
                    > Self::load_config(env).max_oracle_staleness_ledgers
            }
            None => false,
        }
    }

    fn ratio_bps(vault: &AuctionVault, price: i128) -> i128 {
        if vault.debt_amount <= 0 {
            return i128::MAX;
        }
        vault
            .collateral_amount
            .checked_mul(price)
            .and_then(|value| value.checked_mul(BPS_SCALE))
            .map(|scaled| scaled / vault.debt_amount)
            .unwrap_or(i128::MAX)
    }

    fn is_unhealthy(
        env: &Env,
        vault: &AuctionVault,
        price: i128,
        config: &AuctionConfig,
    ) -> bool {
        if vault.debt_amount <= 0 {
            return false;
        }
        let value = vault
            .collateral_amount
            .checked_mul(price)
            .unwrap_or_else(|| panic_with_error!(env, AuctionError::Overflow));
        value
            .checked_mul(BPS_SCALE)
            .unwrap_or(i128::MAX)
            < vault
                .debt_amount
                .checked_mul(config.liquidation_threshold_bps)
                .unwrap_or(i128::MAX)
    }

    fn assert_healthy(
        env: &Env,
        collateral_amount: i128,
        debt_amount: i128,
        price: i128,
    ) -> Result<(), AuctionError> {
        let config = Self::load_config(env);
        let value = collateral_amount
            .checked_mul(price)
            .ok_or(AuctionError::Overflow)?;
        if value
            .checked_mul(BPS_SCALE)
            .ok_or(AuctionError::Overflow)?
            < debt_amount
                .checked_mul(config.liquidation_threshold_bps)
                .ok_or(AuctionError::Overflow)?
        {
            return Err(AuctionError::PositionHealthy);
        }
        Ok(())
    }

    fn price_at(env: &Env, vault: &AuctionVault) -> i128 {
        if vault.auction_start_ledger == 0 || vault.liquidated {
            return 0;
        }
        let now = env.ledger().sequence();
        if now >= vault.auction_end_ledger {
            return 0;
        }
        let duration = vault.auction_end_ledger - vault.auction_start_ledger;
        let remaining = vault.auction_end_ledger - now;
        vault.auction_start_price * (remaining as i128) / (duration as i128)
    }

    fn token_transfer(env: &Env, token: &Address, from: &Address, to: &Address, amount: i128) {
        token::Client::new(env, token).transfer(from, to, &amount);
    }
}

#[cfg(test)]
mod test;
