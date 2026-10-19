//! Immutable 4-of-6 emergency settlement vault. Activation atomically funds
//! the reserve, snapshots supply and permanently freezes the asset token.
//! Each holder receives floor(cumulative_burn * reserve / original_supply),
//! less prior payments, so splitting claims cannot change their entitlement.
use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, panic_with_error,
    symbol_short, token, Address, Env, Vec, U256,
};

#[contractclient(name = "AssetClient")]
#[allow(dead_code)]
trait Asset {
    fn token_admin(env: Env) -> Address;
    fn total_supply(env: Env) -> i128;
    fn activate_liquidation(env: Env, vault: Address);
    fn liquidation_burn(
        env: Env,
        vault: Address,
        holder: Address,
        amount: i128,
        share_class: Option<u32>,
    );
}

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum LiquidationError {
    InvalidCommittee = 100,
    DuplicateSigner = 101,
    NotCommittee = 102,
    InsufficientSignatures = 103,
    AlreadyActive = 104,
    NotActive = 105,
    InvalidAmount = 106,
    Overflow = 107,
    InvalidToken = 108,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidationState {
    pub asset_token: Address,
    pub stablecoin: Address,
    pub trustees: Vec<Address>,
    pub representatives: Vec<Address>,
    pub active: bool,
    pub initial_supply: i128,
    pub proceeds: i128,
    pub burned: i128,
    pub paid: i128,
}

#[contracttype]
enum VaultKey {
    EmergencyState,
    EmergencyRedeemed(Address),
}

#[contract]
pub struct EmergencyLiquidationVault;

#[contractimpl]
impl EmergencyLiquidationVault {
    /// Deploy atomically with the asset administrator's authorization.
    /// The two groups must together contain six distinct members, with at
    /// least one court trustee and one issuer representative.
    pub fn __constructor(
        env: Env,
        asset_token: Address,
        stablecoin: Address,
        trustees: Vec<Address>,
        representatives: Vec<Address>,
    ) {
        AssetClient::new(&env, &asset_token)
            .token_admin()
            .require_auth();
        if asset_token == stablecoin || stablecoin == env.current_contract_address() {
            panic_with_error!(env, LiquidationError::InvalidToken);
        }
        if trustees.is_empty()
            || representatives.is_empty()
            || trustees.len() + representatives.len() != 6
        {
            panic_with_error!(env, LiquidationError::InvalidCommittee);
        }
        let mut members = Vec::new(&env);
        for member in trustees.iter().chain(representatives.iter()) {
            if members.contains(&member) {
                panic_with_error!(env, LiquidationError::DuplicateSigner);
            }
            members.push_back(member);
        }
        env.storage().instance().set(
            &VaultKey::EmergencyState,
            &LiquidationState {
                asset_token,
                stablecoin,
                trustees,
                representatives,
                active: false,
                initial_supply: 0,
                proceeds: 0,
                burned: 0,
                paid: 0,
            },
        );
    }

    /// Four distinct committee signatures on this invocation, plus the
    /// funder's authorization of the actual stablecoin transfer. The
    /// original supply and reserve can never be reset or increased later.
    pub fn activate(env: Env, signers: Vec<Address>, funder: Address, proceeds: i128) {
        let mut state = Self::liquidation_state(env.clone());
        if state.active {
            panic_with_error!(env, LiquidationError::AlreadyActive);
        }
        if signers.len() < 4 || signers.len() > 6 {
            panic_with_error!(env, LiquidationError::InsufficientSignatures);
        }
        let mut seen = Vec::new(&env);
        for signer in signers.iter() {
            if !state.trustees.contains(&signer) && !state.representatives.contains(&signer) {
                panic_with_error!(env, LiquidationError::NotCommittee);
            }
            if seen.contains(&signer) {
                panic_with_error!(env, LiquidationError::DuplicateSigner);
            }
            signer.require_auth();
            seen.push_back(signer);
        }
        funder.require_auth();
        let asset = AssetClient::new(&env, &state.asset_token);
        let supply = asset.total_supply();
        if proceeds <= 0 || supply <= 0 {
            panic_with_error!(env, LiquidationError::InvalidAmount);
        }
        state.active = true;
        state.initial_supply = supply;
        state.proceeds = proceeds;
        env.storage()
            .instance()
            .set(&VaultKey::EmergencyState, &state);
        let me = env.current_contract_address();
        asset.activate_liquidation(&me);
        token::Client::new(&env, &state.stablecoin).transfer(&funder, &me, &proceeds);
        env.events()
            .publish((symbol_short!("activated"),), (supply, proceeds, signers));
    }

    /// Class None for single-class tokens; Some(0) for A, Some(1) for B.
    /// Redemption bypasses trading compliance and time locks: a frozen or
    /// expired KYC record must not prevent court-ordered settlement.
    pub fn redeem(env: Env, holder: Address, amount: i128, share_class: Option<u32>) -> i128 {
        holder.require_auth();
        let mut state = Self::liquidation_state(env.clone());
        if !state.active {
            panic_with_error!(env, LiquidationError::NotActive);
        }
        if amount <= 0 {
            panic_with_error!(env, LiquidationError::InvalidAmount);
        }
        let key = VaultKey::EmergencyRedeemed(holder.clone());
        let previous: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let cumulative = previous
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, LiquidationError::Overflow));
        state.burned = state
            .burned
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, LiquidationError::Overflow));
        if state.burned > state.initial_supply {
            panic_with_error!(env, LiquidationError::InvalidAmount);
        }
        let payout = entitlement(&env, cumulative, state.proceeds, state.initial_supply)
            - entitlement(&env, previous, state.proceeds, state.initial_supply);
        state.paid = state
            .paid
            .checked_add(payout)
            .unwrap_or_else(|| panic_with_error!(env, LiquidationError::Overflow));
        env.storage().persistent().set(&key, &cumulative);
        env.storage()
            .instance()
            .set(&VaultKey::EmergencyState, &state);
        let me = env.current_contract_address();
        AssetClient::new(&env, &state.asset_token).liquidation_burn(
            &me,
            &holder,
            &amount,
            &share_class,
        );
        if payout > 0 {
            token::Client::new(&env, &state.stablecoin).transfer(&me, &holder, &payout);
        }
        env.events()
            .publish((symbol_short!("settled"), holder), (amount, payout));
        payout
    }

    pub fn liquidation_state(env: Env) -> LiquidationState {
        env.storage()
            .instance()
            .get(&VaultKey::EmergencyState)
            .unwrap()
    }

    pub fn vault_asset(env: Env) -> Address {
        Self::liquidation_state(env).asset_token
    }

    pub fn redeemed(env: Env, holder: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&VaultKey::EmergencyRedeemed(holder))
            .unwrap_or(0)
    }
}

fn entitlement(env: &Env, units: i128, proceeds: i128, supply: i128) -> i128 {
    let result = U256::from_u128(env, units as u128)
        .mul(&U256::from_u128(env, proceeds as u128))
        .div(&U256::from_u128(env, supply as u128))
        .to_u128()
        .unwrap_or_else(|| panic_with_error!(env, LiquidationError::Overflow));
    i128::try_from(result).unwrap_or_else(|_| panic_with_error!(env, LiquidationError::Overflow))
}

#[cfg(test)]
#[path = "emergency_liquidation/tests.rs"]
mod tests;
