//! On-Chain Real Estate Lease Tokenization & Automated Rental Sweep Vault.
//!
//! Represents commercial property leases as tokenized cash-flow streams that
//! automatically sweep incoming tenant stablecoin payments into fractional
//! investor payout contracts following a strict senior-debt-first waterfall:
//! 1. **Senior Debt Tranche Priority**: Incoming rent first satisfies any
//!    outstanding `senior_interest_due` owed to the senior debt tranche payout
//!    contract.
//! 2. **Fractional Equity Sweep**: Any residual rent above the senior debt
//!    interest obligation is automatically swept to the fractional equity
//!    payout contract in the same atomic transaction.
//! 3. **Standardized Audit Events**: Emits `RentSwept(lease_id, amount, tenant_address)`
//!    on every automated rental sweep.
#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype,
    panic_with_error, Address, Env, IntoVal, Symbol,
};

#[contractclient(name = "TokenClient")]
pub trait TokenInterface {
    fn transfer(env: Env, from: Address, to: Address, amount: i128);
    fn balance(env: Env, id: Address) -> i128;
}

/// Persistent state of a tokenized commercial real-estate lease.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseAgreement {
    pub lease_id: u64,
    pub tenant: Address,
    pub stablecoin: Address,
    pub senior_tranche_recipient: Address,
    pub equity_tranche_recipient: Address,
    pub monthly_rent: i128,
    pub senior_interest_due: i128,
    pub senior_interest_paid: i128,
    pub equity_distributed: i128,
    pub total_rent_collected: i128,
    pub active: bool,
}

/// Breakdown of an automated rental sweep across senior debt and equity tranches.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SweepResult {
    pub lease_id: u64,
    pub total_amount: i128,
    pub senior_payout: i128,
    pub equity_payout: i128,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum LeaseError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidAmount = 4,
    LeaseNotFound = 5,
    LeaseInactive = 6,
    UnauthorizedTenant = 7,
    Overflow = 8,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    NextLeaseId,
    Lease(u64),
}

/// Standardized event emitted whenever incoming tenant rent is swept across tranches.
#[contractevent]
pub struct RentSwept {
    #[topic]
    pub lease_id: u64,
    pub amount: i128,
    pub tenant_address: Address,
}

#[contract]
pub struct LeaseTokenVault;

#[contractimpl]
impl LeaseTokenVault {
    /// Initialize the lease tokenization & rental sweep vault.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, LeaseError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::NextLeaseId, &0u64);
    }

    /// Register a new tokenized commercial lease with senior debt priority
    /// interest obligations and fractional equity sweep routing.
    pub fn create_lease(
        env: Env,
        admin: Address,
        tenant: Address,
        stablecoin: Address,
        senior_tranche_recipient: Address,
        equity_tranche_recipient: Address,
        monthly_rent: i128,
        senior_interest_due: i128,
    ) -> u64 {
        Self::require_admin(&env, &admin);
        if monthly_rent <= 0 || senior_interest_due < 0 {
            panic_with_error!(env, LeaseError::InvalidAmount);
        }

        let lease_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextLeaseId)
            .unwrap_or(0);
        let next_id = lease_id
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(env, LeaseError::Overflow));

        let lease = LeaseAgreement {
            lease_id,
            tenant,
            stablecoin,
            senior_tranche_recipient,
            equity_tranche_recipient,
            monthly_rent,
            senior_interest_due,
            senior_interest_paid: 0,
            equity_distributed: 0,
            total_rent_collected: 0,
            active: true,
        };

        env.storage()
            .instance()
            .set(&DataKey::NextLeaseId, &next_id);
        env.storage()
            .persistent()
            .set(&DataKey::Lease(lease_id), &lease);
        lease_id
    }

    /// Record additional periodic priority interest accrued on the senior debt tranche.
    pub fn accrue_senior_interest(
        env: Env,
        admin: Address,
        lease_id: u64,
        additional_interest: i128,
    ) {
        Self::require_admin(&env, &admin);
        if additional_interest <= 0 {
            panic_with_error!(env, LeaseError::InvalidAmount);
        }

        let mut lease = Self::load_lease(&env, lease_id);
        if !lease.active {
            panic_with_error!(env, LeaseError::LeaseInactive);
        }

        lease.senior_interest_due = lease
            .senior_interest_due
            .checked_add(additional_interest)
            .unwrap_or_else(|| panic_with_error!(env, LeaseError::Overflow));

        env.storage()
            .persistent()
            .set(&DataKey::Lease(lease_id), &lease);
    }

    /// Receive a stablecoin rental payment from `tenant_address` and
    /// automatically sweep proceeds to the senior debt tranche (up to
    /// `senior_interest_due`) followed by the fractional equity payout contract.
    ///
    /// Emits `RentSwept(lease_id, amount, tenant_address)`.
    pub fn pay_rent(
        env: Env,
        lease_id: u64,
        tenant_address: Address,
        amount: i128,
    ) -> SweepResult {
        tenant_address.require_auth();
        if amount <= 0 {
            panic_with_error!(env, LeaseError::InvalidAmount);
        }

        let mut lease = Self::load_lease(&env, lease_id);
        if !lease.active {
            panic_with_error!(env, LeaseError::LeaseInactive);
        }
        if lease.tenant != tenant_address {
            panic_with_error!(env, LeaseError::UnauthorizedTenant);
        }

        // Priority 1: Senior debt interest obligation
        let senior_payout = core::cmp::min(amount, lease.senior_interest_due);
        // Priority 2: Residual cash flow swept to fractional equity holders
        let equity_payout = amount
            .checked_sub(senior_payout)
            .unwrap_or_else(|| panic_with_error!(env, LeaseError::Overflow));

        lease.senior_interest_due = lease
            .senior_interest_due
            .checked_sub(senior_payout)
            .unwrap_or_else(|| panic_with_error!(env, LeaseError::Overflow));
        lease.senior_interest_paid = lease
            .senior_interest_paid
            .checked_add(senior_payout)
            .unwrap_or_else(|| panic_with_error!(env, LeaseError::Overflow));
        lease.equity_distributed = lease
            .equity_distributed
            .checked_add(equity_payout)
            .unwrap_or_else(|| panic_with_error!(env, LeaseError::Overflow));
        lease.total_rent_collected = lease
            .total_rent_collected
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, LeaseError::Overflow));

        // CEI: persist updated lease accounting prior to external token transfers.
        env.storage()
            .persistent()
            .set(&DataKey::Lease(lease_id), &lease);

        let vault_address = env.current_contract_address();
        let token_client = TokenClient::new(&env, &lease.stablecoin);

        // Pull stablecoin payment from tenant into the sweep vault
        token_client.transfer(&tenant_address, &vault_address, &amount);

        // Automatically sweep senior tranche allocation
        if senior_payout > 0 {
            Self::transfer_from_vault(
                &env,
                &lease.stablecoin,
                &lease.senior_tranche_recipient,
                senior_payout,
            );
        }

        // Automatically sweep residual rent to fractional equity payout contract
        if equity_payout > 0 {
            Self::transfer_from_vault(
                &env,
                &lease.stablecoin,
                &lease.equity_tranche_recipient,
                equity_payout,
            );
        }

        RentSwept {
            lease_id,
            amount,
            tenant_address,
        }
        .publish(&env);

        SweepResult {
            lease_id,
            total_amount: amount,
            senior_payout,
            equity_payout,
        }
    }

    /// Deactivate a lease agreement.
    pub fn close_lease(env: Env, admin: Address, lease_id: u64) {
        Self::require_admin(&env, &admin);
        let mut lease = Self::load_lease(&env, lease_id);
        if !lease.active {
            panic_with_error!(env, LeaseError::LeaseInactive);
        }
        lease.active = false;
        env.storage()
            .persistent()
            .set(&DataKey::Lease(lease_id), &lease);
    }

    /// Retrieve the full `LeaseAgreement` state for `lease_id`.
    pub fn get_lease(env: Env, lease_id: u64) -> LeaseAgreement {
        Self::load_lease(&env, lease_id)
    }

    // ------------------------------------------------------------------
    // Internal Helpers
    // ------------------------------------------------------------------

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, LeaseError::NotInitialized));
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, LeaseError::Unauthorized);
        }
    }

    fn load_lease(env: &Env, lease_id: u64) -> LeaseAgreement {
        env.storage()
            .persistent()
            .get(&DataKey::Lease(lease_id))
            .unwrap_or_else(|| panic_with_error!(env, LeaseError::LeaseNotFound))
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
    use soroban_sdk::testutils::{Address as _, Events as _};

    #[derive(Clone)]
    #[contracttype]
    enum TokenKey {
        Balance(Address),
    }

    #[contract]
    pub struct MockStablecoin;

    #[contractimpl]
    impl MockStablecoin {
        pub fn mint(env: Env, to: Address, amount: i128) {
            let bal = Self::balance(env.clone(), to.clone());
            env.storage()
                .persistent()
                .set(&TokenKey::Balance(to), &(bal + amount));
        }

        pub fn balance(env: Env, id: Address) -> i128 {
            env.storage()
                .persistent()
                .get(&TokenKey::Balance(id))
                .unwrap_or(0)
        }

        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            from.require_auth();
            let from_bal = Self::balance(env.clone(), from.clone());
            assert!(from_bal >= amount, "insufficient balance");
            let to_bal = Self::balance(env.clone(), to.clone());
            env.storage()
                .persistent()
                .set(&TokenKey::Balance(from), &(from_bal - amount));
            env.storage()
                .persistent()
                .set(&TokenKey::Balance(to), &(to_bal + amount));
        }
    }

    struct Setup {
        env: Env,
        admin: Address,
        tenant: Address,
        senior_pool: Address,
        equity_pool: Address,
        vault_id: Address,
        vault: LeaseTokenVaultClient<'static>,
        stablecoin: MockStablecoinClient<'static>,
        stablecoin_id: Address,
    }

    fn setup() -> Setup {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let tenant = Address::generate(&env);
        let senior_pool = Address::generate(&env);
        let equity_pool = Address::generate(&env);

        let stablecoin_id = env.register(MockStablecoin, ());
        let vault_id = env.register(LeaseTokenVault, ());

        let stablecoin = MockStablecoinClient::new(&env, &stablecoin_id);
        let vault = LeaseTokenVaultClient::new(&env, &vault_id);

        vault.initialize(&admin);
        stablecoin.mint(&tenant, &500_000);

        Setup {
            env,
            admin,
            tenant,
            senior_pool,
            equity_pool,
            vault_id,
            vault,
            stablecoin,
            stablecoin_id,
        }
    }

    #[test]
    fn sweeps_senior_debt_priority_interest_before_equity_and_emits_event() {
        let s = setup();
        // Monthly rent = 10,000, Senior debt interest due = 6,000
        let lease_id = s.vault.create_lease(
            &s.admin,
            &s.tenant,
            &s.stablecoin_id,
            &s.senior_pool,
            &s.equity_pool,
            &10_000,
            &6_000,
        );

        // Partial payment of 4,000 goes 100% to senior debt tranche first
        let res1 = s.vault.pay_rent(&lease_id, &s.tenant, &4_000);
        assert_eq!(res1.senior_payout, 4_000);
        assert_eq!(res1.equity_payout, 0);
        assert_eq!(s.stablecoin.balance(&s.senior_pool), 4_000);
        assert_eq!(s.stablecoin.balance(&s.equity_pool), 0);

        // Next payment of 6,000: remaining 2,000 satisfies senior interest, 4,000 swept to equity
        let res2 = s.vault.pay_rent(&lease_id, &s.tenant, &6_000);
        assert!(!s.env.events().all().events().is_empty());
        assert_eq!(res2.senior_payout, 2_000);
        assert_eq!(res2.equity_payout, 4_000);
        assert_eq!(s.stablecoin.balance(&s.senior_pool), 6_000);
        assert_eq!(s.stablecoin.balance(&s.equity_pool), 4_000);
        assert_eq!(s.stablecoin.balance(&s.vault_id), 0);

        let lease = s.vault.get_lease(&lease_id);
        assert_eq!(lease.senior_interest_due, 0);
        assert_eq!(lease.senior_interest_paid, 6_000);
        assert_eq!(lease.equity_distributed, 4_000);
        assert_eq!(lease.total_rent_collected, 10_000);
    }

    #[test]
    fn rejects_unauthorized_tenant_or_inactive_lease() {
        let s = setup();
        let lease_id = s.vault.create_lease(
            &s.admin,
            &s.tenant,
            &s.stablecoin_id,
            &s.senior_pool,
            &s.equity_pool,
            &10_000,
            &3_000,
        );

        let imposter = Address::generate(&s.env);
        assert_eq!(
            s.vault.try_pay_rent(&lease_id, &imposter, &10_000),
            Err(Ok(LeaseError::UnauthorizedTenant.into()))
        );

        s.vault.close_lease(&s.admin, &lease_id);
        assert_eq!(
            s.vault.try_pay_rent(&lease_id, &s.tenant, &10_000),
            Err(Ok(LeaseError::LeaseInactive.into()))
        );
    }
}
