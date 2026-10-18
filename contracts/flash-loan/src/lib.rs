//! Cross-Asset Flash Loan Verification Engine for Compliance-Gated Tokens.
//!
//! Enables uncollateralized single-transaction flash loans for tokenized RWAs
//! while enforcing:
//! 1. **Dual-Step Compliance Verification**: Verifies that `receiver_contract`
//!    satisfies the on-chain compliance allowlist (`is_allowed`) BOTH prior to
//!    loan disbursement and upon callback return (preventing mid-loan
//!    compliance revocation or unauthorized flash receiver state transitions).
//! 2. **Reentrancy Protection**: Enforces a strict non-reentrant execution lock
//!    so nested flash loans cannot manipulate vault balance snapshots.
//! 3. **Atomic Payback + Premium Fee Enforcement**: Verifies that the vault's
//!    token balance after `on_flash_loan` is at least `pre_balance + fee`
//!    (default `9` bps = `0.09%`), reverting the entire Soroban transaction
//!    frame if principal + premium are not repaid in full.
#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype,
    panic_with_error, Address, Bytes, Env, IntoVal, Symbol, Val, Vec,
};

/// Basis points denominator (`10_000` = 100%).
pub const BPS_SCALE: i128 = 10_000;

/// Default flash loan fee in basis points (`9` bps = 0.09%).
pub const DEFAULT_FEE_BPS: u32 = 9;

#[contractclient(name = "TokenClient")]
pub trait TokenInterface {
    fn transfer(env: Env, from: Address, to: Address, amount: i128);
    fn balance(env: Env, id: Address) -> i128;
}

#[contractclient(name = "ComplianceClient")]
pub trait ComplianceInterface {
    fn is_allowed(env: Env, address: Address) -> bool;
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
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    ComplianceContract,
    Treasury,
    FeeBps,
    ReentrancyGuard,
    TotalFeesCollected(Address),
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

    /// Update the flash-loan premium fee in basis points (e.g., `9` = 0.09%).
    pub fn set_fee_bps(env: Env, admin: Address, fee_bps: u32) {
        Self::require_admin(&env, &admin);
        if fee_bps > BPS_SCALE as u32 {
            panic_with_error!(env, FlashLoanError::InvalidFeeRate);
        }
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
    }

    /// Execute an atomic, compliance-gated flash loan of `amount` units of
    /// `token_address` to `receiver_contract`.
    ///
    /// # Flow
    /// 1. Authenticate `receiver_contract` and acquire the non-reentrant lock.
    /// 2. Verify `receiver_contract` passes `ComplianceContract::is_allowed` prior to disbursement.
    /// 3. Snapshot vault balance and transfer `amount` of `token_address` to `receiver_contract`.
    /// 4. Invoke `receiver_contract.on_flash_loan(receiver_contract, token_address, amount, fee, data)`.
    /// 5. Re-verify `receiver_contract` passes `ComplianceContract::is_allowed` on the returning step.
    /// 6. Assert post-callback token balance `>= pre_balance + fee`; otherwise revert the entire frame.
    ///
    /// Returns the premium `fee` collected.
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

        // Acquire reentrancy lock before any external calls (CEI / non-reentrant guard).
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

        let fee = Self::calculate_fee(env.clone(), amount);

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

        // Record accumulated fees and release reentrancy guard.
        let prev_fees: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::TotalFeesCollected(token_address.clone()))
            .unwrap_or(0);
        let updated_fees = prev_fees
            .checked_add(fee)
            .unwrap_or_else(|| panic_with_error!(env, FlashLoanError::Overflow));
        env.storage().persistent().set(
            &DataKey::TotalFeesCollected(token_address.clone()),
            &updated_fees,
        );
        env.storage()
            .instance()
            .set(&DataKey::ReentrancyGuard, &false);

        FlashLoanExecuted {
            receiver_contract,
            token_address,
            amount,
            fee,
        }
        .publish(&env);

        fee
    }

    /// Compute the flash loan fee for `amount` (default 0.09% = 9 bps).
    /// Rounds up so non-zero loans with a positive fee rate always pay at least 1 unit.
    pub fn calculate_fee(env: Env, amount: i128) -> i128 {
        if amount <= 0 {
            panic_with_error!(env, FlashLoanError::InvalidAmount);
        }
        let fee_bps = Self::fee_bps(env.clone()) as i128;
        if fee_bps == 0 {
            return 0;
        }
        let numerator = amount
            .checked_mul(fee_bps)
            .unwrap_or_else(|| panic_with_error!(env, FlashLoanError::Overflow));
        let exact = numerator / BPS_SCALE;
        if numerator % BPS_SCALE != 0 {
            exact + 1
        } else {
            exact
        }
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

        pub fn balance(env: Env, id: Address) -> i128 {
            env.storage()
                .persistent()
                .get(&MockTokenKey::Balance(id))
                .unwrap_or(0)
        }

        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            from.require_auth();
            let from_bal = Self::balance(env.clone(), from.clone());
            assert!(from_bal >= amount, "insufficient token balance");
            let to_bal = Self::balance(env.clone(), to.clone());
            env.storage()
                .persistent()
                .set(&MockTokenKey::Balance(from), &(from_bal - amount));
            env.storage()
                .persistent()
                .set(&MockTokenKey::Balance(to), &(to_bal + amount));
        }
    }

    #[derive(Clone)]
    #[contracttype]
    enum MockCompKey {
        Allowed(Address),
    }

    #[contract]
    pub struct MockCompliance;

    #[contractimpl]
    impl MockCompliance {
        pub fn set_allowed(env: Env, addr: Address, allowed: bool) {
            env.storage()
                .persistent()
                .set(&MockCompKey::Allowed(addr), &allowed);
        }

        pub fn is_allowed(env: Env, address: Address) -> bool {
            env.storage()
                .persistent()
                .get(&MockCompKey::Allowed(address))
                .unwrap_or(false)
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
                    // Only repay principal without the 0.09% fee
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

    struct Setup {
        env: Env,
        token_id: Address,
        compliance_id: Address,
        vault_id: Address,
        receiver_id: Address,
        token: MockRwaTokenClient<'static>,
        compliance: MockComplianceClient<'static>,
        vault: FlashLoanEngineClient<'static>,
        receiver: MockFlashReceiverClient<'static>,
    }

    fn setup() -> Setup {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token_id = env.register(MockRwaToken, ());
        let compliance_id = env.register(MockCompliance, ());
        let vault_id = env.register(FlashLoanEngine, ());
        let receiver_id = env.register(MockFlashReceiver, ());

        let token = MockRwaTokenClient::new(&env, &token_id);
        let compliance = MockComplianceClient::new(&env, &compliance_id);
        let vault = FlashLoanEngineClient::new(&env, &vault_id);
        let receiver = MockFlashReceiverClient::new(&env, &receiver_id);

        vault.initialize(&admin, &compliance_id, &treasury);
        token.mint(&vault_id, &1_000_000);
        // Fund receiver with enough tokens to cover the 0.09% flash loan fee
        token.mint(&receiver_id, &10_000);

        Setup {
            env,
            token_id,
            compliance_id,
            vault_id,
            receiver_id,
            token,
            compliance,
            vault,
            receiver,
        }
    }

    #[test]
    fn successful_flash_loan_verifies_compliance_and_collects_009_pct_fee() {
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
        assert_eq!(s.token.balance(&s.vault_id), 1_000_090);
        assert_eq!(s.vault.total_fees_collected(&s.token_id), 90);
    }

    #[test]
    fn rejects_non_compliant_borrower_before_disbursement() {
        let s = setup();
        s.compliance.set_allowed(&s.receiver_id, &false);
        s.receiver
            .set_mode(&ReceiverMode::RepayInFull(s.vault_id.clone()));

        assert_eq!(
            s.vault.try_flash_loan(
                &s.receiver_id,
                &s.token_id,
                &100_000,
                &Bytes::new(&s.env),
            ),
            Err(Ok(FlashLoanError::BorrowerNotCompliant.into()))
        );
        assert_eq!(s.token.balance(&s.vault_id), 1_000_000);
    }

    #[test]
    fn reverts_when_compliance_revoked_on_return_step() {
        let s = setup();
        s.compliance.set_allowed(&s.receiver_id, &true);
        s.receiver
            .set_mode(&ReceiverMode::RevokeComplianceOnReturn(
                s.vault_id.clone(),
                s.compliance_id.clone(),
            ));

        assert_eq!(
            s.vault.try_flash_loan(
                &s.receiver_id,
                &s.token_id,
                &100_000,
                &Bytes::new(&s.env),
            ),
            Err(Ok(FlashLoanError::ReturnStepNotCompliant.into()))
        );
        // Entire transaction frame reverted: vault balance unchanged
        assert_eq!(s.token.balance(&s.vault_id), 1_000_000);
    }

    #[test]
    fn reverts_when_flash_loan_fee_not_repaid() {
        let s = setup();
        s.compliance.set_allowed(&s.receiver_id, &true);
        s.receiver
            .set_mode(&ReceiverMode::Underpay(s.vault_id.clone()));

        assert_eq!(
            s.vault.try_flash_loan(
                &s.receiver_id,
                &s.token_id,
                &100_000,
                &Bytes::new(&s.env),
            ),
            Err(Ok(FlashLoanError::FlashLoanNotRepaid.into()))
        );
        assert_eq!(s.token.balance(&s.vault_id), 1_000_000);
    }

    #[test]
    fn blocks_reentrant_flash_loan() {
        let s = setup();
        s.compliance.set_allowed(&s.receiver_id, &true);
        s.receiver
            .set_mode(&ReceiverMode::AttemptReentrancy(s.vault_id.clone()));

        assert!(s
            .vault
            .try_flash_loan(
                &s.receiver_id,
                &s.token_id,
                &100_000,
                &Bytes::new(&s.env),
            )
            .is_err());
        assert_eq!(s.token.balance(&s.vault_id), 1_000_000);
    }
}
