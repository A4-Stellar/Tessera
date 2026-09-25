//! Compliance contract — good-faith reconstruction of the deployed testnet
//! contract at `CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU`,
//! built from `docs/app/docs/contracts/compliance/page.mdx` and
//! cross-checked against the `RawKyc` shape `api/src/indexer/mod.rs`
//! already decodes from that live contract. See the repository root
//! `contracts/` entry in the pull request description for the full
//! reconstruction caveat.
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    BytesN, Env, IntoVal, String, Symbol, Val, Vec,
};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComplianceStatus {
    Approved,
    Pending,
    Rejected,
    Suspended,
}

#[contracttype]
#[derive(Clone)]
pub struct KycRecord {
    pub address: Address,
    pub status: ComplianceStatus,
    pub jurisdiction: String,
    pub verified_at: u32,
    pub expires_at: u32,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    RecordNotFound = 3,
    InvalidExpiry = 4,
    Unauthorized = 5,
    /// Appended for issue #11. Placed after the highest pre-existing error
    /// code (5) rather than renumbering anything; note this contract's
    /// documented numbering already skips `2`.
    Paused = 6,
    /// Appended for issue #9 (modular transfer gate hooks). Placed after the
    /// highest pre-existing error code (6) rather than renumbering anything.
    HookAlreadyRegistered = 7,
    HookNotRegistered = 8,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    Record(Address),
    AllowList,
    BlockedJurisdictions,
    /// Issue #9: contract addresses of registered pluggable transfer-gate
    /// hooks (e.g. per-jurisdiction rule modules), each expected to expose
    /// `check(env, address: Address) -> bool`.
    Hooks,
    TaxResidency(Address),
}

#[contract]
pub struct ComplianceContract;

#[contractimpl]
impl ComplianceContract {
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::AllowList, &Vec::<Address>::new(&env));
        env.storage()
            .instance()
            .set(&DataKey::BlockedJurisdictions, &Vec::<String>::new(&env));
        env.storage()
            .instance()
            .set(&DataKey::Hooks, &Vec::<Address>::new(&env));
    }

    pub fn add_to_allowlist(
        env: Env,
        admin: Address,
        address: Address,
        jurisdiction: String,
        expires_at: u32,
    ) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);

        let now = env.ledger().sequence();
        if expires_at != 0 && expires_at <= now {
            panic_with_error!(env, Error::InvalidExpiry);
        }

        let is_new = !env
            .storage()
            .persistent()
            .has(&DataKey::Record(address.clone()));
        let record = KycRecord {
            address: address.clone(),
            status: ComplianceStatus::Approved,
            jurisdiction: jurisdiction.clone(),
            verified_at: now,
            expires_at,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Record(address.clone()), &record);

        if is_new {
            let mut list: Vec<Address> = env.storage().instance().get(&DataKey::AllowList).unwrap();
            list.push_back(address.clone());
            env.storage().instance().set(&DataKey::AllowList, &list);
        }

        env.events()
            .publish((symbol_short!("approved"), address), (jurisdiction, expires_at));
    }

    pub fn suspend(env: Env, admin: Address, address: Address) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);

        let mut record = Self::record_or_panic(&env, &address);
        record.status = ComplianceStatus::Suspended;
        env.storage()
            .persistent()
            .set(&DataKey::Record(address.clone()), &record);

        env.events().publish((symbol_short!("suspend"), address), ());
    }

    pub fn remove(env: Env, admin: Address, address: Address) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);

        if !env
            .storage()
            .persistent()
            .has(&DataKey::Record(address.clone()))
        {
            panic_with_error!(env, Error::RecordNotFound);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::Record(address.clone()));

        let mut list: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllowList)
            .unwrap_or(Vec::new(&env));
        if let Some(idx) = list.iter().position(|a| a == address) {
            list.remove(idx as u32);
            env.storage().instance().set(&DataKey::AllowList, &list);
        }

        env.events().publish((symbol_short!("removed"), address), ());
    }

    pub fn is_allowed(env: Env, address: Address) -> bool {
        let record: Option<KycRecord> = env.storage().persistent().get(&DataKey::Record(address));
        let record = match record {
            Some(r) => r,
            None => return false,
        };
        if record.status != ComplianceStatus::Approved {
            return false;
        }
        if record.expires_at != 0 && env.ledger().sequence() >= record.expires_at {
            return false;
        }
        !Self::is_jurisdiction_blocked(env.clone(), record.jurisdiction)
    }

    pub fn get_record(env: Env, address: Address) -> Option<KycRecord> {
        env.storage().persistent().get(&DataKey::Record(address))
    }

    pub fn get_allowlist(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::AllowList)
            .unwrap_or(Vec::new(&env))
    }

    // ---- issue #9: modular transfer gate hooks for regulatory jurisdictions ----
    //
    // Jurisdictions/rules (e.g. SEC Regulation D vs. Regulation S) can be
    // added or removed without redeploying this contract by registering a
    // hook contract's address. Each registered hook is expected to expose
    // `check(env: Env, address: Address) -> bool`, evaluated via
    // cross-contract invocation. `is_allowed` itself is left untouched for
    // backward compatibility with existing callers (notably
    // `asset-token::compliance_allows`); `is_allowed_with_hooks` is the new
    // entry point that layers hook evaluation on top of it.

    /// Register a rule-module hook contract. Admin-authenticated.
    pub fn register_hook(env: Env, admin: Address, hook_contract: Address) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);

        let mut hooks: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Hooks)
            .unwrap_or(Vec::new(&env));
        if hooks.iter().any(|h| h == hook_contract) {
            panic_with_error!(env, Error::HookAlreadyRegistered);
        }
        hooks.push_back(hook_contract.clone());
        env.storage().instance().set(&DataKey::Hooks, &hooks);

        env.events()
            .publish((symbol_short!("hookreg"),), hook_contract);
    }

    /// Unregister a previously-registered hook contract. Admin-authenticated.
    pub fn unregister_hook(env: Env, admin: Address, hook_contract: Address) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);

        let mut hooks: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Hooks)
            .unwrap_or(Vec::new(&env));
        let idx = hooks
            .iter()
            .position(|h| h == hook_contract)
            .unwrap_or_else(|| panic_with_error!(env, Error::HookNotRegistered));
        hooks.remove(idx as u32);
        env.storage().instance().set(&DataKey::Hooks, &hooks);

        env.events()
            .publish((symbol_short!("hookunreg"),), hook_contract);
    }

    /// Currently registered transfer-gate hook contracts.
    pub fn get_hooks(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::Hooks)
            .unwrap_or(Vec::new(&env))
    }

    /// `is_allowed` plus every registered hook's `check(address)`. Returns
    /// `true` only if the built-in checks pass AND every hook returns
    /// `true`. Hooks are evaluated in registration order and evaluation
    /// short-circuits on the first failing hook (or the first failing
    /// built-in check), so a rejected address never pays for the
    /// cross-contract calls that follow it.
    pub fn is_allowed_with_hooks(env: Env, address: Address) -> bool {
        if !Self::is_allowed(env.clone(), address.clone()) {
            return false;
        }

        let hooks: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Hooks)
            .unwrap_or(Vec::new(&env));
        for hook in hooks.iter() {
            if !Self::invoke_hook_check(&env, &hook, &address) {
                return false;
            }
        }
        true
    }

    pub fn block_jurisdiction(env: Env, admin: Address, jurisdiction: String) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);

        let mut list: Vec<String> = env
            .storage()
            .instance()
            .get(&DataKey::BlockedJurisdictions)
            .unwrap_or(Vec::new(&env));
        if !list.iter().any(|j| j == jurisdiction) {
            list.push_back(jurisdiction.clone());
            env.storage()
                .instance()
                .set(&DataKey::BlockedJurisdictions, &list);
        }

        env.events().publish((symbol_short!("blockjur"),), jurisdiction);
    }

    pub fn unblock_jurisdiction(env: Env, admin: Address, jurisdiction: String) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);

        let mut list: Vec<String> = env
            .storage()
            .instance()
            .get(&DataKey::BlockedJurisdictions)
            .unwrap_or(Vec::new(&env));
        if let Some(idx) = list.iter().position(|j| j == jurisdiction) {
            list.remove(idx as u32);
            env.storage()
                .instance()
                .set(&DataKey::BlockedJurisdictions, &list);
        }

        env.events().publish((symbol_short!("unblkjur"),), jurisdiction);
    }

    pub fn is_jurisdiction_blocked(env: Env, jurisdiction: String) -> bool {
        let list: Vec<String> = env
            .storage()
            .instance()
            .get(&DataKey::BlockedJurisdictions)
            .unwrap_or(Vec::new(&env));
        list.iter().any(|j| j == jurisdiction)
    }

    pub fn set_tax_residency(env: Env, admin: Address, investor: Address, residency_hash: BytesN<32>) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::TaxResidency(investor), &residency_hash);
    }

    pub fn get_tax_residency(env: Env, investor: Address) -> Option<BytesN<32>> {
        env.storage().persistent().get(&DataKey::TaxResidency(investor))
    }

    /// Current contract ABI version, polled by the off-chain indexer.
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

    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::require_admin(&env, &admin);
        tessera_common::upgrade(&env, new_wasm_hash);
    }

    // ---- internal ----

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
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

    fn record_or_panic(env: &Env, address: &Address) -> KycRecord {
        env.storage()
            .persistent()
            .get(&DataKey::Record(address.clone()))
            .unwrap_or_else(|| panic_with_error!(env, Error::RecordNotFound))
    }

    /// Cross-contract call into a registered hook's `check(address)`.
    fn invoke_hook_check(env: &Env, hook: &Address, address: &Address) -> bool {
        let args: Vec<Val> = (address.clone(),).into_val(env);
        env.invoke_contract(hook, &Symbol::new(env, "check"), args)
    }
}
