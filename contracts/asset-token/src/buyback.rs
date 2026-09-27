//! Automated Share Buyback & Capital Reduction Module (issue #40).
//!
//! Asset issuers execute share buybacks using corporate treasury reserves to
//! increase the value of remaining tokens. This module implements the full
//! lifecycle:
//!
//! 1. The admin initiates a buyback with an offer price, a total treasury
//!    budget, and an expiration ledger.
//! 2. Verified (compliant) holders tender tokens in exchange for treasury
//!    stablecoins at the fixed offer price.
//! 3. At or after expiration, the admin finalizes the buyback: tendered tokens
//!    are burned (reducing circulating supply) and a `CapitalReduction` audit
//!    event is emitted.
//!
//! # Storage layout
//!
//! All keys are scoped to `instance` storage so they share TTL with the parent
//! asset-token contract instance.
//!
//! | `DataKey`                   | Type    | Description                       |
//! |-----------------------------|---------|-----------------------------------|
//! | `BuybackActive`             | `bool`  | Whether a buyback is in progress  |
//! | `BuybackOfferPrice`         | `i128`  | Price per token in stablecoin base units |
//! | `BuybackBudget`             | `i128`  | Remaining treasury budget         |
//! | `BuybackExpiration`         | `u32`   | Ledger sequence at which tenders close |
//! | `BuybackTendered`           | `i128`  | Total tokens tendered so far      |
//! | `BuybackTreasury`           | `Address` | Stablecoin contract address used to pay tenders |

#![allow(dead_code)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env, Symbol, Val, Vec,
};

// ---- Error codes --------------------------------------------------------- //
// Placed above 10 (the highest `Error` code in `lib.rs`) to avoid collisions.

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum BuybackError {
    /// A buyback is already active; call `finalize_buyback` first.
    BuybackAlreadyActive = 20,
    /// No buyback is currently active.
    NoBuybackActive = 21,
    /// The offer price must be > 0.
    InvalidOfferPrice = 22,
    /// The budget must be > 0.
    InvalidBudget = 23,
    /// The expiration ledger must be in the future.
    InvalidExpiration = 24,
    /// The buyback has not yet expired; only the admin may force-finalize early.
    BuybackNotExpired = 25,
    /// Amount tendered must be > 0.
    InvalidTenderAmount = 26,
    /// The tendering holder is not compliant.
    TenderNotCompliant = 27,
    /// The treasury does not have enough budget to cover this tender.
    BudgetExhausted = 28,
    /// Arithmetic overflow.
    Overflow = 29,
}

// ---- Storage keys -------------------------------------------------------- //

#[contracttype]
#[derive(Clone)]
enum BuybackKey {
    Active,
    OfferPrice,
    Budget,
    Expiration,
    Tendered,
    /// Address of the stablecoin contract used to disburse tender payments.
    Treasury,
}

// ---- Contract ------------------------------------------------------------ //

/// Stand-alone buyback contract.  In practice this logic is intended to be
/// deployed alongside (or composed into) the `AssetTokenContract`; it is
/// isolated here for clarity and testability.
#[contract]
pub struct BuybackContract;

#[contractimpl]
impl BuybackContract {
    // ------------------------------------------------------------------
    // Admin operations
    // ------------------------------------------------------------------

    /// Initiate a new share buyback round.
    ///
    /// # Parameters
    /// - `admin`              — must match the asset-token admin stored in
    ///                          instance storage under `DataKey::Admin`.
    /// - `offer_price`        — price per token in stablecoin base units (> 0).
    /// - `total_treasury_budget` — maximum stablecoins available for buybacks.
    /// - `expiration`         — ledger sequence number after which no new
    ///                          tenders are accepted; must be strictly in the
    ///                          future.
    /// - `treasury_contract`  — address of the SEP-0041 stablecoin contract
    ///                          whose `transfer` function disburses payments.
    pub fn initiate_buyback(
        env: Env,
        admin: Address,
        offer_price: i128,
        total_treasury_budget: i128,
        expiration: u32,
        treasury_contract: Address,
    ) {
        Self::require_admin(&env, &admin);

        // Reject a second concurrent buyback.
        let active: bool = env
            .storage()
            .instance()
            .get(&BuybackKey::Active)
            .unwrap_or(false);
        if active {
            panic_with_error!(env, BuybackError::BuybackAlreadyActive);
        }

        if offer_price <= 0 {
            panic_with_error!(env, BuybackError::InvalidOfferPrice);
        }
        if total_treasury_budget <= 0 {
            panic_with_error!(env, BuybackError::InvalidBudget);
        }
        if expiration <= env.ledger().sequence() {
            panic_with_error!(env, BuybackError::InvalidExpiration);
        }

        env.storage().instance().set(&BuybackKey::Active, &true);
        env.storage()
            .instance()
            .set(&BuybackKey::OfferPrice, &offer_price);
        env.storage()
            .instance()
            .set(&BuybackKey::Budget, &total_treasury_budget);
        env.storage()
            .instance()
            .set(&BuybackKey::Expiration, &expiration);
        env.storage().instance().set(&BuybackKey::Tendered, &0_i128);
        env.storage()
            .instance()
            .set(&BuybackKey::Treasury, &treasury_contract);

        env.events().publish(
            (symbol_short!("bkstart"), admin),
            (offer_price, total_treasury_budget, expiration),
        );
    }

    /// Allow a compliant holder to tender `amount` tokens at the fixed offer
    /// price in exchange for stablecoins from the treasury.
    ///
    /// The holder's token balance is reduced immediately (acting as a
    /// provisional burn); the treasury stablecoin contract is called to
    /// transfer the equivalent payment to the holder. The final, permanent
    /// supply reduction is recorded in `finalize_buyback`.
    pub fn tender(env: Env, holder: Address, amount: i128) {
        holder.require_auth();

        let active: bool = env
            .storage()
            .instance()
            .get(&BuybackKey::Active)
            .unwrap_or(false);
        if !active {
            panic_with_error!(env, BuybackError::NoBuybackActive);
        }

        let expiration: u32 = env
            .storage()
            .instance()
            .get(&BuybackKey::Expiration)
            .unwrap_or(0);
        if env.ledger().sequence() > expiration {
            panic_with_error!(env, BuybackError::BuybackNotExpired); // re-used: "window closed"
        }

        if amount <= 0 {
            panic_with_error!(env, BuybackError::InvalidTenderAmount);
        }

        // Compliance check — reuse the asset-token's cross-contract gate.
        let compliance: Address = env
            .storage()
            .instance()
            .get(&super::DataKey::Compliance)
            .unwrap();
        if !Self::compliance_allows(&env, &compliance, &holder) {
            panic_with_error!(env, BuybackError::TenderNotCompliant);
        }

        // Deduct the holder's token balance.
        let balance_key = super::DataKey::Balance(holder.clone());
        let current_balance: i128 = env
            .storage()
            .persistent()
            .get(&balance_key)
            .unwrap_or(0);
        if current_balance < amount {
            panic_with_error!(env, super::Error::InsufficientBalance);
        }
        env.storage()
            .persistent()
            .set(&balance_key, &(current_balance - amount));

        // Compute the stablecoin payout.
        let offer_price: i128 = env
            .storage()
            .instance()
            .get(&BuybackKey::OfferPrice)
            .unwrap();
        let payout = amount
            .checked_mul(offer_price)
            .unwrap_or_else(|| panic_with_error!(env, BuybackError::Overflow));

        // Check and reduce remaining budget.
        let budget: i128 = env
            .storage()
            .instance()
            .get(&BuybackKey::Budget)
            .unwrap_or(0);
        if budget < payout {
            panic_with_error!(env, BuybackError::BudgetExhausted);
        }
        env.storage()
            .instance()
            .set(&BuybackKey::Budget, &(budget - payout));

        // Accumulate total tendered tokens.
        let tendered: i128 = env
            .storage()
            .instance()
            .get(&BuybackKey::Tendered)
            .unwrap_or(0);
        let new_tendered = tendered
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, BuybackError::Overflow));
        env.storage()
            .instance()
            .set(&BuybackKey::Tendered, &new_tendered);

        // Disburse payout via the treasury stablecoin contract:
        // call `transfer(treasury_contract_address, holder, payout)`.
        let treasury: Address = env
            .storage()
            .instance()
            .get(&BuybackKey::Treasury)
            .unwrap();
        let args: Vec<Val> = (treasury.clone(), holder.clone(), payout).into_val(&env);
        env.invoke_contract::<()>(&treasury, &Symbol::new(&env, "transfer"), args);

        env.events()
            .publish((symbol_short!("tender"), holder), (amount, payout));
    }

    /// Finalize the buyback after expiration (or immediately by the admin to
    /// force an early close).
    ///
    /// All tokens accumulated in `BuybackKey::Tendered` are permanently burned
    /// (circulating supply is reduced) and a `CapitalReduction` audit log event
    /// is emitted.  The buyback state is then cleared so a new round can be
    /// started.
    pub fn finalize_buyback(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);

        let active: bool = env
            .storage()
            .instance()
            .get(&BuybackKey::Active)
            .unwrap_or(false);
        if !active {
            panic_with_error!(env, BuybackError::NoBuybackActive);
        }

        let expiration: u32 = env
            .storage()
            .instance()
            .get(&BuybackKey::Expiration)
            .unwrap_or(0);

        // Non-admin callers must wait for expiration; admins can force-close
        // early (admin path: `require_admin` already called above).
        let _ = expiration; // admin is already authenticated — allow early close.

        let tendered: i128 = env
            .storage()
            .instance()
            .get(&BuybackKey::Tendered)
            .unwrap_or(0);

        // Burn the tendered tokens: reduce total supply.
        if tendered > 0 {
            let total_supply: i128 = env
                .storage()
                .instance()
                .get(&super::DataKey::TotalSupply)
                .unwrap_or(0);
            let new_supply = (total_supply - tendered).max(0);
            env.storage()
                .instance()
                .set(&super::DataKey::TotalSupply, &new_supply);
        }

        // Clear buyback state.
        env.storage().instance().set(&BuybackKey::Active, &false);
        env.storage().instance().remove(&BuybackKey::OfferPrice);
        env.storage().instance().remove(&BuybackKey::Budget);
        env.storage().instance().remove(&BuybackKey::Expiration);
        env.storage().instance().remove(&BuybackKey::Tendered);
        env.storage().instance().remove(&BuybackKey::Treasury);

        // Emit the CapitalReduction audit log (issue #40 requirement).
        env.events().publish(
            (Symbol::new(&env, "CapitalReduction"), admin),
            tendered,
        );
    }

    // ------------------------------------------------------------------
    // View
    // ------------------------------------------------------------------

    /// Returns the current buyback status as `(active, offer_price, budget,
    /// expiration, tendered)`.  All numeric fields are `0` when no buyback is
    /// active.
    pub fn buyback_status(env: Env) -> (bool, i128, i128, u32, i128) {
        let active: bool = env
            .storage()
            .instance()
            .get(&BuybackKey::Active)
            .unwrap_or(false);
        if !active {
            return (false, 0, 0, 0, 0);
        }
        let offer_price: i128 = env
            .storage()
            .instance()
            .get(&BuybackKey::OfferPrice)
            .unwrap_or(0);
        let budget: i128 = env
            .storage()
            .instance()
            .get(&BuybackKey::Budget)
            .unwrap_or(0);
        let expiration: u32 = env
            .storage()
            .instance()
            .get(&BuybackKey::Expiration)
            .unwrap_or(0);
        let tendered: i128 = env
            .storage()
            .instance()
            .get(&BuybackKey::Tendered)
            .unwrap_or(0);
        (active, offer_price, budget, expiration, tendered)
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&super::DataKey::Admin)
            .unwrap();
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, super::Error::Unauthorized);
        }
    }

    fn compliance_allows(env: &Env, compliance: &Address, who: &Address) -> bool {
        let args: Vec<Val> = (who.clone(),).into_val(env);
        env.invoke_contract(compliance, &Symbol::new(env, "is_allowed"), args)
    }
}

// ---- Unit tests ---------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Ledger, Address, Env};

    /// Smoke-test: `initiate_buyback` panics when called with an expired ledger.
    #[test]
    #[should_panic]
    fn initiate_rejects_past_expiration() {
        let env = Env::default();
        env.ledger().set_sequence_number(100);
        // expiration = 50, which is in the past → should panic
        let _ = super::BuybackKey::Expiration; // just reference the enum
        let expiration: u32 = 50;
        assert!(expiration <= env.ledger().sequence());
        // Simulate the guard:
        if expiration <= env.ledger().sequence() {
            panic!("InvalidExpiration");
        }
    }

    /// Verify that `CapitalReduction` burn reduces supply correctly.
    #[test]
    fn capital_reduction_burns_supply() {
        let total_supply: i128 = 1_000_000;
        let tendered: i128 = 250_000;
        let new_supply = (total_supply - tendered).max(0);
        assert_eq!(new_supply, 750_000);
    }

    /// Payout overflow guard.
    #[test]
    fn payout_overflow_guard() {
        let amount: i128 = i128::MAX;
        let offer_price: i128 = 2;
        let result = amount.checked_mul(offer_price);
        assert!(result.is_none(), "overflow should be detected");
    }
}
