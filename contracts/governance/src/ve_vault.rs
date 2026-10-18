//! Vote-Escrowed Token Staking Vault (`veTESSERA`).
//!
//! Implements a Curve-style Vote-Escrowed (ve) staking model where users lock
//! TESSERA tokens for a chosen duration up to 4 years (`MAX_LOCK_DURATION`) to
//! receive time-weighted governance voting power (`veTESSERA`) and non-linearly
//! boosted dividend allocation weights.
//!
//! # Core Mechanics
//! - **Initial Voting Weight**:
//!   $$W = \text{Amount} \times \frac{T_{\text{locked}}}{T_{\text{max}}}$$
//! - **Linear Decay**:
//!   Voting power decays linearly as the lockup approaches `unlock_time`:
//!   $$W(t) = \text{Amount} \times \frac{\max(0, T_{\text{unlock}} - t)}{T_{\text{max}}}$$
//!   while the underlying locked principal remains untouched in escrow.
//! - **Non-Linear Dividend Weight**:
//!   Lock duration non-linearly increases dividend allocation weight via a
//!   quadratic duration curve:
//!   $$D(t) = \text{Amount} \times \left(\frac{\max(0, T_{\text{unlock}} - t)}{T_{\text{max}}}\right)^2$$
//! - **Progressive Early Withdrawal Penalty**:
//!   Stakers may exit before `unlock_time` by paying a progressive penalty that
//!   scales linearly with the remaining lock ratio (`T_remaining / T_locked`)
//!   up to `max_penalty_bps`. The penalty is transferred directly to the
//!   protocol treasury.
//! - **Soroban State Expiry Protection**:
//!   Multi-year locks (up to 4 years) exceed a single Soroban ledger TTL window
//!    on some network configurations. Every state mutation and `extend_lock_ttl`
//!   bumps both persistent and instance storage TTLs up to the required ledgers
//!   (capped at `env.storage().max_ttl()`).

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype,
    panic_with_error, Address, Env, IntoVal,
};

/// Seconds in one day (24 * 60 * 60).
pub const SECONDS_PER_DAY: u64 = 86_400;

/// Maximum lockup duration: 4 years (4 * 365 * 86,400 = 126,144,000 seconds).
pub const MAX_LOCK_DURATION: u64 = 4 * 365 * SECONDS_PER_DAY;

/// Approximate seconds per Soroban ledger close (~5 seconds).
pub const SECONDS_PER_LEDGER: u64 = 5;

/// Basis points denominator (100% = 10,000 bps).
pub const BPS_SCALE: i128 = 10_000;

/// Default maximum early-withdrawal penalty in basis points (5,000 bps = 50%).
pub const DEFAULT_MAX_PENALTY_BPS: u32 = 5_000;

#[contractclient(name = "VeTokenClient")]
pub trait TokenInterface {
    fn transfer(env: Env, from: Address, to: Address, amount: i128);
    fn balance(env: Env, id: Address) -> i128;
}

/// Represents a user's Vote-Escrowed (`veTESSERA`) lock position.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VeLock {
    /// Address of the staker who owns the lock.
    pub owner: Address,
    /// Principal amount of TESSERA tokens locked in escrow.
    pub amount: i128,
    /// Ledger timestamp (seconds) when the lock was created or last reset.
    pub start_time: u64,
    /// Total duration (seconds) committed for this lock (`<= MAX_LOCK_DURATION`).
    pub lock_duration: u64,
    /// Ledger timestamp (seconds) when the lock matures (`start_time + lock_duration`).
    pub unlock_time: u64,
    /// Voting weight at the moment the lock was created/updated: `amount * lock_duration / MAX_LOCK_DURATION`.
    pub initial_voting_power: i128,
    /// Non-linear dividend allocation weight at the moment the lock was created/updated.
    pub initial_dividend_weight: i128,
    /// Whether the principal has already been withdrawn.
    pub withdrawn: bool,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum VeError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidAmount = 4,
    InvalidDuration = 5,
    LockAlreadyExists = 6,
    LockNotFound = 7,
    LockAlreadyWithdrawn = 8,
    LockNotExpired = 9,
    LockAlreadyExpired = 10,
    InvalidPenaltyRate = 11,
    Overflow = 12,
}

#[derive(Clone)]
#[contracttype]
enum VeDataKey {
    Admin,
    Token,
    Treasury,
    MaxPenaltyBps,
    TotalLocked,
    TotalPenalties,
    Lock(Address),
}

#[contractevent]
pub struct LockCreated {
    #[topic]
    pub staker: Address,
    pub amount: i128,
    pub lock_duration: u64,
    pub unlock_time: u64,
    pub voting_power: i128,
    pub dividend_weight: i128,
}

#[contractevent]
pub struct LockUpdated {
    #[topic]
    pub staker: Address,
    pub amount: i128,
    pub lock_duration: u64,
    pub unlock_time: u64,
    pub voting_power: i128,
    pub dividend_weight: i128,
}

#[contractevent]
pub struct Withdrawn {
    #[topic]
    pub staker: Address,
    pub amount: i128,
}

#[contractevent]
pub struct EarlyWithdrawn {
    #[topic]
    pub staker: Address,
    pub payout: i128,
    pub penalty: i128,
    pub treasury: Address,
}

#[contract]
pub struct VeVaultContract;

#[contractimpl]
impl VeVaultContract {
    /// Initialize the Vote-Escrowed staking vault with the underlying token and
    /// protocol treasury that receives early-withdrawal penalty fees.
    pub fn initialize_vault(env: Env, admin: Address, token: Address, treasury: Address) {
        if env.storage().instance().has(&VeDataKey::Admin) {
            panic_with_error!(env, VeError::AlreadyInitialized);
        }
        admin.require_auth();

        env.storage().instance().set(&VeDataKey::Admin, &admin);
        env.storage().instance().set(&VeDataKey::Token, &token);
        env.storage()
            .instance()
            .set(&VeDataKey::Treasury, &treasury);
        env.storage()
            .instance()
            .set(&VeDataKey::MaxPenaltyBps, &DEFAULT_MAX_PENALTY_BPS);
        env.storage().instance().set(&VeDataKey::TotalLocked, &0i128);
        env.storage()
            .instance()
            .set(&VeDataKey::TotalPenalties, &0i128);

        Self::bump_instance_ttl(&env);
    }

    /// Configure the maximum early withdrawal penalty in basis points (`0..=10_000`).
    pub fn set_max_penalty_bps(env: Env, admin: Address, max_penalty_bps: u32) {
        Self::require_admin(&env, &admin);
        if max_penalty_bps > BPS_SCALE as u32 {
            panic_with_error!(env, VeError::InvalidPenaltyRate);
        }
        env.storage()
            .instance()
            .set(&VeDataKey::MaxPenaltyBps, &max_penalty_bps);
        Self::bump_instance_ttl(&env);
    }

    /// Lock `amount` of TESSERA tokens for `lock_duration` seconds (`1..=MAX_LOCK_DURATION`).
    ///
    /// Computes initial voting weight $W = \text{Amount} \times \frac{T_{\text{locked}}}{T_{\text{max}}}$
    /// and non-linear dividend allocation weight.
    pub fn create_lock(
        env: Env,
        staker: Address,
        amount: i128,
        lock_duration: u64,
    ) -> VeLock {
        Self::require_initialized(&env);
        staker.require_auth();

        if amount <= 0 {
            panic_with_error!(env, VeError::InvalidAmount);
        }
        if lock_duration == 0 || lock_duration > MAX_LOCK_DURATION {
            panic_with_error!(env, VeError::InvalidDuration);
        }

        let key = VeDataKey::Lock(staker.clone());
        if let Some(existing) = env.storage().persistent().get::<VeDataKey, VeLock>(&key) {
            if !existing.withdrawn {
                panic_with_error!(env, VeError::LockAlreadyExists);
            }
        }

        let now = env.ledger().timestamp();
        let unlock_time = now
            .checked_add(lock_duration)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow));

        let initial_voting_power = Self::calc_voting_weight(&env, amount, lock_duration);
        let initial_dividend_weight = Self::calc_dividend_weight(&env, amount, lock_duration);

        let lock = VeLock {
            owner: staker.clone(),
            amount,
            start_time: now,
            lock_duration,
            unlock_time,
            initial_voting_power,
            initial_dividend_weight,
            withdrawn: false,
        };

        let total_locked: i128 = env
            .storage()
            .instance()
            .get(&VeDataKey::TotalLocked)
            .unwrap_or(0);
        let new_total = total_locked
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow));

        // CEI: persist state updates before external token transfer.
        env.storage()
            .instance()
            .set(&VeDataKey::TotalLocked, &new_total);
        env.storage().persistent().set(&key, &lock);
        Self::bump_lock_ttl(&env, &key, lock_duration);
        Self::bump_instance_ttl(&env);

        let token = Self::token_address(&env);
        VeTokenClient::new(&env, &token).transfer(
            &staker,
            &env.current_contract_address(),
            &amount,
        );

        LockCreated {
            staker,
            amount,
            lock_duration,
            unlock_time,
            voting_power: initial_voting_power,
            dividend_weight: initial_dividend_weight,
        }
        .publish(&env);

        lock
    }

    /// Add `additional_amount` of tokens to an existing active lock without
    /// changing its expiration timestamp.
    pub fn increase_amount(env: Env, staker: Address, additional_amount: i128) -> VeLock {
        Self::require_initialized(&env);
        staker.require_auth();

        if additional_amount <= 0 {
            panic_with_error!(env, VeError::InvalidAmount);
        }

        let mut lock = Self::load_active_lock(&env, &staker);
        let now = env.ledger().timestamp();
        if now >= lock.unlock_time {
            panic_with_error!(env, VeError::LockAlreadyExpired);
        }

        let remaining_duration = lock.unlock_time - now;
        let new_amount = lock
            .amount
            .checked_add(additional_amount)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow));

        lock.amount = new_amount;
        lock.initial_voting_power = Self::calc_voting_weight(&env, new_amount, remaining_duration);
        lock.initial_dividend_weight =
            Self::calc_dividend_weight(&env, new_amount, remaining_duration);

        let total_locked: i128 = env
            .storage()
            .instance()
            .get(&VeDataKey::TotalLocked)
            .unwrap_or(0);
        let new_total = total_locked
            .checked_add(additional_amount)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow));

        let key = VeDataKey::Lock(staker.clone());
        // CEI: persist state before external token transfer.
        env.storage()
            .instance()
            .set(&VeDataKey::TotalLocked, &new_total);
        env.storage().persistent().set(&key, &lock);
        Self::bump_lock_ttl(&env, &key, remaining_duration);
        Self::bump_instance_ttl(&env);

        let token = Self::token_address(&env);
        VeTokenClient::new(&env, &token).transfer(
            &staker,
            &env.current_contract_address(),
            &additional_amount,
        );

        LockUpdated {
            staker,
            amount: lock.amount,
            lock_duration: lock.lock_duration,
            unlock_time: lock.unlock_time,
            voting_power: lock.initial_voting_power,
            dividend_weight: lock.initial_dividend_weight,
        }
        .publish(&env);

        lock
    }

    /// Extend the lock duration of an existing active lock to `new_lock_duration`
    /// from the current ledger timestamp (`now + new_lock_duration > lock.unlock_time`
    /// and `new_lock_duration <= MAX_LOCK_DURATION`).
    pub fn extend_lock_duration(env: Env, staker: Address, new_lock_duration: u64) -> VeLock {
        Self::require_initialized(&env);
        staker.require_auth();

        if new_lock_duration == 0 || new_lock_duration > MAX_LOCK_DURATION {
            panic_with_error!(env, VeError::InvalidDuration);
        }

        let mut lock = Self::load_active_lock(&env, &staker);
        let now = env.ledger().timestamp();
        let new_unlock_time = now
            .checked_add(new_lock_duration)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow));

        if new_unlock_time <= lock.unlock_time {
            panic_with_error!(env, VeError::InvalidDuration);
        }

        lock.start_time = now;
        lock.lock_duration = new_lock_duration;
        lock.unlock_time = new_unlock_time;
        lock.initial_voting_power = Self::calc_voting_weight(&env, lock.amount, new_lock_duration);
        lock.initial_dividend_weight =
            Self::calc_dividend_weight(&env, lock.amount, new_lock_duration);

        let key = VeDataKey::Lock(staker.clone());
        env.storage().persistent().set(&key, &lock);
        Self::bump_lock_ttl(&env, &key, new_lock_duration);
        Self::bump_instance_ttl(&env);

        LockUpdated {
            staker,
            amount: lock.amount,
            lock_duration: lock.lock_duration,
            unlock_time: lock.unlock_time,
            voting_power: lock.initial_voting_power,
            dividend_weight: lock.initial_dividend_weight,
        }
        .publish(&env);

        lock
    }

    /// Withdraw the full locked principal after the lockup has expired (`now >= unlock_time`).
    pub fn withdraw(env: Env, staker: Address) -> i128 {
        Self::require_initialized(&env);
        staker.require_auth();

        let mut lock = Self::load_active_lock(&env, &staker);
        let now = env.ledger().timestamp();
        if now < lock.unlock_time {
            panic_with_error!(env, VeError::LockNotExpired);
        }

        let amount = lock.amount;
        lock.withdrawn = true;

        let total_locked: i128 = env
            .storage()
            .instance()
            .get(&VeDataKey::TotalLocked)
            .unwrap_or(0);
        let new_total = total_locked.saturating_sub(amount);

        let key = VeDataKey::Lock(staker.clone());
        // CEI: update storage before external token transfer.
        env.storage()
            .instance()
            .set(&VeDataKey::TotalLocked, &new_total);
        env.storage().persistent().set(&key, &lock);
        Self::bump_instance_ttl(&env);

        Self::transfer_from_vault(&env, &staker, amount);

        Withdrawn { staker, amount }.publish(&env);
        amount
    }

    /// Withdraw locked tokens prior to `unlock_time`, incurring a progressive
    /// penalty fee proportional to the remaining lock duration that is transferred
    /// to the protocol treasury.
    ///
    /// Returns `(payout_to_staker, penalty_to_treasury)`.
    pub fn early_withdraw(env: Env, staker: Address) -> (i128, i128) {
        Self::require_initialized(&env);
        staker.require_auth();

        let mut lock = Self::load_active_lock(&env, &staker);
        let now = env.ledger().timestamp();
        if now >= lock.unlock_time {
            panic_with_error!(env, VeError::LockAlreadyExpired);
        }

        let penalty = Self::calc_progressive_penalty(&env, &lock, now);
        let payout = lock
            .amount
            .checked_sub(penalty)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow));

        lock.withdrawn = true;

        let total_locked: i128 = env
            .storage()
            .instance()
            .get(&VeDataKey::TotalLocked)
            .unwrap_or(0);
        let new_total = total_locked.saturating_sub(lock.amount);

        let total_penalties: i128 = env
            .storage()
            .instance()
            .get(&VeDataKey::TotalPenalties)
            .unwrap_or(0);
        let new_penalties = total_penalties
            .checked_add(penalty)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow));

        let treasury: Address = env
            .storage()
            .instance()
            .get(&VeDataKey::Treasury)
            .unwrap_or_else(|| panic_with_error!(env, VeError::NotInitialized));

        let key = VeDataKey::Lock(staker.clone());
        // CEI: persist state updates before external token transfers.
        env.storage()
            .instance()
            .set(&VeDataKey::TotalLocked, &new_total);
        env.storage()
            .instance()
            .set(&VeDataKey::TotalPenalties, &new_penalties);
        env.storage().persistent().set(&key, &lock);
        Self::bump_instance_ttl(&env);

        if payout > 0 {
            Self::transfer_from_vault(&env, &staker, payout);
        }
        if penalty > 0 {
            Self::transfer_from_vault(&env, &treasury, penalty);
        }

        EarlyWithdrawn {
            staker,
            payout,
            penalty,
            treasury,
        }
        .publish(&env);

        (payout, penalty)
    }

    /// Extend the Soroban persistent storage TTL for `staker`'s lock entry to
    /// prevent state archival during multi-year lockups (up to 4 years).
    /// Returns the TTL (in ledgers) applied to the lock entry.
    pub fn extend_lock_ttl(env: Env, caller: Address, staker: Address) -> u32 {
        Self::require_initialized(&env);
        caller.require_auth();

        let lock = Self::load_active_lock(&env, &staker);
        let now = env.ledger().timestamp();
        let remaining_seconds = lock.unlock_time.saturating_sub(now);
        let key = VeDataKey::Lock(staker);
        let ttl = Self::bump_lock_ttl(&env, &key, remaining_seconds);
        Self::bump_instance_ttl(&env);
        ttl
    }

    // ------------------------------------------------------------------
    // Views & Pure Calculations
    // ------------------------------------------------------------------

    /// Compute voting weight for `amount` locked for `lock_duration`:
    /// $$W = \text{Amount} \times \frac{T_{\text{locked}}}{T_{\text{max}}}$$
    pub fn compute_voting_weight(env: Env, amount: i128, lock_duration: u64) -> i128 {
        Self::calc_voting_weight(&env, amount, lock_duration)
    }

    /// Compute non-linear dividend allocation weight for `amount` locked for `lock_duration`:
    /// $$D = \text{Amount} \times \left(\frac{T_{\text{locked}}}{T_{\text{max}}}\right)^2$$
    pub fn compute_dividend_weight(env: Env, amount: i128, lock_duration: u64) -> i128 {
        Self::calc_dividend_weight(&env, amount, lock_duration)
    }

    /// Current decaying voting power for `staker` at the current ledger timestamp.
    pub fn voting_power(env: Env, staker: Address) -> i128 {
        let now = env.ledger().timestamp();
        Self::voting_power_at(env, staker, now)
    }

    /// Voting power for `staker` evaluated at `timestamp`.
    /// Decays linearly to `0` as `timestamp` approaches `unlock_time`.
    pub fn voting_power_at(env: Env, staker: Address, timestamp: u64) -> i128 {
        let key = VeDataKey::Lock(staker);
        let lock: VeLock = match env.storage().persistent().get(&key) {
            Some(l) => l,
            None => return 0,
        };
        if lock.withdrawn || timestamp >= lock.unlock_time {
            return 0;
        }
        let remaining = lock.unlock_time - timestamp;
        Self::calc_voting_weight(&env, lock.amount, remaining)
    }

    /// Current non-linear dividend allocation weight for `staker` at the current ledger timestamp.
    pub fn dividend_weight(env: Env, staker: Address) -> i128 {
        let now = env.ledger().timestamp();
        Self::dividend_weight_at(env, staker, now)
    }

    /// Non-linear dividend allocation weight for `staker` evaluated at `timestamp`.
    pub fn dividend_weight_at(env: Env, staker: Address, timestamp: u64) -> i128 {
        let key = VeDataKey::Lock(staker);
        let lock: VeLock = match env.storage().persistent().get(&key) {
            Some(l) => l,
            None => return 0,
        };
        if lock.withdrawn || timestamp >= lock.unlock_time {
            return 0;
        }
        let remaining = lock.unlock_time - timestamp;
        Self::calc_dividend_weight(&env, lock.amount, remaining)
    }

    /// Preview the progressive penalty fee if `staker` were to withdraw early at the current timestamp.
    pub fn calculate_early_exit_penalty(env: Env, staker: Address) -> i128 {
        let lock = Self::load_active_lock(&env, &staker);
        let now = env.ledger().timestamp();
        if now >= lock.unlock_time {
            return 0;
        }
        Self::calc_progressive_penalty(&env, &lock, now)
    }

    /// Retrieve the full `VeLock` metadata for `staker`.
    pub fn get_lock(env: Env, staker: Address) -> VeLock {
        env.storage()
            .persistent()
            .get(&VeDataKey::Lock(staker))
            .unwrap_or_else(|| panic_with_error!(env, VeError::LockNotFound))
    }

    /// Total principal amount currently locked across all active positions.
    pub fn total_locked(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&VeDataKey::TotalLocked)
            .unwrap_or(0)
    }

    /// Total early-withdrawal penalties collected and sent to the protocol treasury.
    pub fn total_penalties_collected(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&VeDataKey::TotalPenalties)
            .unwrap_or(0)
    }

    /// Current configured maximum penalty rate in basis points.
    pub fn max_penalty_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&VeDataKey::MaxPenaltyBps)
            .unwrap_or(DEFAULT_MAX_PENALTY_BPS)
    }

    // ------------------------------------------------------------------
    // Internal Helpers
    // ------------------------------------------------------------------

    fn require_initialized(env: &Env) {
        if !env.storage().instance().has(&VeDataKey::Admin) {
            panic_with_error!(env, VeError::NotInitialized);
        }
    }

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&VeDataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, VeError::NotInitialized));
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, VeError::Unauthorized);
        }
    }

    fn token_address(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&VeDataKey::Token)
            .unwrap_or_else(|| panic_with_error!(env, VeError::NotInitialized))
    }

    fn load_active_lock(env: &Env, staker: &Address) -> VeLock {
        let lock: VeLock = env
            .storage()
            .persistent()
            .get(&VeDataKey::Lock(staker.clone()))
            .unwrap_or_else(|| panic_with_error!(env, VeError::LockNotFound));
        if lock.withdrawn {
            panic_with_error!(env, VeError::LockAlreadyWithdrawn);
        }
        lock
    }

    /// $W = \text{Amount} \times \frac{T_{\text{locked}}}{T_{\text{max}}}$
    fn calc_voting_weight(env: &Env, amount: i128, duration: u64) -> i128 {
        if amount <= 0 || duration == 0 {
            return 0;
        }
        let capped_duration = core::cmp::min(duration, MAX_LOCK_DURATION) as i128;
        let numerator = amount
            .checked_mul(capped_duration)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow));
        numerator / (MAX_LOCK_DURATION as i128)
    }

    /// Non-linear (quadratic) dividend allocation weight:
    /// $D = \text{Amount} \times \left(\frac{T_{\text{locked}}}{T_{\text{max}}}\right)^2$
    fn calc_dividend_weight(env: &Env, amount: i128, duration: u64) -> i128 {
        if amount <= 0 || duration == 0 {
            return 0;
        }
        let capped_duration = core::cmp::min(duration, MAX_LOCK_DURATION) as i128;
        let max_dur = MAX_LOCK_DURATION as i128;
        let linear_weight = amount
            .checked_mul(capped_duration)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow))
            / max_dur;
        linear_weight
            .checked_mul(capped_duration)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow))
            / max_dur
    }

    /// Progressive early-exit penalty proportional to remaining lock duration:
    /// $$\text{Penalty} = \text{Amount} \times \frac{\text{max\_penalty\_bps}}{10000} \times \frac{T_{\text{remaining}}}{T_{\text{locked}}}$$
    fn calc_progressive_penalty(env: &Env, lock: &VeLock, now: u64) -> i128 {
        if now >= lock.unlock_time || lock.lock_duration == 0 {
            return 0;
        }
        let remaining = (lock.unlock_time - now) as i128;
        let lock_duration = lock.lock_duration as i128;
        let max_penalty_bps = Self::max_penalty_bps(env.clone()) as i128;

        let max_penalty_amount = lock
            .amount
            .checked_mul(max_penalty_bps)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow))
            / BPS_SCALE;

        max_penalty_amount
            .checked_mul(remaining)
            .unwrap_or_else(|| panic_with_error!(env, VeError::Overflow))
            / lock_duration
    }

    fn transfer_from_vault(env: &Env, to: &Address, amount: i128) {
        let token = Self::token_address(env);
        let contract_address = env.current_contract_address();
        env.authorize_as_current_contract(soroban_sdk::vec![
            env,
            soroban_sdk::auth::InvokerContractAuthEntry::Contract(
                soroban_sdk::auth::SubContractInvocation {
                    context: soroban_sdk::auth::ContractContext {
                        contract: token.clone(),
                        fn_name: soroban_sdk::Symbol::new(env, "transfer"),
                        args: (contract_address.clone(), to.clone(), amount).into_val(env),
                    },
                    sub_invocations: soroban_sdk::vec![env],
                },
            ),
        ]);
        VeTokenClient::new(env, &token).transfer(&contract_address, to, &amount);
    }

    fn bump_lock_ttl(env: &Env, key: &VeDataKey, remaining_seconds: u64) -> u32 {
        let max_ttl = env.storage().max_ttl();
        // Estimate ledgers needed for the remaining lock duration plus a 30-day safety buffer,
        // bounded by the network's maximum allowed TTL.
        let required_ledgers_u64 = (remaining_seconds / SECONDS_PER_LEDGER).saturating_add(518_400);
        let target_ttl = core::cmp::min(
            u32::try_from(required_ledgers_u64).unwrap_or(max_ttl),
            max_ttl,
        );
        if target_ttl > 0 {
            env.storage()
                .persistent()
                .extend_ttl(key, target_ttl, target_ttl);
        }
        target_ttl
    }

    fn bump_instance_ttl(env: &Env) {
        let max_ttl = env.storage().max_ttl();
        if max_ttl > 0 {
            env.storage().instance().extend_ttl(max_ttl, max_ttl);
        }
    }
}
