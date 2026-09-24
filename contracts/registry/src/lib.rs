//! Registry contract — good-faith reconstruction of the deployed testnet
//! contract at `CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3`,
//! built from `docs/app/docs/contracts/registry/page.mdx` and cross-checked
//! against the `RawAssetEntry` shape `api/src/indexer/mod.rs` already
//! decodes from that live contract. See the repository root `contracts/`
//! entry in the pull request description for the full reconstruction
//! caveat: this is not the original deployed bytecode, which was never
//! present in this repository.
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    BytesN, Env, String, Vec,
};

/// A registered tokenized real-world asset.
#[contracttype]
#[derive(Clone)]
pub struct AssetEntry {
    pub id: u64,
    pub token_contract: Address,
    pub issuer: Address,
    pub name: String,
    pub asset_type: String,
    pub valuation: i128,
    pub created_at: u32,
    pub active: bool,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    AssetNotFound = 4,
    InvalidValuation = 5,
    /// Appended for issue #11 (upgradeable contract pattern + emergency
    /// pause): mutating calls are rejected while the contract is paused.
    /// Placed after the highest pre-existing error code (5) rather than
    /// renumbering anything.
    Paused = 6,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    NextId,
    AllIds,
    Asset(u64),
}

#[contract]
pub struct RegistryContract;

#[contractimpl]
impl RegistryContract {
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::NextId, &0u64);
        env.storage()
            .instance()
            .set(&DataKey::AllIds, &Vec::<u64>::new(&env));
    }

    pub fn register_asset(
        env: Env,
        issuer: Address,
        token_contract: Address,
        name: String,
        asset_type: String,
        valuation: i128,
    ) -> u64 {
        Self::require_not_paused(&env);
        Self::require_initialized(&env);
        issuer.require_auth();
        if valuation < 0 {
            panic_with_error!(env, Error::InvalidValuation);
        }

        let id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap();
        env.storage().instance().set(&DataKey::NextId, &(id + 1));

        let entry = AssetEntry {
            id,
            token_contract,
            issuer: issuer.clone(),
            name,
            asset_type,
            valuation,
            created_at: env.ledger().sequence(),
            active: true,
        };
        env.storage().persistent().set(&DataKey::Asset(id), &entry);

        let mut all_ids: Vec<u64> = env.storage().instance().get(&DataKey::AllIds).unwrap();
        all_ids.push_back(id);
        env.storage().instance().set(&DataKey::AllIds, &all_ids);

        env.events()
            .publish((symbol_short!("register"), issuer), id);
        id
    }

    pub fn get_asset(env: Env, asset_id: u64) -> AssetEntry {
        Self::asset_or_panic(&env, asset_id)
    }

    pub fn get_assets_by_issuer(env: Env, issuer: Address) -> Vec<AssetEntry> {
        Self::filter_assets(&env, |entry| entry.issuer == issuer)
    }

    pub fn get_assets_by_type(env: Env, asset_type: String) -> Vec<AssetEntry> {
        Self::filter_assets(&env, |entry| entry.asset_type == asset_type)
    }

    pub fn get_all_assets(env: Env) -> Vec<AssetEntry> {
        Self::filter_assets(&env, |_| true)
    }

    pub fn deactivate_asset(env: Env, admin: Address, asset_id: u64) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);

        let mut entry = Self::asset_or_panic(&env, asset_id);
        entry.active = false;
        env.storage()
            .persistent()
            .set(&DataKey::Asset(asset_id), &entry);

        env.events().publish((symbol_short!("deactvate"),), asset_id);
    }

    pub fn total_value_locked(env: Env) -> i128 {
        Self::filter_assets(&env, |entry| entry.active)
            .iter()
            .map(|entry| entry.valuation)
            .fold(0i128, |acc, v| acc.saturating_add(v))
    }

    pub fn asset_count(env: Env) -> u64 {
        let all_ids: Vec<u64> = env
            .storage()
            .instance()
            .get(&DataKey::AllIds)
            .unwrap_or(Vec::new(&env));
        all_ids.len() as u64
    }

    /// Current contract ABI version, polled by the off-chain indexer
    /// (`api/src/indexer/mod.rs::check_abi`) before it trusts this
    /// contract's other return shapes.
    pub fn version(_env: Env) -> u64 {
        1
    }

    // ---- issue #11: emergency pause + upgradeable contract pattern ----

    pub fn pause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        tessera_common::set_paused(&env, true);
    }

    pub fn unpause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        tessera_common::set_paused(&env, false);
    }

    /// Migrate this contract to a new WASM build without losing storage.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::require_admin(&env, &admin);
        tessera_common::upgrade(&env, new_wasm_hash);
    }

    // ---- internal ----

    fn require_initialized(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin = Self::require_initialized(env);
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    fn require_not_paused(env: &Env) {
        if tessera_common::is_paused(env) {
            panic_with_error!(env, Error::Paused);
        }
    }

    fn asset_or_panic(env: &Env, asset_id: u64) -> AssetEntry {
        env.storage()
            .persistent()
            .get(&DataKey::Asset(asset_id))
            .unwrap_or_else(|| panic_with_error!(env, Error::AssetNotFound))
    }

    fn filter_assets(env: &Env, predicate: impl Fn(&AssetEntry) -> bool) -> Vec<AssetEntry> {
        let all_ids: Vec<u64> = env
            .storage()
            .instance()
            .get(&DataKey::AllIds)
            .unwrap_or(Vec::new(env));
        let mut out = Vec::new(env);
        for id in all_ids.iter() {
            if let Some(entry) = env
                .storage()
                .persistent()
                .get::<DataKey, AssetEntry>(&DataKey::Asset(id))
            {
                if predicate(&entry) {
                    out.push_back(entry);
                }
            }
        }
        out
    }
}
