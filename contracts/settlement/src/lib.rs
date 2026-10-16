//! Atomic multilateral batch settlement engine (issue #316).
//!
//! Bilateral pairs in tokenized secondary markets are often illiquid. A
//! multilateral cross-trade lets N participants settle a ring of trades —
//! A pays B in a real-estate token, B pays C in a bond token, C pays A in
//! USDC — as one indivisible unit, so nobody delivers their leg without
//! receiving theirs.
//!
//! The settlement contract never takes custody. Each leg is a direct
//! `token.transfer(from, to, amount)` invoked by this contract on the
//! sender's behalf, so every sender must authorize the batch (their auth
//! entry covers this contract's root invocation and the token transfer
//! sub-invocations it makes). All validation — graph shape, conservation,
//! balances, compliance — runs before the first transfer, and the transfers
//! themselves run inside the same Soroban invocation: any panic anywhere
//! (failed auth, an underfunded sender, a token refusing a transfer) aborts
//! the transaction and rolls back every leg already applied.
//!
//! Tokens must be registered by the admin before they can appear in a batch,
//! either as *regulated* (bound to the compliance contract whose
//! `is_allowed(address) -> bool` gates every leg in that token) or explicitly
//! as *unregulated* (e.g. a USDC Stellar Asset Contract used as the cash leg).
//! An unregistered token fails closed.
#![no_std]

mod batch_settler;

#[cfg(test)]
mod test;

pub use batch_settler::{AssetPolicy, TradeLeg, MAX_LEGS};

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, Address, BytesN, Env,
    Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    Paused = 4,
    /// The batch is empty or longer than [`MAX_LEGS`].
    InvalidBatchSize = 5,
    /// A leg has a non-positive amount.
    InvalidAmount = 6,
    /// A leg sends a token from an account to itself.
    SelfTransfer = 7,
    /// A leg uses a token that has not been registered with the engine.
    AssetNotRegistered = 8,
    /// A leg is not part of any directed cycle in the trade graph, i.e. it
    /// is a one-way transfer with nothing flowing back to its sender.
    LegNotInCycle = 9,
    /// Inflows and outflows of a token do not net to zero across the batch,
    /// or an account that both receives and forwards the same token does not
    /// pass it through in full.
    ConservationViolation = 10,
    /// A sender cannot fund its legs from its current balance plus what the
    /// batch delivers to it.
    InsufficientBalance = 11,
    /// The token's compliance contract rejected a leg's sender.
    SenderNotCompliant = 12,
    /// The token's compliance contract rejected a leg's recipient.
    RecipientNotCompliant = 13,
    Overflow = 14,
}

#[derive(Clone)]
#[contracttype]
pub(crate) enum DataKey {
    Admin,
    /// Registration of a token: its [`AssetPolicy`].
    Asset(Address),
    /// Number of batches settled so far; the next batch's id.
    BatchCount,
}

#[contract]
pub struct SettlementContract;

#[contractimpl]
impl SettlementContract {
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::BatchCount, &0u64);
    }

    /// Register `token` as tradeable in batches. `compliance` is the
    /// compliance contract that gates the token; `None` registers it as an
    /// unregulated settlement asset (a cash leg such as USDC).
    #[allow(deprecated)] // Same `events().publish` style as the other Tessera contracts.
    pub fn register_asset(env: Env, admin: Address, token: Address, compliance: Option<Address>) {
        Self::require_admin(&env, &admin);
        let policy = match compliance.clone() {
            Some(c) => AssetPolicy::Regulated(c),
            None => AssetPolicy::Unregulated,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Asset(token.clone()), &policy);
        env.events()
            .publish((soroban_sdk::symbol_short!("asset_reg"), token), compliance);
    }

    #[allow(deprecated)] // Same `events().publish` style as the other Tessera contracts.
    pub fn deregister_asset(env: Env, admin: Address, token: Address) {
        Self::require_admin(&env, &admin);
        env.storage()
            .persistent()
            .remove(&DataKey::Asset(token.clone()));
        env.events()
            .publish((soroban_sdk::symbol_short!("asset_del"), token), ());
    }

    pub fn get_asset_policy(env: Env, token: Address) -> Option<AssetPolicy> {
        env.storage().persistent().get(&DataKey::Asset(token))
    }

    /// Validate and atomically execute a multilateral cross-trade.
    ///
    /// Returns the batch id. See [`batch_settler`] for the full rule set;
    /// every rule is checked before the first transfer, and any failure —
    /// including one raised by a token during execution — reverts every leg.
    #[allow(deprecated)] // Same `events().publish` style as the other Tessera contracts.
    pub fn execute_multilateral_settlement(env: Env, trade_legs: Vec<TradeLeg>) -> u64 {
        if tessera_common::is_paused(&env) {
            panic_with_error!(env, Error::Paused);
        }
        if !env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::NotInitialized);
        }

        batch_settler::settle(&env, &trade_legs);

        let batch_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::BatchCount)
            .unwrap_or(0);
        let next = batch_id
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
        env.storage().instance().set(&DataKey::BatchCount, &next);
        env.events().publish(
            (soroban_sdk::symbol_short!("settled"), batch_id),
            trade_legs.len(),
        );
        batch_id
    }

    /// Run every pre-execution check on `trade_legs` without moving funds or
    /// requiring auth. Panics with the same error the real settlement would.
    pub fn validate_settlement(env: Env, trade_legs: Vec<TradeLeg>) {
        batch_settler::validate(&env, &trade_legs);
    }

    pub fn batch_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::BatchCount)
            .unwrap_or(0)
    }

    pub fn pause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        tessera_common::set_paused(&env, true);
    }

    pub fn unpause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        tessera_common::set_paused(&env, false);
    }

    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::require_admin(&env, &admin);
        tessera_common::upgrade(&env, new_wasm_hash);
    }

    fn require_admin(env: &Env, admin: &Address) {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        admin.require_auth();
        if admin != &stored {
            panic_with_error!(env, Error::Unauthorized);
        }
    }
}
