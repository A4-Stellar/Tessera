#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, token, Error};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    TokenA,
    TokenB,
    ReserveA,
    ReserveB,
    ComplianceContract,
}

#[contract]
pub struct ComplianceAmm;

#[contractimpl]
impl ComplianceAmm {
    pub fn initialize(env: Env, token_a: Address, token_b: Address, compliance_contract: Address) {
        if env.storage().instance().has(&DataKey::TokenA) {
            panic!("already initialized");
        }
        env.storage().instance().set(&DataKey::TokenA, &token_a);
        env.storage().instance().set(&DataKey::TokenB, &token_b);
        env.storage().instance().set(&DataKey::ReserveA, &0i128);
        env.storage().instance().set(&DataKey::ReserveB, &0i128);
        env.storage().instance().set(&DataKey::ComplianceContract, &compliance_contract);
    }

    fn check_compliance(env: &Env, address: &Address) {
        let compliance_contract: Address = env.storage().instance().get(&DataKey::ComplianceContract).unwrap();
        // Assume compliance contract has an `is_allowed` method
        let is_allowed: bool = env.invoke_contract(
            &compliance_contract,
            &soroban_sdk::Symbol::new(env, "is_allowed"),
            soroban_sdk::vec![env, address.to_val()],
        );
        if !is_allowed {
            panic!("Compliance check failed for address");
        }
    }

    pub fn add_liquidity(env: Env, to: Address, amount_a: i128, amount_b: i128) -> (i128, i128) {
        to.require_auth();
        Self::check_compliance(&env, &to);

        let token_a: Address = env.storage().instance().get(&DataKey::TokenA).unwrap();
        let token_b: Address = env.storage().instance().get(&DataKey::TokenB).unwrap();
        
        let client_a = token::Client::new(&env, &token_a);
        let client_b = token::Client::new(&env, &token_b);
        
        client_a.transfer(&to, &env.current_contract_address(), &amount_a);
        client_b.transfer(&to, &env.current_contract_address(), &amount_b);

        let reserve_a: i128 = env.storage().instance().get(&DataKey::ReserveA).unwrap();
        let reserve_b: i128 = env.storage().instance().get(&DataKey::ReserveB).unwrap();

        env.storage().instance().set(&DataKey::ReserveA, &(reserve_a + amount_a));
        env.storage().instance().set(&DataKey::ReserveB, &(reserve_b + amount_b));

        (amount_a, amount_b)
    }

    pub fn remove_liquidity(env: Env, to: Address, amount_a: i128, amount_b: i128) {
        to.require_auth();
        Self::check_compliance(&env, &to);

        let token_a: Address = env.storage().instance().get(&DataKey::TokenA).unwrap();
        let token_b: Address = env.storage().instance().get(&DataKey::TokenB).unwrap();
        
        let client_a = token::Client::new(&env, &token_a);
        let client_b = token::Client::new(&env, &token_b);

        client_a.transfer(&env.current_contract_address(), &to, &amount_a);
        client_b.transfer(&env.current_contract_address(), &to, &amount_b);

        let reserve_a: i128 = env.storage().instance().get(&DataKey::ReserveA).unwrap();
        let reserve_b: i128 = env.storage().instance().get(&DataKey::ReserveB).unwrap();

        env.storage().instance().set(&DataKey::ReserveA, &(reserve_a - amount_a));
        env.storage().instance().set(&DataKey::ReserveB, &(reserve_b - amount_b));
    }

    pub fn swap(env: Env, to: Address, buy_token: Address, sell_amount: i128) -> i128 {
        to.require_auth();
        Self::check_compliance(&env, &to);

        let token_a: Address = env.storage().instance().get(&DataKey::TokenA).unwrap();
        let token_b: Address = env.storage().instance().get(&DataKey::TokenB).unwrap();

        let (sell_token, is_a_for_b) = if buy_token == token_b {
            (token_a.clone(), true)
        } else if buy_token == token_a {
            (token_b.clone(), false)
        } else {
            panic!("Invalid token");
        };

        let reserve_a: i128 = env.storage().instance().get(&DataKey::ReserveA).unwrap();
        let reserve_b: i128 = env.storage().instance().get(&DataKey::ReserveB).unwrap();

        let (reserve_sell, reserve_buy) = if is_a_for_b {
            (reserve_a, reserve_b)
        } else {
            (reserve_b, reserve_a)
        };

        let fee_numerator = 997i128;
        let fee_denominator = 1000i128;
        let sell_amount_with_fee = sell_amount * fee_numerator;
        
        let numerator = sell_amount_with_fee * reserve_buy;
        let denominator = (reserve_sell * fee_denominator) + sell_amount_with_fee;
        let buy_amount = numerator / denominator;

        if buy_amount <= 0 {
            panic!("Insufficient output amount");
        }

        let sell_client = token::Client::new(&env, &sell_token);
        sell_client.transfer(&to, &env.current_contract_address(), &sell_amount);

        let buy_client = token::Client::new(&env, &buy_token);
        buy_client.transfer(&env.current_contract_address(), &to, &buy_amount);

        let (new_reserve_a, new_reserve_b) = if is_a_for_b {
            (reserve_a + sell_amount, reserve_b - buy_amount)
        } else {
            (reserve_a - buy_amount, reserve_b + sell_amount)
        };

        env.storage().instance().set(&DataKey::ReserveA, &new_reserve_a);
        env.storage().instance().set(&DataKey::ReserveB, &new_reserve_b);

        buy_amount
    }
}
