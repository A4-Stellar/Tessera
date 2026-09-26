//! Token vesting schedules with configurable cliffs and linear vesting.
#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, panic_with_error,
    symbol_short, Address, Env, IntoVal,
};

#[contractclient(name = "TokenClient")]
pub trait TokenInterface {
    fn transfer(env: Env, from: Address, to: Address, amount: i128);
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VestingSchedule {
    pub id: u64,
    pub beneficiary: Address,
    pub start_time: u64,
    pub cliff_duration: u64,
    pub total_duration: u64,
    pub amount: i128,
    pub claimed: i128,
    pub revocable: bool,
    pub revoked_at: Option<u64>,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidAmount = 4,
    InvalidDuration = 5,
    ScheduleNotFound = 6,
    NotBeneficiary = 7,
    NotRevocable = 8,
    AlreadyRevoked = 9,
    Overflow = 10,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    Token,
    Treasury,
    NextId,
    Schedule(u64),
}

#[contract]
pub struct VestingContract;

#[contractimpl]
impl VestingContract {
    /// Initialize the vesting contract with its funding token and refund treasury.
    pub fn initialize(env: Env, admin: Address, token: Address, treasury: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage().instance().set(&DataKey::NextId, &0u64);
    }

    /// Create and fund a schedule. `start_time` and durations are ledger timestamp seconds.
    /// At the cliff, the beneficiary may claim the linear amount accrued since `start_time`.
    pub fn create_vesting_schedule(
        env: Env,
        admin: Address,
        beneficiary: Address,
        start_time: u64,
        cliff_duration: u64,
        total_duration: u64,
        amount: i128,
        revocable: bool,
    ) -> u64 {
        Self::require_admin(&env, &admin);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        if total_duration == 0 || cliff_duration > total_duration {
            panic_with_error!(env, Error::InvalidDuration);
        }
        start_time
            .checked_add(total_duration)
            .and_then(|_| start_time.checked_add(cliff_duration))
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));

        let id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap_or(0);
        let next_id = id
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
        let schedule = VestingSchedule {
            id,
            beneficiary,
            start_time,
            cliff_duration,
            total_duration,
            amount,
            claimed: 0,
            revocable,
            revoked_at: None,
        };

        // Funding is atomic with recording the schedule: a failed token transfer reverts both.
        let token_address: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        TokenClient::new(&env, &token_address).transfer(
            &admin,
            &env.current_contract_address(),
            &amount,
        );

        env.storage().instance().set(&DataKey::NextId, &next_id);
        env.storage()
            .persistent()
            .set(&DataKey::Schedule(id), &schedule);
        env.events().publish(
            (symbol_short!("created"), admin),
            (id, schedule.beneficiary.clone(), amount),
        );
        id
    }

    /// Claim all currently vested but previously unclaimed tokens for one schedule.
    pub fn claim_vested_tokens(env: Env, beneficiary: Address, schedule_id: u64) -> i128 {
        beneficiary.require_auth();
        let mut schedule = Self::schedule(&env, schedule_id);
        if schedule.beneficiary != beneficiary {
            panic_with_error!(env, Error::NotBeneficiary);
        }

        let vested = Self::vested_at(&env, &schedule, env.ledger().timestamp());
        let claimable = vested.saturating_sub(schedule.claimed);
        if claimable > 0 {
            schedule.claimed = schedule
                .claimed
                .checked_add(claimable)
                .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
            Self::transfer_from_contract(&env, &beneficiary, claimable);
            env.storage()
                .persistent()
                .set(&DataKey::Schedule(schedule_id), &schedule);
            env.events().publish(
                (symbol_short!("claimed"), beneficiary),
                (schedule_id, claimable),
            );
        }
        claimable
    }

    /// Revoke an active schedule and return its unvested balance to the treasury.
    /// Any vested amount remains claimable by the beneficiary.
    pub fn revoke_vesting_schedule(env: Env, admin: Address, schedule_id: u64) -> i128 {
        Self::require_admin(&env, &admin);
        let mut schedule = Self::schedule(&env, schedule_id);
        if !schedule.revocable {
            panic_with_error!(env, Error::NotRevocable);
        }
        if schedule.revoked_at.is_some() {
            panic_with_error!(env, Error::AlreadyRevoked);
        }

        let now = env.ledger().timestamp();
        let vested = Self::vested_at(&env, &schedule, now);
        let refund = schedule.amount - vested;
        schedule.revoked_at = Some(now);
        env.storage()
            .persistent()
            .set(&DataKey::Schedule(schedule_id), &schedule);
        if refund > 0 {
            let treasury: Address = env.storage().instance().get(&DataKey::Treasury).unwrap();
            Self::transfer_from_contract(&env, &treasury, refund);
        }
        env.events()
            .publish((symbol_short!("revoked"), admin), (schedule_id, refund));
        refund
    }

    pub fn get_vesting_schedule(env: Env, schedule_id: u64) -> VestingSchedule {
        Self::schedule(&env, schedule_id)
    }

    pub fn version(_env: Env) -> u64 {
        1
    }

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

    fn schedule(env: &Env, id: u64) -> VestingSchedule {
        env.storage()
            .persistent()
            .get(&DataKey::Schedule(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::ScheduleNotFound))
    }

    fn vested_at(env: &Env, schedule: &VestingSchedule, now: u64) -> i128 {
        let effective_now = schedule
            .revoked_at
            .map(|at| core::cmp::min(now, at))
            .unwrap_or(now);
        let cliff_at = schedule
            .start_time
            .checked_add(schedule.cliff_duration)
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
        if effective_now < cliff_at || effective_now < schedule.start_time {
            return 0;
        }
        let elapsed = core::cmp::min(effective_now - schedule.start_time, schedule.total_duration);
        let numerator = schedule
            .amount
            .checked_mul(elapsed as i128)
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
        numerator / schedule.total_duration as i128
    }

    fn transfer_from_contract(env: &Env, to: &Address, amount: i128) {
        let token_address: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let contract_address = env.current_contract_address();
        env.authorize_as_current_contract(soroban_sdk::vec![
            env,
            soroban_sdk::auth::InvokerContractAuthEntry::Contract(
                soroban_sdk::auth::SubContractInvocation {
                    context: soroban_sdk::auth::ContractContext {
                        contract: token_address.clone(),
                        fn_name: soroban_sdk::Symbol::new(env, "transfer"),
                        args: (contract_address.clone(), to.clone(), amount).into_val(env),
                    },
                    sub_invocations: soroban_sdk::vec![env],
                },
            ),
        ]);
        TokenClient::new(env, &token_address).transfer(&contract_address, to, &amount);
    }
}

#[cfg(test)]
mod test;
