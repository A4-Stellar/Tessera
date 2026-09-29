//! Dividend contract — good-faith reconstruction of the deployed testnet
//! contract at `CAR4XY3CEBQWFOL27JEWFW34KXSIZA7RFKDQMEIV7ZU723RWY37I2SYX`,
//! built from `docs/app/docs/contracts/dividend/page.mdx` and cross-checked
//! against the `RawDistribution` shape `api/src/indexer/mod.rs` already
//! decodes from that live contract. See the repository root `contracts/`
//! entry in the pull request description for the full reconstruction
//! caveat.
//!
//! Cross-contract calls to the asset-token and payment-token contracts use
//! `Env::invoke_contract` directly (rather than a generated client from
//! `contractimport!`) since this repository never shipped compiled `.wasm`
//! for the real, already-deployed contracts to import against.
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    BytesN, Env, IntoVal, Symbol, Val, Vec,
};

#[contracttype]
#[derive(Clone)]
pub struct Distribution {
    pub id: u64,
    pub asset_token: Address,
    pub payment_token: Address,
    pub total_amount: i128,
    pub distributed: i128,
    pub created_at: u32,
    pub completed: bool,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    DistributionNotFound = 4,
    InvalidAmount = 5,
    NothingToClaim = 6,
    AlreadyClaimed = 7,
    /// Appended for issue #11. Placed after the highest pre-existing error
    /// code (7) rather than renumbering anything.
    Paused = 8,
    DripNotConfigured = 9,
    InvalidSwapOutput = 10,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    NextId,
    AllIds,
    Distribution(u64),
    Claimed(u64, Address),
    DripAmm,
    DripCapTable,
}

#[contract]
pub struct DividendContract;

#[contractimpl]
impl DividendContract {
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

    pub fn create_distribution(
        env: Env,
        admin: Address,
        asset_token: Address,
        payment_token: Address,
        total_amount: i128,
    ) -> u64 {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);
        if total_amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }

        // Pull `total_amount` of `payment_token` from `admin` into this
        // contract's escrow. `admin.require_auth()` above covers the nested
        // `transfer` invocation via Soroban's auth propagation.
        Self::token_transfer(
            &env,
            &payment_token,
            &admin,
            &env.current_contract_address(),
            total_amount,
        );

        let id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap();
        env.storage().instance().set(&DataKey::NextId, &(id + 1));

        let dist = Distribution {
            id,
            asset_token,
            payment_token,
            total_amount,
            distributed: 0,
            created_at: env.ledger().sequence(),
            completed: false,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Distribution(id), &dist);

        // Issue #144: record the compound-yield accrual epoch. Yield itself
        // is opt-in per distribution via `set_distribution_yield` +
        // `fund_yield`; this only anchors the clock.
        compound_yield::on_distribution_created(&env, id);

        let mut all_ids: Vec<u64> = env.storage().instance().get(&DataKey::AllIds).unwrap();
        all_ids.push_back(id);
        env.storage().instance().set(&DataKey::AllIds, &all_ids);

        env.events()
            .publish((symbol_short!("created"), admin), (id, total_amount));
        id
    }

    pub fn claimable(env: Env, distribution_id: u64, holder: Address) -> i128 {
        let dist = match Self::get_distribution_opt(&env, distribution_id) {
            Some(d) => d,
            None => return 0,
        };
        if env
            .storage()
            .persistent()
            .has(&DataKey::Claimed(distribution_id, holder.clone()))
        {
            return 0;
        }

        let balance: i128 = Self::token_balance(&env, &dist.asset_token, &holder);
        if balance <= 0 {
            return 0;
        }
        let total_supply: i128 = Self::token_total_supply(&env, &dist.asset_token);
        if total_supply <= 0 {
            return 0;
        }

        // floor(total_amount * balance / total_supply)
        let share = dist.total_amount.saturating_mul(balance) / total_supply;

        // Bound by what remains in escrow. Balances are read live (matching
        // the deployed contract's documented behavior: a holder who
        // acquires more tokens before claiming gets a larger share on that
        // one claim). Without this bound, the SAME underlying tokens could
        // be moved through multiple not-yet-claimed, compliance-approved
        // addresses and each hop would compute a full share independently
        // — draining the escrow far beyond `total_amount` regardless of how
        // many distinct holders it was ever meant to cover. Capping every
        // payout to the remaining escrow keeps the single-holder,
        // live-balance semantics intact while making the aggregate payout
        // across all claimants bounded by what was actually escrowed.
        let remaining = dist.total_amount.saturating_sub(dist.distributed);
        share.min(remaining.max(0))
    }

    // ---- issue #144: compound yield on unclaimed dividends ----

    /// Set the annual compound-yield rate for a distribution (WAD, 18
    /// decimals; `1e18` = 100%/year). `0` disables yield. Yield payouts are
    /// funded separately via `fund_yield`, never out of the base escrow.
    pub fn set_distribution_yield(env: Env, admin: Address, distribution_id: u64, rate: i128) {
        Self::require_admin(&env, &admin);
        if Self::get_distribution_opt(&env, distribution_id).is_none() {
            panic_with_error!(env, Error::DistributionNotFound);
        }
        compound_yield::set_rate(&env, distribution_id, rate);
        env.events().publish(
            (symbol_short!("yieldrate"),),
            (distribution_id, rate),
        );
    }

    /// Deposit extra `payment_token` (from `admin`) earmarked for yield
    /// payouts on a distribution. Base shareholders are never paid yield
    /// out of the base escrow.
    pub fn fund_yield(env: Env, admin: Address, distribution_id: u64, amount: i128) {
        Self::require_admin(&env, &admin);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        let dist = Self::get_distribution_opt(&env, distribution_id)
            .unwrap_or_else(|| panic_with_error!(env, Error::DistributionNotFound));
        Self::token_transfer(
            &env,
            &dist.payment_token,
            &admin,
            &env.current_contract_address(),
            amount,
        );
        compound_yield::fund(&env, distribution_id, amount);
        env.events().publish(
            (symbol_short!("yfund"),),
            (distribution_id, amount),
        );
    }

    /// Fold the compound interest on the holder's current base share into
    /// their accrual slot and pay base share + any payable yield.
    /// Shares already claimed are ineligible for further accrual (the slot
    /// is cleared as the yield is paid).
    pub fn claim(env: Env, distribution_id: u64, holder: Address) {
        Self::require_not_paused(&env);
        holder.require_auth();

        let mut dist: Distribution = env
            .storage()
            .persistent()
            .get(&DataKey::Distribution(distribution_id))
            .unwrap_or_else(|| panic_with_error!(env, Error::DistributionNotFound));

        let claimed_key = DataKey::Claimed(distribution_id, holder.clone());
        if env.storage().persistent().has(&claimed_key) {
            panic_with_error!(env, Error::AlreadyClaimed);
        }

        let amount = Self::claimable(env.clone(), distribution_id, holder.clone());
        if amount <= 0 {
            panic_with_error!(env, Error::NothingToClaim);
        }

        // Issue #144: accrue compound yield on this holder's base share
        // before paying it, then pay any yield the yield budget can cover
        // (the base escrow is never touched for yield).
        compound_yield::accrue_for(&env, distribution_id, &holder, amount);
        let yield_amount = compound_yield::take_accrued(&env, distribution_id, &holder);

        if drip::is_enabled(&env, &holder) {
            let amm: Address = env
                .storage()
                .instance()
                .get(&DataKey::DripAmm)
                .unwrap_or_else(|| panic_with_error!(env, Error::DripNotConfigured));
            let cap_table: Address = env
                .storage()
                .instance()
                .get(&DataKey::DripCapTable)
                .unwrap_or_else(|| panic_with_error!(env, Error::DripNotConfigured));
            drip::execute(
                &env,
                &amm,
                &cap_table,
                &dist.payment_token,
                &dist.asset_token,
                &holder,
                amount,
            );
        } else {
            Self::token_transfer(
                &env,
                &dist.payment_token,
                &env.current_contract_address(),
                &holder,
                amount,
            );
        }

        if yield_amount > 0 {
            Self::token_transfer(
                &env,
                &dist.payment_token,
                &env.current_contract_address(),
                &holder,
                yield_amount,
            );
        }

        env.storage().persistent().set(&claimed_key, &true);
        dist.distributed = dist.distributed.saturating_add(amount);
        if dist.distributed >= dist.total_amount {
            dist.completed = true;
        }
        env.storage()
            .persistent()
            .set(&DataKey::Distribution(distribution_id), &dist);

        env.events().publish(
            (symbol_short!("claim"), holder),
            (distribution_id, amount + yield_amount),
        );
    }

    /// Claim previously accrued (but unpaid) yield without re-claiming the
    /// base share — for when the budget could not cover the full accrual at
    /// claim time, or a later top-up arrives.
    pub fn claim_yield(env: Env, distribution_id: u64, holder: Address) {
        Self::require_not_paused(&env);
        holder.require_auth();
        let dist = Self::get_distribution_opt(&env, distribution_id)
            .unwrap_or_else(|| panic_with_error!(env, Error::DistributionNotFound));
        let yield_amount = compound_yield::take_accrued(&env, distribution_id, &holder);
        if yield_amount <= 0 {
            panic_with_error!(env, Error::NothingToClaim);
        }
        Self::token_transfer(
            &env,
            &dist.payment_token,
            &env.current_contract_address(),
            &holder,
            yield_amount,
        );
        env.events().publish(
            (symbol_short!("yldclaim"), holder),
            (distribution_id, yield_amount),
        );
    }

    /// Read-only: yield accrued in the holder's slot (materialized by past
    /// claims), plus a live estimate on their current base share.
    pub fn get_yield_info(env: Env, distribution_id: u64, holder: Address) -> i128 {
        let accrued = compound_yield::accrued_view(&env, distribution_id, &holder);
        if let Some(dist) = Self::get_distribution_opt(&env, distribution_id) {
            if !env
                .storage()
                .persistent()
                .has(&DataKey::Claimed(distribution_id, holder.clone()))
            {
                let balance: i128 =
                    Self::token_balance(&env, &dist.asset_token, &holder);
                let total_supply: i128 =
                    Self::token_total_supply(&env, &dist.asset_token);
                if balance > 0 && total_supply > 0 {
                    let share = dist.total_amount.saturating_mul(balance) / total_supply;
                    return accrued + compound_yield::estimate_yield(
                        &env,
                        distribution_id,
                        &holder,
                        share.min(dist.total_amount.saturating_sub(dist.distributed).max(0)),
                    );
                }
            }
        }
        accrued
    }

    /// Opt in or out of receiving dividend claims as an AMM purchase of the
    /// distribution's asset token. The preference is shared by all claims.
    pub fn set_drip_preference(env: Env, holder: Address, enabled: bool) {
        drip::set_preference(&env, &holder, enabled);
    }

    pub fn get_drip_preference(env: Env, holder: Address) -> bool {
        drip::is_enabled(&env, &holder)
    }

    /// Configure the router and cap-table ledger used for opted-in claims.
    /// The cap-table must separately authorize this contract as a registrar.
    pub fn configure_drip(env: Env, admin: Address, amm: Address, cap_table: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::DripAmm, &amm);
        env.storage()
            .instance()
            .set(&DataKey::DripCapTable, &cap_table);
        env.events()
            .publish((symbol_short!("dripcfg"),), (amm, cap_table));
    }

    pub fn get_distribution(env: Env, id: u64) -> Distribution {
        env.storage()
            .persistent()
            .get(&DataKey::Distribution(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::DistributionNotFound))
    }

    pub fn get_distributions_for_asset(env: Env, asset_token: Address) -> Vec<Distribution> {
        let all_ids: Vec<u64> = env
            .storage()
            .instance()
            .get(&DataKey::AllIds)
            .unwrap_or(Vec::new(&env));
        let mut out = Vec::new(&env);
        for id in all_ids.iter() {
            if let Some(dist) = Self::get_distribution_opt(&env, id) {
                if dist.asset_token == asset_token {
                    out.push_back(dist);
                }
            }
        }
        out
    }

    pub fn has_claimed(env: Env, id: u64, holder: Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Claimed(id, holder))
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
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
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

    fn get_distribution_opt(env: &Env, id: u64) -> Option<Distribution> {
        env.storage().persistent().get(&DataKey::Distribution(id))
    }

    fn token_transfer(env: &Env, token: &Address, from: &Address, to: &Address, amount: i128) {
        let args: Vec<Val> = (from.clone(), to.clone(), amount).into_val(env);
        let _: () = env.invoke_contract(token, &Symbol::new(env, "transfer"), args);
    }

    fn token_balance(env: &Env, token: &Address, holder: &Address) -> i128 {
        let args: Vec<Val> = (holder.clone(),).into_val(env);
        env.invoke_contract(token, &Symbol::new(env, "balance"), args)
    }

    fn token_total_supply(env: &Env, token: &Address) -> i128 {
        let args: Vec<Val> = ().into_val(env);
        env.invoke_contract(token, &Symbol::new(env, "total_supply"), args)
    }
}
/// Dividend Reinvestment Program routing (AMM swap + cap-table recording).
pub mod drip;

/// Continuous compound yield accrual for uncollected dividend balances.
pub mod compound_yield;

/// Tax-withholding engine, deployed alongside the dividend contract and queried
/// for the per-jurisdiction withholding rate. Public so that its own clients
/// and fuzz target can be generated against it.
pub mod tax_withholding;

#[cfg(test)]
mod test;
