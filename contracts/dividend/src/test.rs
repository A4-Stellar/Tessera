use crate::{DividendContract, DividendContractClient};
use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, Address, Env, IntoVal, Symbol,
    Val, Vec,
};
use tessera_cap_table::{CapTableContract, CapTableContractClient};

#[derive(Clone)]
#[contracttype]
enum TokenKey {
    Supply,
    Balance(Address),
}

#[contract]
struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn initialize(env: Env, owner: Address, supply: i128, balance: i128) {
        env.storage().instance().set(&TokenKey::Supply, &supply);
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(owner), &balance);
    }

    pub fn balance(env: Env, holder: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&TokenKey::Balance(holder))
            .unwrap_or(0)
    }

    pub fn total_supply(env: Env) -> i128 {
        env.storage().instance().get(&TokenKey::Supply).unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        let from_balance = Self::balance(env.clone(), from.clone());
        let to_balance = Self::balance(env.clone(), to.clone());
        assert!(amount > 0 && from_balance >= amount);
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(from), &(from_balance - amount));
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(to), &(to_balance + amount));
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let balance = Self::balance(env.clone(), to.clone());
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(to), &(balance + amount));
    }
}

#[contract]
struct MockAmm;

#[contractimpl]
impl MockAmm {
    pub fn swap_exact_in(
        env: Env,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
        min_amount_out: i128,
        payer: Address,
        recipient: Address,
    ) -> i128 {
        let amm = env.current_contract_address();
        let args: Vec<Val> = (payer, amm, amount_in).into_val(&env);
        let _: () = env.invoke_contract(&token_in, &Symbol::new(&env, "transfer"), args);

        let amount_out = amount_in * 2;
        assert!(amount_out >= min_amount_out);
        let args: Vec<Val> = (recipient, amount_out).into_val(&env);
        let _: () = env.invoke_contract(&token_out, &Symbol::new(&env, "mint"), args);
        amount_out
    }
}

#[test]
fn opted_in_claim_swaps_and_records_acquisition() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let holder = Address::generate(&env);
    let stable = env.register(MockToken, ());
    let asset = env.register(MockToken, ());
    let amm = env.register(MockAmm, ());
    let cap_table_id = env.register(CapTableContract, ());
    let cap_table = CapTableContractClient::new(&env, &cap_table_id);

    MockTokenClient::new(&env, &stable).initialize(&admin, &10_000, &10_000);
    MockTokenClient::new(&env, &asset).initialize(&holder, &10_000, &1_000);

    let dividend_id = env.register(DividendContract, ());
    let dividend = DividendContractClient::new(&env, &dividend_id);
    dividend.initialize(&admin);
    cap_table.initialize(&admin);
    cap_table.set_drip_registrar(&admin, &dividend_id);
    dividend.configure_drip(&admin, &amm, &cap_table_id);
    let distribution_id = dividend.create_distribution(&admin, &asset, &stable, &100);
    dividend.set_drip_preference(&holder, &true);

    dividend.claim(&distribution_id, &holder);

    assert_eq!(MockTokenClient::new(&env, &asset).balance(&holder), 1_200);
    assert_eq!(cap_table.drip_acquisitions(&asset, &holder), 200);
    assert!(dividend.has_claimed(&distribution_id, &holder));
    assert_eq!(dividend.get_distribution(&distribution_id).distributed, 100);
}

#[test]
fn preference_is_opt_in_by_default_and_can_be_disabled() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let holder = Address::generate(&env);
    let contract = env.register(DividendContract, ());
    let client = DividendContractClient::new(&env, &contract);
    client.initialize(&admin);

    assert!(!client.get_drip_preference(&holder));
    client.set_drip_preference(&holder, &true);
    assert!(client.get_drip_preference(&holder));
    client.set_drip_preference(&holder, &false);
    assert!(!client.get_drip_preference(&holder));
}
