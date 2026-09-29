//! Asset-token contract — good-faith reconstruction of the deployed testnet
//! Includes issue #40: Automated Share Buyback & Capital Reduction module
//! (`buyback.rs`).
//! contract at `CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ`,
//! built from `docs/app/docs/contracts/asset-token/page.mdx` and
//! cross-checked against the `RawMetadata` shape `api/src/indexer/mod.rs`
//! already decodes from that live contract. See the repository root
//! `contracts/` entry in the pull request description for the full
//! reconstruction caveat.
#![no_std]

pub mod stock_split;
pub mod redemption_pool;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    BytesN, Env, IntoVal, String, Symbol, Val, Vec,
};

#[contracttype]
#[derive(Clone)]
pub struct AssetMetadata {
    pub name: String,
    pub symbol: String,
    pub asset_type: String,
    pub total_supply: i128,
    pub decimals: u32,
    pub admin: Address,
    pub compliance_contract: Address,
    pub asset_description: String,
    pub valuation: i128,
    pub paused: bool,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    Unauthorized = 3,
    InsufficientBalance = 4,
    InvalidAmount = 5,
    Paused = 6,
    SenderNotCompliant = 7,
    RecipientNotCompliant = 8,
    Overflow = 9,
    /// Appended for issue #10 (holding-period lockups). Placed after the
    /// highest pre-existing error code (9) rather than renumbering anything.
    /// Issue #10 — holding-period lockups.
    Locked = 10,
    /// Issue #90: split ratio is zero, or the cumulative multiplier overflows.
    InvalidSplitRatio = 11,
    NotLiquidated = 12,
    AlreadyLiquidated = 13,
    StablecoinNotConfigured = 14,
    RedemptionExceedsAllocation = 15,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetStatus {
    Active,
    Liquidated,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    Name,
    Symbol,
    AssetType,
    TotalSupply,
    Decimals,
    Compliance,
    Description,
    Valuation,
    Paused,
    Balance(Address),
    /// Issue #10: ledger sequence at/after which `holder` may transfer or
    /// burn. Absent (or `0`) means no lockup is in effect.
    Lockup(Address),
    /// Issue #90: number of splits executed so far.
    SplitCount,
    /// Issue #90: `(numerator, denominator)` of the split with this 1-based index.
    Split(u32),
    /// Issue #90: cumulative `(numerator, denominator)` multiplier (reduced).
    SplitMultiplier,
    /// Issue #90: number of splits already applied to this holder's stored balance.
    HolderEpoch(Address),
    AssetStatus,
    Stablecoin,
    LiquidationSupply,
    LiquidationProceeds,
    Redeemed(Address),
}

#[contract]
pub struct AssetTokenContract;

#[contractimpl]
impl AssetTokenContract {
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        env: Env,
        admin: Address,
        name: String,
        symbol: String,
        asset_type: String,
        total_supply: i128,
        decimals: u32,
        compliance_contract: Address,
        asset_description: String,
        valuation: i128,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        if total_supply < 0 || valuation < 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        if !Self::compliance_allows(&env, &compliance_contract, &admin) {
            panic_with_error!(env, Error::RecipientNotCompliant);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Name, &name);
        env.storage().instance().set(&DataKey::Symbol, &symbol);
        env.storage()
            .instance()
            .set(&DataKey::AssetType, &asset_type);
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &total_supply);
        env.storage().instance().set(&DataKey::Decimals, &decimals);
        env.storage()
            .instance()
            .set(&DataKey::Compliance, &compliance_contract);
        env.storage()
            .instance()
            .set(&DataKey::Description, &asset_description);
        env.storage()
            .instance()
            .set(&DataKey::Valuation, &valuation);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage()
            .instance()
            .set(&DataKey::AssetStatus, &AssetStatus::Active);

        stock_split::set_balance(&env, &admin.clone(), total_supply);

        // Mirrors the deployed contract's documented initial-mint event.
        env.events()
            .publish((symbol_short!("mint"), admin), total_supply);
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        Self::require_not_paused(&env);
        Self::require_active(&env);
        Self::require_not_locked(&env, &from);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }

        let compliance: Address = env.storage().instance().get(&DataKey::Compliance).unwrap();
        if !Self::compliance_allows(&env, &compliance, &from) {
            panic_with_error!(env, Error::SenderNotCompliant);
        }
        if !Self::compliance_allows(&env, &compliance, &to) {
            panic_with_error!(env, Error::RecipientNotCompliant);
        }

        let from_balance = Self::balance(env.clone(), from.clone());
        if from_balance < amount {
            panic_with_error!(env, Error::InsufficientBalance);
        }

        // A self-transfer is a no-op, and must be handled before the two
        // balance writes below. Those writes read `to_balance` *before*
        // `from_balance` is written, so with `from == to` they would store
        // `from_balance - amount` and then immediately overwrite it with
        // `from_balance + amount` — inflating the holder by `amount` while
        // `total_supply` stayed put. Since the excess is not backed by supply,
        // a holder with a small balance could transfer `amount` to themselves
        // repeatedly to manufacture an arbitrarily large balance, which
        // `clawback`/`burn` then treats as real. Guard it explicitly.
        if from == to {
            env.events()
                .publish((symbol_short!("transfer"), from, to), amount);
            return;
        }

        let to_balance = Self::balance(env.clone(), to.clone());
        let new_to_balance = to_balance
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));

        stock_split::set_balance(&env, &from.clone(), from_balance - amount);
        stock_split::set_balance(&env, &to.clone(), to_balance + amount);

        env.events()
            .publish((symbol_short!("transfer"), from, to), amount);
    }

    pub fn mint(env: Env, admin: Address, to: Address, amount: i128) {
        Self::require_admin(&env, &admin);
        Self::require_not_paused(&env);
        Self::require_active(&env);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }

        let compliance: Address = env.storage().instance().get(&DataKey::Compliance).unwrap();
        if !Self::compliance_allows(&env, &compliance, &to) {
            panic_with_error!(env, Error::RecipientNotCompliant);
        }

        let total_supply: i128 = env.storage().instance().get(&DataKey::TotalSupply).unwrap();
        let new_total = total_supply
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &new_total);

        let to_balance = Self::balance(env.clone(), to.clone());
        stock_split::set_balance(&env, &to.clone(), to_balance + amount);

        env.events().publish((symbol_short!("mint"), to), amount);
    }

    pub fn burn(env: Env, from: Address, amount: i128) {
        from.require_auth();
        Self::require_not_locked(&env, &from);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }

        let from_balance = Self::balance(env.clone(), from.clone());
        if from_balance < amount {
            panic_with_error!(env, Error::InsufficientBalance);
        }
        stock_split::set_balance(&env, &from.clone(), from_balance - amount);

        let total_supply: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0);
        // `from_balance >= amount` is checked above, which is enough *only* while
        // the invariant `balance <= total_supply` holds. Checked explicitly so
        // that a supply/balance divergence can never underflow into a trap.
        let new_total = total_supply
            .checked_sub(amount)
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &new_total);

        env.events().publish((symbol_short!("burn"), from), amount);
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        stock_split::effective_balance(&env, &id)
    }

    /// Issue #90 - execute a forward (`num > den`) or reverse (`num < den`)
    /// split. O(1): only a global multiplier changes; holder balances are
    /// settled lazily on next read/write. Admin-authenticated.
    pub fn execute_share_split(
        env: Env,
        admin: Address,
        ratio_numerator: u32,
        ratio_denominator: u32,
    ) {
        Self::require_admin(&env, &admin);
        stock_split::execute(&env, ratio_numerator, ratio_denominator);
    }

    /// Configure the stablecoin used for liquidation proceeds.
    pub fn set_liquidation_stablecoin(env: Env, admin: Address, stablecoin: Address) {
        Self::require_admin(&env, &admin);
        Self::require_active(&env);
        env.storage().instance().set(&DataKey::Stablecoin, &stablecoin);
    }

    /// Deposit the cash proceeds from an asset sale and permanently close the
    /// secondary market. The stablecoin must already have been transferred to
    /// this contract by the administrator.
    pub fn deposit_liquidation_proceeds(env: Env, admin: Address, stablecoin_amount: i128) {
        Self::require_admin(&env, &admin);
        Self::require_active(&env);
        if stablecoin_amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        if !env.storage().instance().has(&DataKey::Stablecoin) {
            panic_with_error!(env, Error::StablecoinNotConfigured);
        }
        let supply = Self::total_supply(env.clone());
        if supply <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        env.storage().instance().set(&DataKey::LiquidationSupply, &supply);
        env.storage()
            .instance()
            .set(&DataKey::LiquidationProceeds, &stablecoin_amount);
        env.storage()
            .instance()
            .set(&DataKey::AssetStatus, &AssetStatus::Liquidated);
        env.events()
            .publish((symbol_short!("liquidate"),), stablecoin_amount);
    }

    /// Burn asset tokens and pay the holder's pro-rata share of the deposited
    /// proceeds. Transfers and minting are disabled after liquidation.
    pub fn redeem_liquidation_proceeds(env: Env, holder: Address, token_amount: i128) -> i128 {
        holder.require_auth();
        if Self::status(env.clone()) != AssetStatus::Liquidated {
            panic_with_error!(env, Error::NotLiquidated);
        }
        if token_amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        let balance = Self::balance(env.clone(), holder.clone());
        let redeemed: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::Redeemed(holder.clone()))
            .unwrap_or(0);
        if balance < token_amount {
            panic_with_error!(env, Error::InsufficientBalance);
        }
        let supply: i128 = env.storage().instance().get(&DataKey::LiquidationSupply).unwrap();
        let proceeds: i128 = env.storage().instance().get(&DataKey::LiquidationProceeds).unwrap();
        let payout = ((token_amount as u128 * proceeds as u128) / supply as u128) as i128;
        let new_redeemed = redeemed.checked_add(token_amount).unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
        stock_split::set_balance(&env, &holder.clone(), balance - token_amount);
        env.storage().persistent().set(&DataKey::Redeemed(holder.clone()), &new_redeemed);
        let total = Self::total_supply(env.clone());
        env.storage().instance().set(&DataKey::TotalSupply, &(total - token_amount));
        let stablecoin: Address = env.storage().instance().get(&DataKey::Stablecoin).unwrap();
        let args: Vec<Val> = (env.current_contract_address(), holder.clone(), payout).into_val(&env);
        env.invoke_contract::<()>(&stablecoin, &Symbol::new(&env, "transfer"), args);
        env.events().publish((symbol_short!("redeem"), holder), (token_amount, payout));
        payout
    }

    pub fn status(env: Env) -> AssetStatus {
        env.storage().instance().get(&DataKey::AssetStatus).unwrap_or(AssetStatus::Active)
    }

    /// Issue #90 - cumulative split multiplier as `(numerator, denominator)`.
    pub fn split_multiplier(env: Env) -> (u128, u128) {
        stock_split::multiplier(&env)
    }

    /// Issue #90 - number of splits executed so far.
    pub fn split_count(env: Env) -> u32 {
        stock_split::split_count(&env)
    }

    pub fn total_supply(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0)
    }

    pub fn pause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::Paused, &true);
        env.events().publish((symbol_short!("pause"),), admin);
    }

    pub fn unpause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.events().publish((symbol_short!("unpause"),), admin);
    }

    pub fn update_valuation(env: Env, admin: Address, new_valuation: i128) {
        Self::require_admin(&env, &admin);
        if new_valuation < 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        env.storage()
            .instance()
            .set(&DataKey::Valuation, &new_valuation);
        env.events()
            .publish((symbol_short!("valuation"),), new_valuation);
    }

    pub fn set_compliance(env: Env, admin: Address, compliance: Address) {
        Self::require_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Compliance, &compliance);
        env.events()
            .publish((symbol_short!("setcomp"),), compliance);
    }

    /// Issue #10 — set (or clear, with `unlock_ledger = 0`) the holding-period
    /// lockup for `holder`. Admin-authenticated. Used both to impose a
    /// regulatory-exemption holding period (e.g. a 12-month lockup) and, by
    /// setting `unlock_ledger` back to `0` or to an earlier ledger, to grant
    /// a partial/early release under a court order or approved exemption.
    /// Does not affect `mint` or `initialize`.
    pub fn set_lockup(env: Env, admin: Address, holder: Address, unlock_ledger: u32) {
        Self::require_admin(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::Lockup(holder.clone()), &unlock_ledger);
        env.events()
            .publish((symbol_short!("lockup"), holder), unlock_ledger);
    }

    /// The ledger sequence at/after which `holder` may transfer or burn.
    /// `0` means no lockup is in effect.
    pub fn get_lockup(env: Env, holder: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Lockup(holder))
            .unwrap_or(0)
    }

    pub fn get_metadata(env: Env) -> AssetMetadata {
        AssetMetadata {
            name: env.storage().instance().get(&DataKey::Name).unwrap(),
            symbol: env.storage().instance().get(&DataKey::Symbol).unwrap(),
            asset_type: env.storage().instance().get(&DataKey::AssetType).unwrap(),
            total_supply: env.storage().instance().get(&DataKey::TotalSupply).unwrap(),
            decimals: env.storage().instance().get(&DataKey::Decimals).unwrap(),
            admin: env.storage().instance().get(&DataKey::Admin).unwrap(),
            compliance_contract: env.storage().instance().get(&DataKey::Compliance).unwrap(),
            asset_description: env.storage().instance().get(&DataKey::Description).unwrap(),
            valuation: env.storage().instance().get(&DataKey::Valuation).unwrap(),
            paused: env.storage().instance().get(&DataKey::Paused).unwrap(),
        }
    }

    /// Current contract ABI version, polled by the off-chain indexer.
    pub fn version(_env: Env) -> u64 {
        1
    }

    /// Issue #12 — on-chain clawback engine.
    ///
    /// Deliberately implemented as a standalone path, NOT on top of
    /// `transfer`: a clawback is an admin-only, legally-authorized recovery
    /// mechanism (court order, lost-key recovery, regulatory enforcement)
    /// that must be able to succeed precisely when the holder cannot or
    /// will not cooperate — for example, their address has since been
    /// suspended in compliance, or they refuse to sign. Routing this
    /// through `transfer` would force it through `from.require_auth()` and
    /// the sender-compliance check, which are exactly the two things a
    /// clawback needs to bypass. It still validates the source balance
    /// (reusing `Error::InsufficientBalance`, since the failure mode is
    /// identical to a transfer's — see the PR description for why a new
    /// error code was not added here), and emits a dedicated `clawback`
    /// event so the recovery is fully auditable on-chain. Total supply is
    /// intentionally left unchanged: the clawed-back balance is removed
    /// from `from` only; re-issuing or burning it is a separate, later
    /// admin action (e.g. `mint`/`burn`) and out of scope for this engine.
    pub fn clawback(env: Env, admin: Address, from: Address, amount: i128) {
        Self::require_admin(&env, &admin);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }

        let from_balance = Self::balance(env.clone(), from.clone());
        if from_balance < amount {
            panic_with_error!(env, Error::InsufficientBalance);
        }
        stock_split::set_balance(&env, &from.clone(), from_balance - amount);

        env.events()
            .publish((symbol_short!("clawback"), from), amount);
    }

    /// Issue #11 — migrate this contract to a new WASM build without losing
    /// storage. Reuses `Error::Unauthorized` (3), already the admin-mismatch
    /// error for every other admin-gated call on this contract; no new
    /// error code is needed.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::require_admin(&env, &admin);
        tessera_common::upgrade(&env, new_wasm_hash);
    }

    // ---- internal ----

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized));
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    fn require_not_paused(env: &Env) {
        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if paused {
            panic_with_error!(env, Error::Paused);
        }
    }

    fn require_active(env: &Env) {
        if Self::status(env.clone()) == AssetStatus::Liquidated {
            panic_with_error!(env, Error::AlreadyLiquidated);
        }
    }

    fn require_not_locked(env: &Env, holder: &Address) {
        let unlock_ledger: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Lockup(holder.clone()))
            .unwrap_or(0);
        if unlock_ledger != 0 && env.ledger().sequence() < unlock_ledger {
            panic_with_error!(env, Error::Locked);
        }
    }

    fn compliance_allows(env: &Env, compliance: &Address, who: &Address) -> bool {
        let args: Vec<Val> = (who.clone(),).into_val(env);
        env.invoke_contract(compliance, &Symbol::new(env, "is_allowed"), args)
    }
}
