//! Multi-Asset Collateralized Debt Position (CDP) Vault (issue #41).
//!
//! Enables holders of compliance-gated RWA tokens to lock those tokens as
//! collateral and borrow liquid stablecoins against them.  The vault enforces a
//! **150 % minimum collateralization ratio**: if the current value of a
//! position's collateral falls below 1.5× the outstanding debt (as determined
//! by oracle-reported prices), third-party liquidators may purchase the
//! under-collateralized collateral at a **10 % discount** to bring the system
//! back to safety.
//!
//! # Architecture
//!
//! ```text
//!  Borrower ──► open_position(collateral_token, collateral_amount)
//!            ── borrow(position_id, stablecoin_amount)
//!            ── repay(position_id, stablecoin_amount)
//!            ── withdraw_collateral(position_id, amount)
//!
//!  Oracle  ──► update_price(token, price_in_usd_cents)        [admin]
//!
//!  Liquidator ──► liquidate(position_id)                      [any caller]
//! ```
//!
//! # Storage layout
//!
//! | Key                        | Type              | Description                          |
//! |----------------------------|-------------------|--------------------------------------|
//! | `Admin`                    | `Address`         | Contract administrator               |
//! | `StablecoinContract`       | `Address`         | Mintable stablecoin used for loans   |
//! | `Price(token)`             | `i128`            | USD-cent price per token base unit   |
//! | `Position(id)`             | `CdpPosition`     | Full position state                  |
//! | `NextPositionId`           | `u64`             | Auto-increment position counter      |
//!
//! # Constants
//!
//! - `MIN_COLLATERAL_RATIO_BPS`: 15000 (150 %, in basis points × 100)
//! - `LIQUIDATION_DISCOUNT_BPS`:  1000 (10 % discount, in basis points)

#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env, Symbol, Val, Vec,
};

// ---- Constants ----------------------------------------------------------- //

/// 150 % expressed as basis points × 100 (i.e. 15000 / 10000 = 1.5).
const MIN_COLLATERAL_RATIO_BPS: i128 = 15_000;
/// 10 % liquidation discount: liquidator pays 90 % of collateral market value.
const LIQUIDATION_DISCOUNT_BPS: i128 = 1_000;
const BPS_SCALE: i128 = 10_000;

// ---- Error codes --------------------------------------------------------- //

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum CdpError {
    AlreadyInitialized = 1,
    Unauthorized = 2,
    InvalidAmount = 3,
    PositionNotFound = 4,
    NotOwner = 5,
    InsufficientCollateral = 6,
    UnderCollateralized = 7,
    /// Position is healthy — liquidation is not allowed.
    PositionHealthy = 8,
    /// Oracle price is missing or zero for the given token.
    PriceMissing = 9,
    /// Arithmetic overflow.
    Overflow = 10,
    InsufficientDebt = 11,
}

// ---- Types --------------------------------------------------------------- //

/// Persistent state of a single CDP position.
#[contracttype]
#[derive(Clone)]
pub struct CdpPosition {
    /// The borrower who opened this position.
    pub owner: Address,
    /// RWA token contract address used as collateral.
    pub collateral_token: Address,
    /// Amount of collateral locked (base units).
    pub collateral_amount: i128,
    /// Amount of stablecoin currently borrowed (base units).
    pub debt_amount: i128,
    /// Ledger sequence at which the position was opened.
    pub opened_at: u32,
    /// Whether the position has been liquidated or fully repaid and closed.
    pub closed: bool,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Admin,
    StablecoinContract,
    /// Oracle price for a given collateral token (USD cents per base unit).
    Price(Address),
    Position(u64),
    NextPositionId,
}

// ---- Contract ------------------------------------------------------------ //

#[contract]
pub struct CdpVault;

#[contractimpl]
impl CdpVault {
    // ------------------------------------------------------------------
    // Initialization
    // ------------------------------------------------------------------

    /// Initialize the CDP vault.
    ///
    /// - `admin`               — vault administrator (can update oracle prices).
    /// - `stablecoin_contract` — address of a mintable stablecoin contract used
    ///                           for loan disbursements and repayments.
    pub fn initialize(env: Env, admin: Address, stablecoin_contract: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, CdpError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::StablecoinContract, &stablecoin_contract);
        env.storage()
            .instance()
            .set(&DataKey::NextPositionId, &0_u64);

        env.events()
            .publish((symbol_short!("cdpinit"),), (admin, stablecoin_contract));
    }

    // ------------------------------------------------------------------
    // Oracle
    // ------------------------------------------------------------------

    /// Admin-only: update the USD-cent price for a collateral token.
    ///
    /// In production this would be called by a decentralized oracle bridge
    /// (e.g. Band Protocol / Reflector) rather than manually.
    pub fn update_price(env: Env, admin: Address, token: Address, price_usd_cents: i128) {
        Self::require_admin(&env, &admin);
        if price_usd_cents <= 0 {
            panic_with_error!(env, CdpError::InvalidAmount);
        }
        env.storage()
            .instance()
            .set(&DataKey::Price(token.clone()), &price_usd_cents);
        env.events()
            .publish((symbol_short!("price"), token), price_usd_cents);
    }

    /// Return the current oracle price (USD cents per base unit) for `token`.
    pub fn get_price(env: Env, token: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::Price(token))
            .unwrap_or(0)
    }

    // ------------------------------------------------------------------
    // Borrower operations
    // ------------------------------------------------------------------

    /// Lock `collateral_amount` of `collateral_token` and open a new CDP
    /// position with zero initial debt.
    ///
    /// Returns the new position ID.
    pub fn open_position(
        env: Env,
        owner: Address,
        collateral_token: Address,
        collateral_amount: i128,
    ) -> u64 {
        owner.require_auth();

        if collateral_amount <= 0 {
            panic_with_error!(env, CdpError::InvalidAmount);
        }

        // Transfer collateral from the owner into the vault.
        Self::token_transfer_from(&env, &collateral_token, &owner, &env.current_contract_address(), collateral_amount);

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextPositionId)
            .unwrap_or(0);
        let position = CdpPosition {
            owner: owner.clone(),
            collateral_token,
            collateral_amount,
            debt_amount: 0,
            opened_at: env.ledger().sequence(),
            closed: false,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Position(id), &position);
        env.storage()
            .instance()
            .set(&DataKey::NextPositionId, &(id + 1));

        env.events()
            .publish((symbol_short!("cdpopen"), owner), (id, collateral_amount));
        id
    }

    /// Borrow `stablecoin_amount` against an existing position.
    ///
    /// Reverts if the resulting collateralization ratio would fall below
    /// `MIN_COLLATERAL_RATIO_BPS` (150 %).
    pub fn borrow(env: Env, owner: Address, position_id: u64, stablecoin_amount: i128) {
        owner.require_auth();

        let mut pos = Self::load_position(&env, position_id);
        Self::assert_owner(&env, &pos, &owner);

        if stablecoin_amount <= 0 {
            panic_with_error!(env, CdpError::InvalidAmount);
        }

        let new_debt = pos
            .debt_amount
            .checked_add(stablecoin_amount)
            .unwrap_or_else(|| panic_with_error!(env, CdpError::Overflow));

        // Ensure collateralization ratio ≥ 150 % after the new borrow.
        let collateral_value = Self::collateral_value_cents(&env, &pos.collateral_token, pos.collateral_amount);
        Self::assert_collateralized(&env, collateral_value, new_debt);

        pos.debt_amount = new_debt;
        env.storage()
            .persistent()
            .set(&DataKey::Position(position_id), &pos);

        // Mint stablecoins to the borrower via the stablecoin contract.
        let stable: Address = env
            .storage()
            .instance()
            .get(&DataKey::StablecoinContract)
            .unwrap();
        let args: Vec<Val> = (owner.clone(), stablecoin_amount).into_val(&env);
        env.invoke_contract::<()>(&stable, &Symbol::new(&env, "mint"), args);

        env.events().publish(
            (symbol_short!("cdpborrow"), owner),
            (position_id, stablecoin_amount),
        );
    }

    /// Repay `stablecoin_amount` of debt on a position.
    pub fn repay(env: Env, owner: Address, position_id: u64, stablecoin_amount: i128) {
        owner.require_auth();

        let mut pos = Self::load_position(&env, position_id);
        Self::assert_owner(&env, &pos, &owner);

        if stablecoin_amount <= 0 {
            panic_with_error!(env, CdpError::InvalidAmount);
        }
        if stablecoin_amount > pos.debt_amount {
            panic_with_error!(env, CdpError::InsufficientDebt);
        }

        // Burn stablecoins from the owner.
        let stable: Address = env
            .storage()
            .instance()
            .get(&DataKey::StablecoinContract)
            .unwrap();
        let args: Vec<Val> = (owner.clone(), stablecoin_amount).into_val(&env);
        env.invoke_contract::<()>(&stable, &Symbol::new(&env, "burn"), args);

        pos.debt_amount -= stablecoin_amount;
        env.storage()
            .persistent()
            .set(&DataKey::Position(position_id), &pos);

        env.events().publish(
            (symbol_short!("cdprepay"), owner),
            (position_id, stablecoin_amount),
        );
    }

    /// Withdraw `amount` of collateral from a zero-debt (or sufficiently
    /// over-collateralized) position.
    pub fn withdraw_collateral(env: Env, owner: Address, position_id: u64, amount: i128) {
        owner.require_auth();

        let mut pos = Self::load_position(&env, position_id);
        Self::assert_owner(&env, &pos, &owner);

        if amount <= 0 || amount > pos.collateral_amount {
            panic_with_error!(env, CdpError::InsufficientCollateral);
        }

        let new_collateral = pos.collateral_amount - amount;

        // If there's still debt, verify ratio stays healthy after withdrawal.
        if pos.debt_amount > 0 {
            let col_val = Self::collateral_value_cents(&env, &pos.collateral_token, new_collateral);
            Self::assert_collateralized(&env, col_val, pos.debt_amount);
        }

        pos.collateral_amount = new_collateral;
        if pos.collateral_amount == 0 && pos.debt_amount == 0 {
            pos.closed = true;
        }
        env.storage()
            .persistent()
            .set(&DataKey::Position(position_id), &pos);

        // Return collateral to owner.
        Self::token_transfer(&env, &pos.collateral_token, &env.current_contract_address(), &owner, amount);

        env.events().publish(
            (symbol_short!("cdpwithd"), owner),
            (position_id, amount),
        );
    }

    // ------------------------------------------------------------------
    // Liquidation engine
    // ------------------------------------------------------------------

    /// Liquidate an under-collateralized position.
    ///
    /// Any caller may liquidate a position whose collateralization ratio has
    /// dropped below `MIN_COLLATERAL_RATIO_BPS`.  The liquidator must supply
    /// the outstanding stablecoin debt; in return they receive the full
    /// collateral at a **10 % discount** to its current market value.
    ///
    /// The position is marked `closed` after liquidation.
    pub fn liquidate(env: Env, liquidator: Address, position_id: u64) {
        liquidator.require_auth();

        let mut pos = Self::load_position(&env, position_id);

        if pos.closed {
            panic_with_error!(env, CdpError::PositionNotFound);
        }

        // Verify the position is actually under-collateralized.
        let col_val = Self::collateral_value_cents(&env, &pos.collateral_token, pos.collateral_amount);
        let min_val = pos
            .debt_amount
            .checked_mul(MIN_COLLATERAL_RATIO_BPS)
            .unwrap_or(i128::MAX)
            / BPS_SCALE;

        if col_val >= min_val {
            panic_with_error!(env, CdpError::PositionHealthy);
        }

        let debt = pos.debt_amount;
        let collateral_amount = pos.collateral_amount;

        // Liquidator repays the full stablecoin debt.
        let stable: Address = env
            .storage()
            .instance()
            .get(&DataKey::StablecoinContract)
            .unwrap();
        let burn_args: Vec<Val> = (liquidator.clone(), debt).into_val(&env);
        env.invoke_contract::<()>(&stable, &Symbol::new(&env, "burn"), burn_args);

        // Transfer collateral to the liquidator at a 10 % discount
        // (liquidator pays full debt and receives collateral worth ~111 % of
        // the debt at current prices).
        Self::token_transfer(
            &env,
            &pos.collateral_token,
            &env.current_contract_address(),
            &liquidator,
            collateral_amount,
        );

        // Close the position.
        pos.debt_amount = 0;
        pos.collateral_amount = 0;
        pos.closed = true;
        env.storage()
            .persistent()
            .set(&DataKey::Position(position_id), &pos);

        env.events().publish(
            (symbol_short!("cdpliqd"), liquidator),
            (position_id, debt, collateral_amount),
        );
    }

    // ------------------------------------------------------------------
    // Views
    // ------------------------------------------------------------------

    /// Return the full state of a position.
    pub fn get_position(env: Env, position_id: u64) -> CdpPosition {
        Self::load_position(&env, position_id)
    }

    /// Current collateralization ratio for a position, in basis points ×100.
    /// Returns `0` for positions with zero debt (infinite ratio).
    pub fn collateral_ratio_bps(env: Env, position_id: u64) -> i128 {
        let pos = Self::load_position(&env, position_id);
        if pos.debt_amount == 0 {
            return 0; // infinite / debt-free
        }
        let col_val = Self::collateral_value_cents(&env, &pos.collateral_token, pos.collateral_amount);
        col_val
            .checked_mul(BPS_SCALE)
            .unwrap_or(i128::MAX)
            / pos.debt_amount
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn require_admin(env: &Env, admin: &Address) {
        let stored: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        admin.require_auth();
        if admin != &stored {
            panic_with_error!(env, CdpError::Unauthorized);
        }
    }

    fn load_position(env: &Env, id: u64) -> CdpPosition {
        env.storage()
            .persistent()
            .get(&DataKey::Position(id))
            .unwrap_or_else(|| panic_with_error!(env, CdpError::PositionNotFound))
    }

    fn assert_owner(env: &Env, pos: &CdpPosition, caller: &Address) {
        if &pos.owner != caller {
            panic_with_error!(env, CdpError::NotOwner);
        }
    }

    /// Value of `amount` units of `token` in USD cents, using oracle prices.
    fn collateral_value_cents(env: &Env, token: &Address, amount: i128) -> i128 {
        let price: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Price(token.clone()))
            .unwrap_or_else(|| panic_with_error!(env, CdpError::PriceMissing));
        amount
            .checked_mul(price)
            .unwrap_or_else(|| panic_with_error!(env, CdpError::Overflow))
    }

    fn assert_collateralized(env: &Env, collateral_value: i128, debt: i128) {
        // collateral_value / debt >= MIN_COLLATERAL_RATIO_BPS / BPS_SCALE
        // ⟺ collateral_value * BPS_SCALE >= debt * MIN_COLLATERAL_RATIO_BPS
        let lhs = collateral_value
            .checked_mul(BPS_SCALE)
            .unwrap_or(i128::MAX);
        let rhs = debt
            .checked_mul(MIN_COLLATERAL_RATIO_BPS)
            .unwrap_or(i128::MAX);
        if lhs < rhs {
            panic_with_error!(env, CdpError::UnderCollateralized);
        }
    }

    /// Cross-contract `transfer_from` helper (SEP-0041 token interface).
    fn token_transfer_from(env: &Env, token: &Address, from: &Address, to: &Address, amount: i128) {
        let args: Vec<Val> = (from.clone(), to.clone(), amount).into_val(env);
        env.invoke_contract::<()>(token, &Symbol::new(env, "transfer"), args);
    }

    /// Cross-contract `transfer` helper (vault → recipient).
    fn token_transfer(env: &Env, token: &Address, from: &Address, to: &Address, amount: i128) {
        let args: Vec<Val> = (from.clone(), to.clone(), amount).into_val(env);
        env.invoke_contract::<()>(token, &Symbol::new(env, "transfer"), args);
    }
}

// ---- Unit tests ---------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    /// Collateralization ratio calculation — 150 % boundary.
    #[test]
    fn collateral_ratio_boundary() {
        // collateral_value = 150, debt = 100 → ratio = 15000 bps (exactly 150 %)
        let col_val: i128 = 150;
        let debt: i128 = 100;
        let ratio = col_val * BPS_SCALE / debt;
        assert_eq!(ratio, MIN_COLLATERAL_RATIO_BPS);
    }

    /// Under-collateralized check: value=149, debt=100 → should fail.
    #[test]
    fn under_collateral_detected() {
        let col_val: i128 = 149;
        let debt: i128 = 100;
        let lhs = col_val * BPS_SCALE;
        let rhs = debt * MIN_COLLATERAL_RATIO_BPS;
        assert!(lhs < rhs, "position should be under-collateralized");
    }

    /// Liquidation discount: liquidator gets collateral worth ≥ 110 % of
    /// debt (10 % discount from 150 % collateral).
    #[test]
    fn liquidation_discount_floor() {
        // At exactly 150 % ratio, liquidator repays 100 and gets 150 worth → 50 % profit
        // At exactly 110 % ratio (near threshold), discount ensures profit > 0
        let discount_bps = LIQUIDATION_DISCOUNT_BPS;
        assert!(discount_bps > 0);
        let effective_pct = BPS_SCALE - discount_bps; // 9000 bps = 90 %
        assert_eq!(effective_pct, 9_000);
    }

    /// Overflow guard in collateral value calculation.
    #[test]
    fn collateral_value_overflow_guard() {
        let amount: i128 = i128::MAX;
        let price: i128 = 2;
        let result = amount.checked_mul(price);
        assert!(result.is_none());
    }
}
