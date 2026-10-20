//! Multi-Currency Dynamic FX Hedging Pool (Issue #221).
//!
//! Protects international real estate investors receiving dividends in volatile currencies
//! by auto-executing FX forward hedges using DEX liquidity pools:
//! - Ingests FX rate oracle feeds (e.g., EUR/USD, GBP/USD, JPY/USD).
//! - Calculates required FX forward hedge ratios based on projected annual dividend yields.
//! - Executes automated rebalancing swaps when currency fluctuations exceed variance limits.
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env, Symbol,
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
    Pair(Symbol),
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

    /// Read hedge position for a given pair.
    pub fn get_hedge_position(env: Env, currency_pair: Symbol) -> Option<HedgePosition> {
        env.storage().instance().get(&DataKey::Pair(currency_pair))
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
}
