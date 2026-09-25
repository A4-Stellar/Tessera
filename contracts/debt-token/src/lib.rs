#![no_std]
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
    
    pub fn trigger_default(env: Env) {
        env.events().publish((Symbol::new(&env, "default_triggered"),), ());
    }
}
