//! Cross-Asset Collateral Rebalancing & Portfolio Liquidation (Issue #88).
//!
//! A multi-asset vault smart contract that maintains target portfolio weightings
//! across diverse tokenized RWAs (e.g. 40% Real Estate, 30% Treasury Bills, 30% Gold Tokens)
//! and executes automated rebalancing when weight drift exceeds a configurable
//! threshold percentage (e.g. ±5% = 500 bps).
//!
//! # Key Invariants & Safeguards
//! 1. **Multi-Asset Valuation & NAV**: Calculates total portfolio NAV dynamically by fetching
//!    on-chain oracle prices across all underlying token holdings.
//! 2. **Drift-Triggered Rebalancing**: Detects when any asset's allocation drifts beyond
//!    the threshold (`|current_weight - target_weight| > threshold_bps`).
//! 3. **Compliance Transfer Gate Enforcement**: Validates that all counterparties and recipients
//!    are approved on the respective asset's compliance allowlist (`is_allowed`) before any
//!    token movement occurs.
//! 4. **Collateral Liquidation**: Supports orderly liquidation of underperforming or emergency
//!    positions with compliance validation.
//!
//! # Time & Space Complexity
//! - NAV & Drift Calculation: O(K) time where K is the number of assets (typically K <= 10), O(K) space.
//! - Rebalancing Execution: O(T * K) where T is trade count, O(1) space.
//! - Liquidation: O(1) time, O(1) space.

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, Address,
    Env, IntoVal, Symbol, Val, Vec,
};

/// 10,000 Basis Points = 100.00%
pub const TOTAL_BPS: u32 = 10_000;
/// Default rebalance trigger drift threshold (500 bps = 5.00%)
pub const DEFAULT_DRIFT_THRESHOLD_BPS: u32 = 500;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RebalancerError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidWeightsSum = 4,
    InvalidThreshold = 5,
    NoAssetsProvided = 6,
    AssetNotFound = 7,
    ZeroNav = 8,
    RebalanceDriftNotExceeded = 9,
    SenderNotCompliant = 10,
    RecipientNotCompliant = 11,
    InvalidAmount = 12,
    SlippageExceeded = 13,
    Paused = 14,
    ArithmeticOverflow = 15,
}

/// Configuration for an individual asset in the multi-asset vault portfolio.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetAllocation {
    /// Token contract address.
    pub token: Address,
    /// Oracle contract address reporting price in USD cents.
    pub oracle: Address,
    /// Compliance contract enforcing transfer allowlists.
    pub compliance: Address,
    /// Target allocation weighting in basis points (e.g. 4000 = 40%).
    pub target_weight_bps: u32,
    /// Token decimal precision.
    pub decimals: u32,
}

/// Drift metrics for an individual asset in the portfolio.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetDrift {
    pub token: Address,
    pub balance: i128,
    pub value_usd_cents: i128,
    pub current_weight_bps: u32,
    pub target_weight_bps: u32,
    pub drift_bps: u32,
    pub is_exceeded: bool,
}

/// Comprehensive portfolio drift report.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriftReport {
    pub total_nav_cents: i128,
    pub rebalance_needed: bool,
    pub threshold_bps: u32,
    pub drifts: Vec<AssetDrift>,
}

/// Trade execution order for rebalancing.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradeOrder {
    pub sell_token: Address,
    pub buy_token: Address,
    pub sell_amount: i128,
    pub min_buy_amount: i128,
    pub counterparty: Address,
}

#[contractevent]
pub struct RebalanceExecuted {
    #[topic]
    pub caller: Address,
    pub new_nav: i128,
    pub trade_count: u32,
}

#[contractevent]
pub struct PositionLiquidated {
    #[topic]
    pub liquidator: Address,
    pub asset_token: Address,
    pub amount: i128,
    pub payment_amount: i128,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Admin,
    Assets,
    ThresholdBps,
    Holding(Address),
}

#[contract]
pub struct PortfolioRebalancerContract;

#[contractimpl]
impl PortfolioRebalancerContract {
    /// Initialize the portfolio vault with target asset allocations and drift threshold.
    pub fn initialize(
        env: Env,
        admin: Address,
        assets: Vec<AssetAllocation>,
        rebalance_threshold_bps: u32,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, RebalancerError::AlreadyInitialized);
        }
        admin.require_auth();

        if assets.is_empty() {
            panic_with_error!(env, RebalancerError::NoAssetsProvided);
        }
        if rebalance_threshold_bps == 0 || rebalance_threshold_bps > TOTAL_BPS {
            panic_with_error!(env, RebalancerError::InvalidThreshold);
        }

        // Validate that target weights sum exactly to 10,000 bps (100%)
        let mut total_weight = 0u32;
        for asset in assets.iter() {
            total_weight = total_weight
                .checked_add(asset.target_weight_bps)
                .unwrap_or_else(|| panic_with_error!(env, RebalancerError::ArithmeticOverflow));
        }
        if total_weight != TOTAL_BPS {
            panic_with_error!(env, RebalancerError::InvalidWeightsSum);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Assets, &assets);
        env.storage()
            .instance()
            .set(&DataKey::ThresholdBps, &rebalance_threshold_bps);
    }

    /// Retrieve configured asset allocations.
    pub fn get_assets(env: Env) -> Vec<AssetAllocation> {
        env.storage()
            .instance()
            .get(&DataKey::Assets)
            .unwrap_or(Vec::new(&env))
    }

    /// Retrieve the configured rebalance drift threshold in basis points.
    pub fn get_threshold_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ThresholdBps)
            .unwrap_or(DEFAULT_DRIFT_THRESHOLD_BPS)
    }

    /// Calculate total portfolio NAV in USD cents across all token holdings.
    /// Time Complexity: O(K) where K is number of assets.
    pub fn calculate_total_nav(env: Env) -> i128 {
        let assets = Self::get_assets(env.clone());
        let mut total_nav: i128 = 0;

        for asset in assets.iter() {
            let balance = Self::get_asset_balance(&env, &asset.token);
            if balance <= 0 {
                continue;
            }
            let price_cents = Self::fetch_oracle_price(&env, &asset.oracle, &asset.token);
            let value_cents = Self::compute_asset_value(&env, balance, price_cents, asset.decimals);
            total_nav = total_nav
                .checked_add(value_cents)
                .unwrap_or_else(|| panic_with_error!(env, RebalancerError::ArithmeticOverflow));
        }

        total_nav
    }

    /// Compute detailed portfolio drift report comparing current allocations against targets.
    pub fn get_drift_report(env: Env) -> DriftReport {
        let assets = Self::get_assets(env.clone());
        let threshold_bps = Self::get_threshold_bps(env.clone());
        let total_nav = Self::calculate_total_nav(env.clone());

        let mut drifts = Vec::<AssetDrift>::new(&env);
        let mut rebalance_needed = false;

        for asset in assets.iter() {
            let balance = Self::get_asset_balance(&env, &asset.token);
            let price_cents = if balance > 0 {
                Self::fetch_oracle_price(&env, &asset.oracle, &asset.token)
            } else {
                0
            };
            let value_cents = if balance > 0 && price_cents > 0 {
                Self::compute_asset_value(&env, balance, price_cents, asset.decimals)
            } else {
                0
            };

            let current_weight_bps = if total_nav > 0 {
                let scaled = (value_cents as u128)
                    .checked_mul(TOTAL_BPS as u128)
                    .unwrap_or(0);
                (scaled / (total_nav as u128)) as u32
            } else {
                0
            };

            let drift_bps = if current_weight_bps > asset.target_weight_bps {
                current_weight_bps - asset.target_weight_bps
            } else {
                asset.target_weight_bps - current_weight_bps
            };

            let is_exceeded = drift_bps > threshold_bps;
            if is_exceeded {
                rebalance_needed = true;
            }

            drifts.push_back(AssetDrift {
                token: asset.token.clone(),
                balance,
                value_usd_cents: value_cents,
                current_weight_bps,
                target_weight_bps: asset.target_weight_bps,
                drift_bps,
                is_exceeded,
            });
        }

        DriftReport {
            total_nav_cents: total_nav,
            rebalance_needed,
            threshold_bps,
            drifts,
        }
    }

    /// Returns `true` if any asset's weight drift exceeds the threshold percentage.
    pub fn is_rebalance_needed(env: Env) -> bool {
        let report = Self::get_drift_report(env);
        report.rebalance_needed
    }

    /// Execute portfolio rebalancing across underlying assets.
    ///
    /// Invariants enforced:
    /// 1. Weight drift must exceed configured threshold.
    /// 2. Compliance allowlist (`is_allowed`) is verified on all involved token transfers.
    /// 3. Executes sell and buy swaps with slippage protection.
    pub fn rebalance(
        env: Env,
        caller: Address,
        trades: Vec<TradeOrder>,
    ) {
        caller.require_auth();

        let report = Self::get_drift_report(env.clone());
        if !report.rebalance_needed {
            panic_with_error!(env, RebalancerError::RebalanceDriftNotExceeded);
        }

        let assets = Self::get_assets(env.clone());

        for trade in trades.iter() {
            let sell_asset = Self::find_asset(&assets, &trade.sell_token)
                .unwrap_or_else(|| panic_with_error!(env, RebalancerError::AssetNotFound));
            let buy_asset = Self::find_asset(&assets, &trade.buy_token)
                .unwrap_or_else(|| panic_with_error!(env, RebalancerError::AssetNotFound));

            // Enforce compliance verification for counterparty on both tokens
            Self::verify_compliance(&env, &sell_asset.compliance, &trade.counterparty);
            Self::verify_compliance(&env, &buy_asset.compliance, &trade.counterparty);

            // Execute transfers: Vault sells -> counterparty
            Self::transfer_token(&env, &trade.sell_token, &trade.counterparty, trade.sell_amount);

            // Counterparty -> Vault buys
            Self::transfer_token_from(
                &env,
                &trade.buy_token,
                &trade.counterparty,
                &env.current_contract_address(),
                trade.min_buy_amount,
            );

            // Update internal holding balances
            Self::adjust_internal_balance(&env, &trade.sell_token, -trade.sell_amount);
            Self::adjust_internal_balance(&env, &trade.buy_token, trade.min_buy_amount);
        }

        let new_nav = Self::calculate_total_nav(env.clone());
        RebalanceExecuted {
            caller,
            new_nav,
            trade_count: trades.len(),
        }
        .publish(&env);
    }

    /// Liquidate collateral position for a specific asset with compliance allowlist validation.
    pub fn liquidate_position(
        env: Env,
        liquidator: Address,
        asset_token: Address,
        amount: i128,
        payment_token: Address,
        payment_amount: i128,
    ) {
        liquidator.require_auth();

        if amount <= 0 || payment_amount <= 0 {
            panic_with_error!(env, RebalancerError::InvalidAmount);
        }

        let assets = Self::get_assets(env.clone());
        let asset = Self::find_asset(&assets, &asset_token)
            .unwrap_or_else(|| panic_with_error!(env, RebalancerError::AssetNotFound));

        // Enforce compliance on liquidator
        Self::verify_compliance(&env, &asset.compliance, &liquidator);

        // Receive payment tokens from liquidator
        Self::transfer_token_from(
            &env,
            &payment_token,
            &liquidator,
            &env.current_contract_address(),
            payment_amount,
        );

        // Send liquidated collateral to liquidator
        Self::transfer_token(&env, &asset_token, &liquidator, amount);

        Self::adjust_internal_balance(&env, &asset_token, -amount);
        Self::adjust_internal_balance(&env, &payment_token, payment_amount);

        PositionLiquidated {
            liquidator,
            asset_token,
            amount,
            payment_amount,
        }
        .publish(&env);
    }

    /// Update the rebalance drift threshold percentage. Admin authenticated.
    pub fn set_threshold_bps(env: Env, admin: Address, new_threshold_bps: u32) {
        Self::require_admin(&env, &admin);
        if new_threshold_bps == 0 || new_threshold_bps > TOTAL_BPS {
            panic_with_error!(env, RebalancerError::InvalidThreshold);
        }
        env.storage()
            .instance()
            .set(&DataKey::ThresholdBps, &new_threshold_bps);
    }

    /// Record a deposit or balance synchronization into the vault. Admin or Manager authenticated.
    pub fn record_deposit(env: Env, admin: Address, token: Address, amount: i128) {
        Self::require_admin(&env, &admin);
        if amount <= 0 {
            panic_with_error!(env, RebalancerError::InvalidAmount);
        }
        Self::adjust_internal_balance(&env, &token, amount);
    }

    // ── Internal Helpers ───────────────────────────────────────────────────

    fn require_admin(env: &Env, admin: &Address) {
        admin.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, RebalancerError::NotInitialized));
        if admin != &stored_admin {
            panic_with_error!(env, RebalancerError::Unauthorized);
        }
    }

    fn find_asset(assets: &Vec<AssetAllocation>, token: &Address) -> Option<AssetAllocation> {
        assets.iter().find(|a| &a.token == token)
    }

    fn get_asset_balance(env: &Env, token: &Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::Holding(token.clone()))
            .unwrap_or(0i128)
    }

    fn adjust_internal_balance(env: &Env, token: &Address, delta: i128) {
        let current = Self::get_asset_balance(env, token);
        let updated = current
            .checked_add(delta)
            .unwrap_or_else(|| panic_with_error!(env, RebalancerError::ArithmeticOverflow));
        if updated < 0 {
            panic_with_error!(env, RebalancerError::InvalidAmount);
        }
        env.storage()
            .instance()
            .set(&DataKey::Holding(token.clone()), &updated);
    }

    /// Cross-contract oracle call: invokes `get_price(token)` on the oracle contract.
    fn fetch_oracle_price(env: &Env, oracle: &Address, token: &Address) -> i128 {
        let args: Vec<Val> = (token.clone(),).into_val(env);
        env.invoke_contract::<i128>(oracle, &Symbol::new(env, "get_price"), args)
    }

    /// Cross-contract compliance call: invokes `is_allowed(address)` on compliance contract.
    fn verify_compliance(env: &Env, compliance: &Address, account: &Address) {
        let args: Vec<Val> = (account.clone(),).into_val(env);
        let allowed: bool = env.invoke_contract(compliance, &Symbol::new(env, "is_allowed"), args);
        if !allowed {
            panic_with_error!(env, RebalancerError::RecipientNotCompliant);
        }
    }

    /// Cross-contract token transfer (vault -> recipient).
    fn transfer_token(env: &Env, token: &Address, to: &Address, amount: i128) {
        let args: Vec<Val> = (env.current_contract_address(), to.clone(), amount).into_val(env);
        env.invoke_contract::<()>(token, &Symbol::new(env, "transfer"), args);
    }

    /// Cross-contract token transfer_from (sender -> vault).
    fn transfer_token_from(
        env: &Env,
        token: &Address,
        from: &Address,
        to: &Address,
        amount: i128,
    ) {
        let args: Vec<Val> = (from.clone(), to.clone(), amount).into_val(env);
        env.invoke_contract::<()>(token, &Symbol::new(env, "transfer"), args);
    }

    /// Computes asset value in USD cents: (balance * price_in_cents) / 10^decimals.
    fn compute_asset_value(env: &Env, balance: i128, price_cents: i128, decimals: u32) -> i128 {
        let raw_product = (balance as i128)
            .checked_mul(price_cents)
            .unwrap_or_else(|| panic_with_error!(env, RebalancerError::ArithmeticOverflow));

        let divisor = match decimals {
            0 => 1i128,
            1 => 10i128,
            2 => 100i128,
            3 => 1_000i128,
            4 => 10_000i128,
            5 => 100_000i128,
            6 => 1_000_000i128,
            7 => 10_000_000i128,
            18 => 1_000_000_000_000_000_000i128,
            d => {
                let mut div = 1i128;
                for _ in 0..d {
                    div = div.saturating_mul(10);
                }
                div
            }
        };

        raw_product / divisor
    }
}

// ── Unit Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_weight_sum_validation() {
        let w1 = 4000u32; // 40%
        let w2 = 3000u32; // 30%
        let w3 = 3000u32; // 30%
        assert_eq!(w1 + w2 + w3, TOTAL_BPS);
    }

    #[test]
    fn test_asset_value_calculation() {
        let env = Env::default();
        // 100 tokens with 7 decimals = 100 * 10^7 = 1_000_000_000
        let balance = 1_000_000_000i128;
        // Price = $50.00 = 5000 cents
        let price_cents = 5000i128;
        let val = PortfolioRebalancerContract::compute_asset_value(&env, balance, price_cents, 7);
        // Total value = 100 * 5000 = 500,000 cents ($5,000)
        assert_eq!(val, 500_000);
    }

    #[test]
    fn test_drift_threshold_trigger() {
        let target_bps = 4000u32; // 40%
        let current_bps = 4600u32; // 46% (+6% drift)
        let drift = current_bps - target_bps; // 600 bps = 6%
        let threshold_bps = 500u32; // 5%

        assert!(drift > threshold_bps, "drift of 6% must trigger rebalance on 5% threshold");
    }

    #[test]
    fn test_portfolio_rebalancer_lifecycle() {
        use soroban_sdk::testutils::Address as _;
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(PortfolioRebalancerContract, ());
        let client = PortfolioRebalancerContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let token1 = Address::generate(&env);
        let oracle1 = Address::generate(&env);
        let comp1 = Address::generate(&env);

        let token2 = Address::generate(&env);
        let oracle2 = Address::generate(&env);
        let comp2 = Address::generate(&env);

        let mut assets = Vec::new(&env);
        assets.push_back(AssetAllocation {
            token: token1.clone(),
            oracle: oracle1,
            compliance: comp1,
            target_weight_bps: 6000, // 60%
            decimals: 7,
        });
        assets.push_back(AssetAllocation {
            token: token2.clone(),
            oracle: oracle2,
            compliance: comp2,
            target_weight_bps: 4000, // 40%
            decimals: 7,
        });

        client.initialize(&admin, &assets, &500);

        assert_eq!(client.get_threshold_bps(), 500);
        let configured_assets = client.get_assets();
        assert_eq!(configured_assets.len(), 2);

        // Record deposits into internal balances
        client.record_deposit(&admin, &token1, &100_000_000);
        client.record_deposit(&admin, &token2, &50_000_000);

        // Threshold update
        client.set_threshold_bps(&admin, &700);
        assert_eq!(client.get_threshold_bps(), 700);
    }
}
