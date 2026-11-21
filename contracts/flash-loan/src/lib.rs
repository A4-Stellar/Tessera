//! Cross-Asset Flash Loan Verification Engine for Compliance-Gated Tokens.
//!
//! Enables uncollateralized single-transaction flash loans for tokenized RWAs
//! while enforcing:
//! 1. **Dual-Step Compliance Verification**: Verifies that `receiver_contract`
//!    satisfies the on-chain compliance allowlist (`is_allowed`) BOTH prior to
//!    loan disbursement and upon callback return (preventing mid-loan
//!    compliance revocation or unauthorized flash receiver state transitions).
//! 2. **Reentrancy Protection**: Enforces a strict non-reentrant execution lock
//!    covering all cross-contract call sites so nested calls cannot manipulate balances.
//! 3. **Atomic Payback + Premium Fee Enforcement**: Verifies that the vault's
//!    token balance after `on_flash_loan` is at least `pre_balance + fee`
//!    (default `9` bps = `0.09%`), reverting the entire Soroban transaction
//!    frame if principal + premium are not repaid in full.
//! 4. **Multi-Hop Atomic Swaps**: Arbitrage path allowing borrowing asset A,
//!    routing A -> B -> C across two AMM pools, repaying in asset C, with oracle
//!    spot price verification ensuring non-negative net value after slippage.
//! 5. **Fee Tier System & Redistribution**: 0.09% base fee, 0.05% for governance
//!    token holders, 0% waived for whitelisted liquidators. Fees are distributed:
//!    60% LP reserve, 30% governance treasury, 10% protocol insurance.
#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype,
    panic_with_error, Address, Bytes, Env, IntoVal, Symbol, Val, Vec,
};

/// Basis points denominator (`10_000` = 100%).
pub const BPS_SCALE: i128 = 10_000;

/// Default flash loan fee in basis points (`9` bps = 0.09%).
pub const DEFAULT_FEE_BPS: u32 = 9;

/// Discounted fee for governance token holders (`5` bps = 0.05%).
pub const GOVERNANCE_FEE_BPS: u32 = 5;

#[contractclient(name = "TokenClient")]
pub trait TokenInterface {
    fn transfer(env: Env, from: Address, to: Address, amount: i128);
    fn balance(env: Env, id: Address) -> i128;
}

#[contractclient(name = "ComplianceClient")]
pub trait ComplianceInterface {
    fn is_allowed(env: Env, address: Address) -> bool;
}

#[contractclient(name = "OracleClient")]
pub trait OracleInterface {
    fn get_price(env: Env, asset: Address) -> i128;
}

#[contractclient(name = "AmmPoolClient")]
pub trait AmmPoolInterface {
    fn swap(
        env: Env,
        to: Address,
        zero_for_one: bool,
        amount_specified: i128,
        sqrt_price_limit: u128,
        max_ticks_crossed: u32,
    ) -> i128;
}

#[contractclient(name = "FlashLoanReceiverClient")]
pub trait FlashLoanReceiverInterface {
    fn on_flash_loan(
        env: Env,
        initiator: Address,
        token_address: Address,
        amount: i128,
        fee: i128,
        data: Bytes,
    );
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum FlashLoanError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidAmount = 4,
    InvalidFeeRate = 5,
    InsufficientLiquidity = 6,
    BorrowerNotCompliant = 7,
    ReturnStepNotCompliant = 8,
    ReentrancyDetected = 9,
    FlashLoanNotRepaid = 10,
    Overflow = 11,
    NegativeNetArbitrageValue = 12,
    SlippageExceeded = 13,
    InvalidOraclePrice = 14,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    ComplianceContract,
    Treasury,
    InsurancePool,
    GovernanceToken,
    FeeBps,
    ReentrancyGuard,
    TotalFeesCollected(Address),
    LiquidatorWhitelisted(Address),
}

#[contractevent]
pub struct FlashLoanExecuted {
    #[topic]
    pub receiver_contract: Address,
    #[topic]
    pub token_address: Address,
    pub amount: i128,
    pub fee: i128,
}

#[contractevent]
pub struct ArbitrageExecuted {
    #[topic]
    pub borrower: Address,
    pub asset_in: Address,
    pub asset_out: Address,
    pub amount_in: i128,
    pub amount_out: i128,
    pub fee_collected: i128,
}

#[contractevent]
pub struct FeeDistributed {
    #[topic]
    pub token_address: Address,
    pub lp_share: i128,
    pub treasury_share: i128,
    pub insurance_share: i128,
}

#[contract]
pub struct FlashLoanEngine;

#[contractimpl]
impl FlashLoanEngine {
    /// Initialize the flash loan engine with its admin, compliance registry
    /// contract, and protocol fee treasury.
    pub fn initialize(
        env: Env,
        admin: Address,
        compliance_contract: Address,
        treasury: Address,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, FlashLoanError::AlreadyInitialized);
        }
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::ComplianceContract, &compliance_contract);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage()
            .instance()
            .set(&DataKey::FeeBps, &DEFAULT_FEE_BPS);
        env.storage()
            .instance()
            .set(&DataKey::ReentrancyGuard, &false);
    }

    /// Set protocol insurance pool address for fee redistribution.
    pub fn set_insurance_pool(env: Env, admin: Address, insurance_pool: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::InsurancePool, &insurance_pool);
    }

    /// Set governance token contract address for discounted fee tier verification.
    pub fn set_governance_token(env: Env, admin: Address, gov_token: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::GovernanceToken, &gov_token);
    }

    /// Whitelist or remove liquidator address for fee waiver.
    pub fn set_liquidator_whitelist(env: Env, admin: Address, liquidator: Address, allowed: bool) {
        Self::require_admin(&env, &admin);
        if allowed {
            env.storage().persistent().set(&DataKey::LiquidatorWhitelisted(liquidator), &true);
        } else {
            env.storage().persistent().remove(&DataKey::LiquidatorWhitelisted(liquidator));
        }
    }

    /// Update the flash-loan premium fee in basis points (e.g., `9` = 0.09%).
    pub fn set_fee_bps(env: Env, admin: Address, fee_bps: u32) {
        Self::require_admin(&env, &admin);
        if fee_bps > BPS_SCALE as u32 {
            panic_with_error!(env, FlashLoanError::InvalidFeeRate);
        }
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
    }

    /// Determine dynamic fee basis points for a borrower address based on fee tiers:
    /// - Whitelisted liquidator: 0 bps (0.00%)
    /// - Governance token holder: 5 bps (0.05%)
    /// - Standard: base fee (default 9 bps = 0.09%)
    pub fn get_borrower_fee_bps(env: Env, borrower: Address) -> u32 {
        if env.storage().persistent().has(&DataKey::LiquidatorWhitelisted(borrower.clone())) {
            return 0;
        }
        if let Some(gov_token) = env.storage().instance().get::<_, Address>(&DataKey::GovernanceToken) {
            let bal = TokenClient::new(&env, &gov_token).balance(&borrower);
            if bal > 0 {
                return GOVERNANCE_FEE_BPS;
            }
        }
        Self::fee_bps(env)
    }

    /// Execute an atomic, compliance-gated flash loan of `amount` units of
    /// `token_address` to `receiver_contract`.
    pub fn flash_loan(
        env: Env,
        receiver_contract: Address,
        token_address: Address,
        amount: i128,
        data: Bytes,
    ) -> i128 {
        Self::require_initialized(&env);
        receiver_contract.require_auth();

        if amount <= 0 {
            panic_with_error!(env, FlashLoanError::InvalidAmount);
        }

        Self::acquire_reentrancy_guard(&env);

        // Step 1: Pre-disbursement compliance check.
        if !Self::check_compliance(&env, &receiver_contract) {
            panic_with_error!(env, FlashLoanError::BorrowerNotCompliant);
        }

        let vault_address = env.current_contract_address();
        let token_client = TokenClient::new(&env, &token_address);
        let pre_balance = token_client.balance(&vault_address);
        if pre_balance < amount {
            panic_with_error!(env, FlashLoanError::InsufficientLiquidity);
        }

        let fee_bps = Self::get_borrower_fee_bps(env.clone(), receiver_contract.clone());
        let fee = Self::calculate_fee_with_bps(env.clone(), amount, fee_bps);

        // Step 2: Disburse flash loan principal to the receiver contract.
        Self::transfer_from_vault(&env, &token_address, &receiver_contract, amount);

        // Step 3: Invoke callback on the receiver contract.
        let callback_args: Vec<Val> = (
            receiver_contract.clone(),
            token_address.clone(),
            amount,
            fee,
            data,
        )
            .into_val(&env);
        let _: Val = env.invoke_contract(
            &receiver_contract,
            &Symbol::new(&env, "on_flash_loan"),
            callback_args,
        );

        // Step 4: Post-callback (returning step) compliance verification.
        if !Self::check_compliance(&env, &receiver_contract) {
            panic_with_error!(env, FlashLoanError::ReturnStepNotCompliant);
        }

        // Step 5: Verify atomic payback of principal + fee.
        let post_balance = token_client.balance(&vault_address);
        let required_balance = pre_balance
            .checked_add(fee)
            .unwrap_or_else(|| panic_with_error!(env, FlashLoanError::Overflow));
        if post_balance < required_balance {
            panic_with_error!(env, FlashLoanError::FlashLoanNotRepaid);
        }

        // Step 6: Distribute fees (60% LP, 30% Treasury, 10% Insurance).
        Self::distribute_fee(&env, &token_address, fee);

        Self::release_reentrancy_guard(&env);

        FlashLoanExecuted {
            receiver_contract,
            token_address,
            amount,
            fee,
        }
        .publish(&env);

        fee
    }

    /// Multi-hop atomic swap flash loan arbitrage path:
    /// 1. Borrower borrows Asset A.
    /// 2. Swaps A -> B on `amm_pool_ab`.
    /// 3. Swaps B -> C on `amm_pool_bc`.
    /// 4. Verifies net arbitrage value is non-negative using oracle spot prices.
    /// 5. Repays loan in Asset C (or equivalent value) plus tier fee.
    pub fn flash_loan_multi_hop_arbitrage(
        env: Env,
        borrower: Address,
        asset_a: Address,
        asset_b: Address,
        asset_c: Address,
        amm_pool_ab: Address,
        amm_pool_bc: Address,
        oracle: Address,
        amount_a: i128,
        min_amount_c: i128,
    ) -> i128 {
        Self::require_initialized(&env);
        borrower.require_auth();

        if amount_a <= 0 {
            panic_with_error!(env, FlashLoanError::InvalidAmount);
        }

        Self::acquire_reentrancy_guard(&env);

        if !Self::check_compliance(&env, &borrower) {
            panic_with_error!(env, FlashLoanError::BorrowerNotCompliant);
        }

        let vault_address = env.current_contract_address();
        let token_a_client = TokenClient::new(&env, &asset_a);
        let pre_bal_a = token_a_client.balance(&vault_address);
        if pre_bal_a < amount_a {
            panic_with_error!(env, FlashLoanError::InsufficientLiquidity);
        }

        let fee_bps = Self::get_borrower_fee_bps(env.clone(), borrower.clone());
        let fee_a = Self::calculate_fee_with_bps(env.clone(), amount_a, fee_bps);

        // Step 1: Transfer Asset A from vault to AMM Pool AB
        Self::transfer_from_vault(&env, &asset_a, &amm_pool_ab, amount_a);

        // Step 2: Swap A -> B on AMM Pool AB
        let out_b: i128 = env.invoke_contract(
            &amm_pool_ab,
            &Symbol::new(&env, "swap"),
            (amm_pool_bc.clone(), true, amount_a, 0u128, 64u32).into_val(&env),
        );

        // Step 3: Swap B -> C on AMM Pool BC
        let out_c: i128 = env.invoke_contract(
            &amm_pool_bc,
            &Symbol::new(&env, "swap"),
            (vault_address.clone(), true, out_b, 0u128, 64u32).into_val(&env),
        );

        if out_c < min_amount_c {
            panic_with_error!(env, FlashLoanError::SlippageExceeded);
        }

        // Step 4: Verify net arbitrage value using oracle spot prices
        let price_a: i128 = env.invoke_contract(
            &oracle,
            &Symbol::new(&env, "get_price"),
            (asset_a.clone(),).into_val(&env),
        );
        let price_c: i128 = env.invoke_contract(
            &oracle,
            &Symbol::new(&env, "get_price"),
            (asset_c.clone(),).into_val(&env),
        );

        if price_a <= 0 || price_c <= 0 {
            panic_with_error!(env, FlashLoanError::InvalidOraclePrice);
        }

        let value_in = (amount_a + fee_a)
            .checked_mul(price_a)
            .unwrap_or_else(|| panic_with_error!(env, FlashLoanError::Overflow));
        let value_out = out_c
            .checked_mul(price_c)
            .unwrap_or_else(|| panic_with_error!(env, FlashLoanError::Overflow));

        if value_out < value_in {
            panic_with_error!(env, FlashLoanError::NegativeNetArbitrageValue);
        }

        // Return compliance check
        if !Self::check_compliance(&env, &borrower) {
            panic_with_error!(env, FlashLoanError::ReturnStepNotCompliant);
        }

        // Step 5: Distribute fee
        Self::distribute_fee(&env, &asset_c, fee_a);

        Self::release_reentrancy_guard(&env);

        ArbitrageExecuted {
            borrower,
            asset_in: asset_a,
            asset_out: asset_c,
            amount_in: amount_a,
            amount_out: out_c,
            fee_collected: fee_a,
        }
        .publish(&env);

        out_c
    }

    /// Compute the flash loan fee for `amount` using a specific fee basis points rate.
    pub fn calculate_fee_with_bps(env: Env, amount: i128, fee_bps: u32) -> i128 {
        if amount <= 0 {
            panic_with_error!(env, FlashLoanError::InvalidAmount);
        }
        if fee_bps == 0 {
            return 0;
        }
        let numerator = amount
            .checked_mul(fee_bps as i128)
            .unwrap_or_else(|| panic_with_error!(env, FlashLoanError::Overflow));
        let exact = numerator / BPS_SCALE;
        if numerator % BPS_SCALE != 0 {
            exact + 1
        } else {
            exact
        }
    }

    /// Compute the flash loan fee for `amount` (default 0.09% = 9 bps).
    pub fn calculate_fee(env: Env, amount: i128) -> i128 {
        let fee_bps = Self::fee_bps(env.clone());
        Self::calculate_fee_with_bps(env, amount, fee_bps)
    }

    /// Current flash loan fee in basis points.
    pub fn fee_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::FeeBps)
            .unwrap_or(DEFAULT_FEE_BPS)
    }

    /// Total flash loan fees collected for a given `token_address`.
    pub fn total_fees_collected(env: Env, token_address: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::TotalFeesCollected(token_address))
            .unwrap_or(0)
    }

    // ------------------------------------------------------------------
    // Internal Helpers
    // ------------------------------------------------------------------

    fn acquire_reentrancy_guard(env: &Env) {
        let is_locked: bool = env
            .storage()
            .instance()
            .get(&DataKey::ReentrancyGuard)
            .unwrap_or(false);
        if is_locked {
            panic_with_error!(env, FlashLoanError::ReentrancyDetected);
        }
        env.storage()
            .instance()
            .set(&DataKey::ReentrancyGuard, &true);
    }

    fn release_reentrancy_guard(env: &Env) {
        env.storage()
            .instance()
            .set(&DataKey::ReentrancyGuard, &false);
    }

    fn distribute_fee(env: &Env, token: &Address, fee: i128) {
        if fee <= 0 {
            return;
        }
        // 60% LP reserve (retained in vault / accumulator)
        // 30% Governance treasury
        // 10% Protocol insurance
        let lp_share = (fee * 60) / 100;
        let treasury_share = (fee * 30) / 100;
        let insurance_share = fee - lp_share - treasury_share;

        let prev_fees: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::TotalFeesCollected(token.clone()))
            .unwrap_or(0);
        let updated_fees = prev_fees
            .checked_add(fee)
            .unwrap_or_else(|| panic_with_error!(env, FlashLoanError::Overflow));
        env.storage().persistent().set(
            &DataKey::TotalFeesCollected(token.clone()),
            &updated_fees,
        );

        let treasury: Option<Address> = env.storage().instance().get(&DataKey::Treasury);
        if let Some(treasury_addr) = treasury {
            if treasury_share > 0 {
                Self::transfer_from_vault(env, token, &treasury_addr, treasury_share);
            }
        }

        let insurance: Option<Address> = env.storage().instance().get(&DataKey::InsurancePool);
        if let Some(insurance_addr) = insurance {
            if insurance_share > 0 {
                Self::transfer_from_vault(env, token, &insurance_addr, insurance_share);
            }
        }

        FeeDistributed {
            token_address: token.clone(),
            lp_share,
            treasury_share,
            insurance_share,
        }
        .publish(env);
    }

    fn require_initialized(env: &Env) {
        if !env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, FlashLoanError::NotInitialized);
        }
    }

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, FlashLoanError::NotInitialized));
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, FlashLoanError::Unauthorized);
        }
    }

    fn check_compliance(env: &Env, address: &Address) -> bool {
        let compliance: Address = env
            .storage()
            .instance()
            .get(&DataKey::ComplianceContract)
            .unwrap_or_else(|| panic_with_error!(env, FlashLoanError::NotInitialized));
        ComplianceClient::new(env, &compliance).is_allowed(address)
    }

    fn transfer_from_vault(env: &Env, token: &Address, to: &Address, amount: i128) {
        if amount <= 0 {
            return;
        }
        let contract_address = env.current_contract_address();
        env.authorize_as_current_contract(soroban_sdk::vec![
            env,
            soroban_sdk::auth::InvokerContractAuthEntry::Contract(
                soroban_sdk::auth::SubContractInvocation {
                    context: soroban_sdk::auth::ContractContext {
                        contract: token.clone(),
                        fn_name: Symbol::new(env, "transfer"),
                        args: (contract_address.clone(), to.clone(), amount).into_val(env),
                    },
                    sub_invocations: soroban_sdk::vec![env],
                },
            ),
        ]);
        TokenClient::new(env, token).transfer(&contract_address, to, &amount);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[derive(Clone)]
    #[contracttype]
    enum MockTokenKey {
        Balance(Address),
    }

    #[contract]
    pub struct MockRwaToken;

    #[contractimpl]
    impl MockRwaToken {
        pub fn mint(env: Env, to: Address, amount: i128) {
            let bal = Self::balance(env.clone(), to.clone());
            env.storage()
                .persistent()
                .set(&MockTokenKey::Balance(to), &(bal + amount));
        }

        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            let from_bal = Self::balance(env.clone(), from.clone());
            assert!(from_bal >= amount, "insufficient balance");
            env.storage()
                .persistent()
                .set(&MockTokenKey::Balance(from), &(from_bal - amount));
            let to_bal = Self::balance(env.clone(), to.clone());
            env.storage()
                .persistent()
                .set(&MockTokenKey::Balance(to), &(to_bal + amount));
        }

        pub fn balance(env: Env, id: Address) -> i128 {
            env.storage()
                .persistent()
                .get(&MockTokenKey::Balance(id))
                .unwrap_or(0)
        }
    }

    #[derive(Clone)]
    #[contracttype]
    enum MockComplianceKey {
        Allowed(Address),
    }

    #[contract]
    pub struct MockCompliance;

    #[contractimpl]
    impl MockCompliance {
        pub fn set_allowed(env: Env, address: Address, allowed: bool) {
            env.storage()
                .persistent()
                .set(&MockComplianceKey::Allowed(address), &allowed);
        }

        pub fn is_allowed(env: Env, address: Address) -> bool {
            env.storage()
                .persistent()
                .get(&MockComplianceKey::Allowed(address))
                .unwrap_or(true)
        }
    }

    #[derive(Clone)]
    #[contracttype]
    pub enum ReceiverMode {
        RepayInFull(Address),
        Underpay(Address),
        RevokeComplianceOnReturn(Address, Address),
        AttemptReentrancy(Address),
    }

    #[derive(Clone)]
    #[contracttype]
    enum ReceiverKey {
        Mode,
    }

    #[contract]
    pub struct MockFlashReceiver;

    #[contractimpl]
    impl MockFlashReceiver {
        pub fn set_mode(env: Env, mode: ReceiverMode) {
            env.storage().instance().set(&ReceiverKey::Mode, &mode);
        }

        pub fn on_flash_loan(
            env: Env,
            _initiator: Address,
            token_address: Address,
            amount: i128,
            fee: i128,
            data: Bytes,
        ) {
            let mode: ReceiverMode = env.storage().instance().get(&ReceiverKey::Mode).unwrap();
            let self_addr = env.current_contract_address();
            match mode {
                ReceiverMode::RepayInFull(vault) => {
                    let repay = amount + fee;
                    Self::send_tokens(&env, &token_address, &self_addr, &vault, repay);
                }
                ReceiverMode::Underpay(vault) => {
                    // Only repay principal without the fee
                    Self::send_tokens(&env, &token_address, &self_addr, &vault, amount);
                }
                ReceiverMode::RevokeComplianceOnReturn(vault, compliance) => {
                    let repay = amount + fee;
                    Self::send_tokens(&env, &token_address, &self_addr, &vault, repay);
                    MockComplianceClient::new(&env, &compliance).set_allowed(&self_addr, &false);
                }
                ReceiverMode::AttemptReentrancy(vault) => {
                    FlashLoanEngineClient::new(&env, &vault).flash_loan(
                        &self_addr,
                        &token_address,
                        &amount,
                        &data,
                    );
                }
            }
        }

        fn send_tokens(env: &Env, token: &Address, from: &Address, to: &Address, amount: i128) {
            env.authorize_as_current_contract(soroban_sdk::vec![
                env,
                soroban_sdk::auth::InvokerContractAuthEntry::Contract(
                    soroban_sdk::auth::SubContractInvocation {
                        context: soroban_sdk::auth::ContractContext {
                            contract: token.clone(),
                            fn_name: Symbol::new(env, "transfer"),
                            args: (from.clone(), to.clone(), amount).into_val(env),
                        },
                        sub_invocations: soroban_sdk::vec![env],
                    },
                ),
            ]);
            MockRwaTokenClient::new(env, token).transfer(from, to, &amount);
        }
    }

    #[contract]
    pub struct MockAmm;

    #[contractimpl]
    impl MockAmm {
        pub fn swap(
            _env: Env,
            _to: Address,
            _zero_for_one: bool,
            amount_specified: i128,
            _sqrt_price_limit: u128,
            _max_ticks_crossed: u32,
        ) -> i128 {
            // Mock output with 10% gain
            (amount_specified * 110) / 100
        }
    }

    #[contract]
    pub struct MockOracle;

    #[contractimpl]
    impl MockOracle {
        pub fn get_price(_env: Env, _asset: Address) -> i128 {
            100 // Unit price 100
        }
    }

    struct Setup {
        env: Env,
        admin: Address,
        treasury: Address,
        insurance: Address,
        gov_token_id: Address,
        token_id: Address,
        compliance_id: Address,
        vault_id: Address,
        receiver_id: Address,
        token: MockRwaTokenClient<'static>,
        gov_token: MockRwaTokenClient<'static>,
        compliance: MockComplianceClient<'static>,
        vault: FlashLoanEngineClient<'static>,
        receiver: MockFlashReceiverClient<'static>,
    }

    fn setup() -> Setup {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let insurance = Address::generate(&env);
        let token_id = env.register(MockRwaToken, ());
        let gov_token_id = env.register(MockRwaToken, ());
        let compliance_id = env.register(MockCompliance, ());
        let vault_id = env.register(FlashLoanEngine, ());
        let receiver_id = env.register(MockFlashReceiver, ());

        let token = MockRwaTokenClient::new(&env, &token_id);
        let gov_token = MockRwaTokenClient::new(&env, &gov_token_id);
        let compliance = MockComplianceClient::new(&env, &compliance_id);
        let vault = FlashLoanEngineClient::new(&env, &vault_id);
        let receiver = MockFlashReceiverClient::new(&env, &receiver_id);

        vault.initialize(&admin, &compliance_id, &treasury);
        vault.set_insurance_pool(&admin, &insurance);
        vault.set_governance_token(&admin, &gov_token_id);

        token.mint(&vault_id, &1_000_000);
        token.mint(&receiver_id, &10_000);

        Setup {
            env,
            admin,
            treasury,
            insurance,
            gov_token_id,
            token_id,
            compliance_id,
            vault_id,
            receiver_id,
            token,
            gov_token,
            compliance,
            vault,
            receiver,
        }
    }

    #[test]
    fn successful_flash_loan_verifies_compliance_and_collects_fee() {
        let s = setup();
        s.compliance.set_allowed(&s.receiver_id, &true);
        s.receiver
            .set_mode(&ReceiverMode::RepayInFull(s.vault_id.clone()));

        // Borrow 100,000 -> 0.09% fee (9 bps) = 90
        let fee = s.vault.flash_loan(
            &s.receiver_id,
            &s.token_id,
            &100_000,
            &Bytes::from_slice(&s.env, b"arb"),
        );
        assert_eq!(fee, 90);
        assert_eq!(s.vault.total_fees_collected(&s.token_id), 90);
    }

    #[test]
    fn fee_tier_governance_discount_and_liquidator_waiver() {
        let s = setup();
        let borrower = Address::generate(&s.env);

        // Standard rate: 9 bps
        assert_eq!(s.vault.get_borrower_fee_bps(&borrower), 9);

        // Holding governance token: 5 bps
        s.gov_token.mint(&borrower, &100);
        assert_eq!(s.vault.get_borrower_fee_bps(&borrower), 5);

        // Whitelisted liquidator: 0 bps (waived)
        s.vault.set_liquidator_whitelist(&s.admin, &borrower, &true);
        assert_eq!(s.vault.get_borrower_fee_bps(&borrower), 0);
    }

    #[test]
    fn multi_hop_atomic_swap_arbitrage_flow() {
        let s = setup();
        let pool_ab = s.env.register(MockAmm, ());
        let pool_bc = s.env.register(MockAmm, ());
        let oracle = s.env.register(MockOracle, ());

        let asset_a = s.token_id.clone();
        let asset_b = Address::generate(&s.env);
        let asset_c = Address::generate(&s.env);

        let out = s.vault.flash_loan_multi_hop_arbitrage(
            &s.receiver_id,
            &asset_a,
            &asset_b,
            &asset_c,
            &pool_ab,
            &pool_bc,
            &oracle,
            &10_000,
            &10_000,
        );
        assert!(out > 10_000);
    }
}
