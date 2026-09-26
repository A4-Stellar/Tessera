#![no_std]
pub mod interest_rate;

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Symbol};

#[contracttype]
#[derive(Clone)]
pub enum Tranche {
    Senior,
    Mezzanine,
    Equity,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    SeniorOwed,
    MezzanineOwed,
    EquityOwed,
}

#[contract]
pub struct DebtTokenAmortization;

#[contractimpl]
impl DebtTokenAmortization {
    pub fn deposit_repayment(env: Env, borrower: Address, amount: i128) {
        borrower.require_auth();
        
        let mut remaining = amount;
        
        let senior_owed: i128 = env.storage().instance().get(&DataKey::SeniorOwed).unwrap_or(0);
        let senior_payment = if remaining > senior_owed { senior_owed } else { remaining };
        remaining -= senior_payment;
        env.storage().instance().set(&DataKey::SeniorOwed, &(senior_owed - senior_payment));
        if senior_payment > 0 {
            env.events().publish((Symbol::new(&env, "repayment"), Tranche::Senior), senior_payment);
        }

        let mezzanine_owed: i128 = env.storage().instance().get(&DataKey::MezzanineOwed).unwrap_or(0);
        let mezzanine_payment = if remaining > mezzanine_owed { mezzanine_owed } else { remaining };
        remaining -= mezzanine_payment;
        env.storage().instance().set(&DataKey::MezzanineOwed, &(mezzanine_owed - mezzanine_payment));
        if mezzanine_payment > 0 {
            env.events().publish((Symbol::new(&env, "repayment"), Tranche::Mezzanine), mezzanine_payment);
        }

        let equity_owed: i128 = env.storage().instance().get(&DataKey::EquityOwed).unwrap_or(0);
        let equity_payment = remaining; 
        env.storage().instance().set(&DataKey::EquityOwed, &(equity_owed - equity_payment));
        if equity_payment > 0 {
            env.events().publish((Symbol::new(&env, "repayment"), Tranche::Equity), equity_payment);
        }
    }

    pub fn accrue_interest(env: Env, tranche: Tranche, amount: i128) {
        match tranche {
            Tranche::Senior => {
                let owed: i128 = env.storage().instance().get(&DataKey::SeniorOwed).unwrap_or(0);
                env.storage().instance().set(&DataKey::SeniorOwed, &(owed + amount));
            },
            Tranche::Mezzanine => {
                let owed: i128 = env.storage().instance().get(&DataKey::MezzanineOwed).unwrap_or(0);
                env.storage().instance().set(&DataKey::MezzanineOwed, &(owed + amount));
            },
            Tranche::Equity => {
                let owed: i128 = env.storage().instance().get(&DataKey::EquityOwed).unwrap_or(0);
                env.storage().instance().set(&DataKey::EquityOwed, &(owed + amount));
            },
        }
        env.events().publish((Symbol::new(&env, "interest_accrued"), tranche), amount);
    }
    
    /// Issue #85: set kinked-curve parameters (first caller becomes rate admin).
    pub fn set_rate_params(env: Env, admin: Address, params: interest_rate::RateParams) {
        interest_rate::do_set_rate_params(&env, admin, params);
    }

    pub fn get_rate_params(env: Env) -> interest_rate::RateParams {
        interest_rate::get_params(&env)
    }

    /// Annual borrow rate (WAD) for the given pool balances.
    pub fn current_borrow_rate(env: Env, borrowed: i128, available: i128) -> i128 {
        let u = interest_rate::utilization(&env, borrowed, available);
        interest_rate::borrow_rate(&env, &interest_rate::get_params(&env), u)
    }

    /// Pool utilization (WAD).
    pub fn pool_utilization(env: Env, borrowed: i128, available: i128) -> i128 {
        interest_rate::utilization(&env, borrowed, available)
    }

    /// Accrue the WAD borrow index to the current ledger time; returns it.
    pub fn accrue_rate_index(env: Env, borrowed: i128, available: i128) -> i128 {
        interest_rate::do_accrue_index(&env, borrowed, available)
    }

    pub fn trigger_default(env: Env) {
        env.events().publish((Symbol::new(&env, "default_triggered"),), ());
    }
}
