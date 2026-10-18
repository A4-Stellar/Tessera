//! Automated Regulatory Cap-Table Stock Option & Token Grant Vesting Vault.
//!
//! Supports employee token option grants combining:
//! 1. **Standard Cliff & Linear Time-Based Vesting**: Time-based option tranche
//!    accrues linearly between `start_time` and `start_time + total_duration`
//!    once `start_time + cliff_duration` is reached.
//! 2. **Performance-Milestone Vesting Triggers**: Discrete milestone tranches
//!    unlock via `vest_milestone(admin, grant_id, milestone_index)`.
//! 3. **Strike Price Exercise Settlement**: Employees convert vested options
//!    into active tokenized equity shares by paying `option_amount * strike_price`
//!    in the configured settlement payment token to the corporate treasury.
//! 4. **Automated Termination & Expiration Clawback**: Upon employee
//!    termination (`terminate_employee`) or option expiry (`clawback_expired_options`),
//!    all unvested or expired unexercised equity tokens are automatically
//!    clawed back and returned to the corporate treasury.

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype,
    panic_with_error, Address, Env, IntoVal, Symbol, Vec,
};

#[contractclient(name = "GrantTokenClient")]
pub trait TokenInterface {
    fn transfer(env: Env, from: Address, to: Address, amount: i128);
    fn balance(env: Env, id: Address) -> i128;
}

/// Represents an individual performance milestone within an option grant.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MilestoneTranche {
    pub index: u32,
    pub share_amount: i128,
    pub vested: bool,
    pub vested_at: Option<u64>,
}

/// Complete state of an employee stock option / token grant.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptionGrant {
    pub grant_id: u64,
    pub employee: Address,
    pub start_time: u64,
    pub cliff_duration: u64,
    pub total_duration: u64,
    pub expiration_time: u64,
    /// Options subject to standard cliff + linear time-based schedule.
    pub time_based_options: i128,
    /// Options subject to performance milestone triggers.
    pub milestones: Vec<MilestoneTranche>,
    /// Total options across both time-based and milestone tranches.
    pub total_options: i128,
    /// Cumulative milestone options unlocked so far.
    pub vested_milestone_options: i128,
    /// Cumulative options already exercised into equity shares.
    pub exercised_options: i128,
    /// Cumulative options clawed back to the treasury.
    pub clawed_back_options: i128,
    /// Strike price in `payment_token` base units per option share.
    pub strike_price: i128,
    /// Timestamp at which the employee was terminated (freezes further vesting).
    pub terminated_at: Option<u64>,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum GrantError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidAmount = 4,
    InvalidDuration = 5,
    InvalidStrikePrice = 6,
    GrantNotFound = 7,
    NotEmployee = 8,
    MilestoneOutOfBounds = 9,
    MilestoneAlreadyVested = 10,
    EmployeeTerminated = 11,
    OptionsExpired = 12,
    OptionsNotExpired = 13,
    InsufficientVestedOptions = 14,
    NothingToClawback = 15,
    Overflow = 16,
}

#[derive(Clone)]
#[contracttype]
enum GrantDataKey {
    Admin,
    EquityToken,
    PaymentToken,
    Treasury,
    NextGrantId,
    Grant(u64),
}

#[contractevent]
pub struct GrantCreated {
    #[topic]
    pub grant_id: u64,
    #[topic]
    pub employee: Address,
    pub total_options: i128,
    pub strike_price: i128,
}

#[contractevent]
pub struct MilestoneVested {
    #[topic]
    pub grant_id: u64,
    pub milestone_index: u32,
    pub share_amount: i128,
}

#[contractevent]
pub struct OptionsExercised {
    #[topic]
    pub grant_id: u64,
    #[topic]
    pub employee: Address,
    pub option_amount: i128,
    pub exercise_cost: i128,
}

#[contractevent]
pub struct OptionsClawedBack {
    #[topic]
    pub grant_id: u64,
    pub clawed_back_amount: i128,
    pub reason: Symbol,
}

#[contract]
pub struct GrantVaultContract;

#[contractimpl]
impl GrantVaultContract {
    /// Initialize the option & token grant vault with its admin, underlying
    /// equity token, exercise payment token, and corporate treasury.
    pub fn initialize_grant_vault(
        env: Env,
        admin: Address,
        equity_token: Address,
        payment_token: Address,
        treasury: Address,
    ) {
        if env.storage().instance().has(&GrantDataKey::Admin) {
            panic_with_error!(env, GrantError::AlreadyInitialized);
        }
        admin.require_auth();

        env.storage().instance().set(&GrantDataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&GrantDataKey::EquityToken, &equity_token);
        env.storage()
            .instance()
            .set(&GrantDataKey::PaymentToken, &payment_token);
        env.storage()
            .instance()
            .set(&GrantDataKey::Treasury, &treasury);
        env.storage()
            .instance()
            .set(&GrantDataKey::NextGrantId, &0u64);
    }

    /// Create and escrow a new employee stock option & milestone grant.
    #[allow(clippy::too_many_arguments)]
    pub fn create_option_grant(
        env: Env,
        admin: Address,
        employee: Address,
        start_time: u64,
        cliff_duration: u64,
        total_duration: u64,
        expiration_time: u64,
        time_based_options: i128,
        milestone_shares: Vec<i128>,
        strike_price: i128,
    ) -> u64 {
        Self::require_admin(&env, &admin);

        if time_based_options < 0 {
            panic_with_error!(env, GrantError::InvalidAmount);
        }
        if strike_price < 0 {
            panic_with_error!(env, GrantError::InvalidStrikePrice);
        }
        if time_based_options > 0 && (total_duration == 0 || cliff_duration > total_duration) {
            panic_with_error!(env, GrantError::InvalidDuration);
        }

        let end_time = start_time
            .checked_add(total_duration)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));
        if expiration_time <= end_time {
            panic_with_error!(env, GrantError::InvalidDuration);
        }

        let mut milestones = Vec::new(&env);
        let mut milestone_total: i128 = 0;
        for (idx, share_amount) in milestone_shares.iter().enumerate() {
            if share_amount <= 0 {
                panic_with_error!(env, GrantError::InvalidAmount);
            }
            milestone_total = milestone_total
                .checked_add(share_amount)
                .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));
            milestones.push_back(MilestoneTranche {
                index: idx as u32,
                share_amount,
                vested: false,
                vested_at: None,
            });
        }

        let total_options = time_based_options
            .checked_add(milestone_total)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));
        if total_options <= 0 {
            panic_with_error!(env, GrantError::InvalidAmount);
        }

        let grant_id: u64 = env
            .storage()
            .instance()
            .get(&GrantDataKey::NextGrantId)
            .unwrap_or(0);
        let next_id = grant_id
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));

        let grant = OptionGrant {
            grant_id,
            employee: employee.clone(),
            start_time,
            cliff_duration,
            total_duration,
            expiration_time,
            time_based_options,
            milestones,
            total_options,
            vested_milestone_options: 0,
            exercised_options: 0,
            clawed_back_options: 0,
            strike_price,
            terminated_at: None,
        };

        // CEI: persist grant state prior to pulling escrowed equity shares from admin.
        env.storage()
            .instance()
            .set(&GrantDataKey::NextGrantId, &next_id);
        env.storage()
            .persistent()
            .set(&GrantDataKey::Grant(grant_id), &grant);

        let equity_token = Self::equity_token(&env);
        GrantTokenClient::new(&env, &equity_token).transfer(
            &admin,
            &env.current_contract_address(),
            &total_options,
        );

        GrantCreated {
            grant_id,
            employee,
            total_options,
            strike_price,
        }
        .publish(&env);

        grant_id
    }

    /// Trigger vesting for a performance milestone (`milestone_index`) on `grant_id`.
    /// Returns the number of option shares unlocked by this milestone.
    pub fn vest_milestone(
        env: Env,
        admin: Address,
        grant_id: u64,
        milestone_index: u32,
    ) -> i128 {
        Self::require_admin(&env, &admin);

        let mut grant = Self::load_grant(&env, grant_id);
        if grant.terminated_at.is_some() {
            panic_with_error!(env, GrantError::EmployeeTerminated);
        }
        let now = env.ledger().timestamp();
        if now >= grant.expiration_time {
            panic_with_error!(env, GrantError::OptionsExpired);
        }
        if milestone_index >= grant.milestones.len() {
            panic_with_error!(env, GrantError::MilestoneOutOfBounds);
        }

        let mut tranche = grant.milestones.get(milestone_index).unwrap();
        if tranche.vested {
            panic_with_error!(env, GrantError::MilestoneAlreadyVested);
        }

        tranche.vested = true;
        tranche.vested_at = Some(now);
        grant.milestones.set(milestone_index, tranche.clone());
        grant.vested_milestone_options = grant
            .vested_milestone_options
            .checked_add(tranche.share_amount)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));

        env.storage()
            .persistent()
            .set(&GrantDataKey::Grant(grant_id), &grant);

        MilestoneVested {
            grant_id,
            milestone_index,
            share_amount: tranche.share_amount,
        }
        .publish(&env);

        tranche.share_amount
    }

    /// Exercise `option_amount` of vested options into active tokenized equity shares
    /// by paying `option_amount * strike_price` in the settlement payment token to the treasury.
    ///
    /// Returns the total exercise cost paid.
    pub fn exercise_options(
        env: Env,
        employee: Address,
        grant_id: u64,
        option_amount: i128,
    ) -> i128 {
        employee.require_auth();
        if option_amount <= 0 {
            panic_with_error!(env, GrantError::InvalidAmount);
        }

        let mut grant = Self::load_grant(&env, grant_id);
        if grant.employee != employee {
            panic_with_error!(env, GrantError::NotEmployee);
        }

        let now = env.ledger().timestamp();
        if now >= grant.expiration_time {
            panic_with_error!(env, GrantError::OptionsExpired);
        }

        let exercisable = Self::compute_exercisable(&env, &grant, now);
        if option_amount > exercisable {
            panic_with_error!(env, GrantError::InsufficientVestedOptions);
        }

        let exercise_cost = grant
            .strike_price
            .checked_mul(option_amount)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));

        grant.exercised_options = grant
            .exercised_options
            .checked_add(option_amount)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));

        // CEI: persist updated exercise state before external token transfers.
        env.storage()
            .persistent()
            .set(&GrantDataKey::Grant(grant_id), &grant);

        if exercise_cost > 0 {
            let payment_token = Self::payment_token(&env);
            let treasury = Self::treasury(&env);
            GrantTokenClient::new(&env, &payment_token).transfer(
                &employee,
                &treasury,
                &exercise_cost,
            );
        }

        let equity_token = Self::equity_token(&env);
        Self::transfer_from_vault(&env, &equity_token, &employee, option_amount);

        OptionsExercised {
            grant_id,
            employee,
            option_amount,
            exercise_cost,
        }
        .publish(&env);

        exercise_cost
    }

    /// Record an employee termination event: freezes vesting at the current
    /// timestamp and automatically claws back all unvested options (or all
    /// remaining options if already expired) to the corporate treasury.
    ///
    /// Returns the number of option shares clawed back to the treasury.
    pub fn terminate_employee(env: Env, admin: Address, grant_id: u64) -> i128 {
        Self::require_admin(&env, &admin);

        let mut grant = Self::load_grant(&env, grant_id);
        if grant.terminated_at.is_some() {
            panic_with_error!(env, GrantError::EmployeeTerminated);
        }

        let now = env.ledger().timestamp();
        grant.terminated_at = Some(now);

        let total_vested = if now >= grant.expiration_time {
            // Grant already expired: all unexercised options are clawed back
            grant.exercised_options
        } else {
            Self::compute_total_vested(&env, &grant, now)
        };

        let unvested = grant
            .total_options
            .saturating_sub(total_vested)
            .saturating_sub(grant.clawed_back_options);

        grant.clawed_back_options = grant
            .clawed_back_options
            .checked_add(unvested)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));

        // CEI: persist termination and clawback accounting before external token transfer.
        env.storage()
            .persistent()
            .set(&GrantDataKey::Grant(grant_id), &grant);

        if unvested > 0 {
            let equity_token = Self::equity_token(&env);
            let treasury = Self::treasury(&env);
            Self::transfer_from_vault(&env, &equity_token, &treasury, unvested);
        }

        OptionsClawedBack {
            grant_id,
            clawed_back_amount: unvested,
            reason: Symbol::new(&env, "terminated"),
        }
        .publish(&env);

        unvested
    }

    /// Claw back any remaining unexercised options after `expiration_time`
    /// back to the corporate treasury.
    pub fn clawback_expired_options(env: Env, admin: Address, grant_id: u64) -> i128 {
        Self::require_admin(&env, &admin);

        let mut grant = Self::load_grant(&env, grant_id);
        let now = env.ledger().timestamp();
        if now < grant.expiration_time {
            panic_with_error!(env, GrantError::OptionsNotExpired);
        }

        let remaining = grant
            .total_options
            .saturating_sub(grant.exercised_options)
            .saturating_sub(grant.clawed_back_options);
        if remaining <= 0 {
            panic_with_error!(env, GrantError::NothingToClawback);
        }

        grant.clawed_back_options = grant
            .clawed_back_options
            .checked_add(remaining)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));

        // CEI: update grant state before transferring clawed-back tokens.
        env.storage()
            .persistent()
            .set(&GrantDataKey::Grant(grant_id), &grant);

        let equity_token = Self::equity_token(&env);
        let treasury = Self::treasury(&env);
        Self::transfer_from_vault(&env, &equity_token, &treasury, remaining);

        OptionsClawedBack {
            grant_id,
            clawed_back_amount: remaining,
            reason: Symbol::new(&env, "expired"),
        }
        .publish(&env);

        remaining
    }

    // ------------------------------------------------------------------
    // Views
    // ------------------------------------------------------------------

    /// Calculate the total exercise payment required to convert `option_amount`
    /// vested options into equity shares for `grant_id`.
    pub fn calculate_exercise_cost(env: Env, grant_id: u64, option_amount: i128) -> i128 {
        if option_amount <= 0 {
            panic_with_error!(env, GrantError::InvalidAmount);
        }
        let grant = Self::load_grant(&env, grant_id);
        grant
            .strike_price
            .checked_mul(option_amount)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow))
    }

    /// Total vested options (time-based + milestone) for `grant_id` at the current timestamp.
    pub fn vested_options(env: Env, grant_id: u64) -> i128 {
        let grant = Self::load_grant(&env, grant_id);
        Self::compute_total_vested(&env, &grant, env.ledger().timestamp())
    }

    /// Currently exercisable (vested minus already exercised) options for `grant_id`.
    pub fn exercisable_options(env: Env, grant_id: u64) -> i128 {
        let grant = Self::load_grant(&env, grant_id);
        let now = env.ledger().timestamp();
        if now >= grant.expiration_time {
            return 0;
        }
        Self::compute_exercisable(&env, &grant, now)
    }

    /// Retrieve the full `OptionGrant` record for `grant_id`.
    pub fn get_option_grant(env: Env, grant_id: u64) -> OptionGrant {
        Self::load_grant(&env, grant_id)
    }

    // ------------------------------------------------------------------
    // Internal Helpers
    // ------------------------------------------------------------------

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&GrantDataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::NotInitialized));
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, GrantError::Unauthorized);
        }
    }

    fn equity_token(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&GrantDataKey::EquityToken)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::NotInitialized))
    }

    fn payment_token(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&GrantDataKey::PaymentToken)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::NotInitialized))
    }

    fn treasury(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&GrantDataKey::Treasury)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::NotInitialized))
    }

    fn load_grant(env: &Env, grant_id: u64) -> OptionGrant {
        env.storage()
            .persistent()
            .get(&GrantDataKey::Grant(grant_id))
            .unwrap_or_else(|| panic_with_error!(env, GrantError::GrantNotFound))
    }

    fn compute_time_vested(env: &Env, grant: &OptionGrant, now: u64) -> i128 {
        if grant.time_based_options == 0 || grant.total_duration == 0 {
            return 0;
        }
        let effective_now = grant
            .terminated_at
            .map(|t| core::cmp::min(now, t))
            .unwrap_or(now);
        let cliff_at = grant
            .start_time
            .checked_add(grant.cliff_duration)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));
        if effective_now < cliff_at || effective_now < grant.start_time {
            return 0;
        }
        let elapsed = core::cmp::min(effective_now - grant.start_time, grant.total_duration);
        let numerator = grant
            .time_based_options
            .checked_mul(elapsed as i128)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow));
        numerator / (grant.total_duration as i128)
    }

    fn compute_total_vested(env: &Env, grant: &OptionGrant, now: u64) -> i128 {
        let time_vested = Self::compute_time_vested(env, grant, now);
        time_vested
            .checked_add(grant.vested_milestone_options)
            .unwrap_or_else(|| panic_with_error!(env, GrantError::Overflow))
    }

    fn compute_exercisable(env: &Env, grant: &OptionGrant, now: u64) -> i128 {
        let vested = Self::compute_total_vested(env, grant, now);
        vested.saturating_sub(grant.exercised_options)
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
        GrantTokenClient::new(env, token).transfer(&contract_address, to, &amount);
    }
}
