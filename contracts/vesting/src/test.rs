use super::*;
use soroban_sdk::{contract, contractimpl, contracttype, testutils::Address as _, Env};

#[derive(Clone)]
#[contracttype]
enum TokenKey {
    Balance(Address),
}

#[contract]
struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn mint(env: Env, to: Address, amount: i128) {
        let balance: i128 = env
            .storage()
            .persistent()
            .get(&TokenKey::Balance(to.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(to), &(balance + amount));
    }

    pub fn balance(env: Env, owner: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&TokenKey::Balance(owner))
            .unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        let from_balance: i128 = env
            .storage()
            .persistent()
            .get(&TokenKey::Balance(from.clone()))
            .unwrap_or(0);
        assert!(from_balance >= amount);
        let to_balance: i128 = env
            .storage()
            .persistent()
            .get(&TokenKey::Balance(to.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(from), &(from_balance - amount));
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(to), &(to_balance + amount));
    }
}

fn setup() -> (Env, Address, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let treasury = Address::generate(&env);
    let vesting_id = env.register(VestingContract, ());
    let token_id = env.register(MockToken, ());
    let vesting = VestingContractClient::new(&env, &vesting_id);
    let token = MockTokenClient::new(&env, &token_id);
    token.mint(&admin, &1_000);
    vesting.initialize(&admin, &token_id, &treasury);
    (env, admin, beneficiary, treasury, vesting_id, token_id)
}

fn set_time(env: &Env, timestamp: u64) {
    env.ledger().with_mut(|ledger| ledger.timestamp = timestamp);
}

#[test]
fn linear_vesting_respects_cliff_and_tracks_claims() {
    let (env, admin, beneficiary, treasury, vesting_id, token_id) = setup();
    let vesting = VestingContractClient::new(&env, &vesting_id);
    let token = MockTokenClient::new(&env, &token_id);
    let start = env.ledger().timestamp();
    let id = vesting.create_vesting_schedule(&admin, &beneficiary, &start, &25, &100, &400, &true);
    assert_eq!(token.balance(&vesting_id), 400);

    set_time(&env, start + 24);
    assert_eq!(vesting.claim_vested_tokens(&beneficiary, &id), 0);
    set_time(&env, start + 25);
    assert_eq!(vesting.claim_vested_tokens(&beneficiary, &id), 100);
    set_time(&env, start + 50);
    assert_eq!(vesting.claim_vested_tokens(&beneficiary, &id), 100);
    assert_eq!(vesting.claim_vested_tokens(&beneficiary, &id), 0);
    assert_eq!(token.balance(&beneficiary), 200);
    assert_eq!(token.balance(&treasury), 0);
}

#[test]
fn revocation_refunds_only_unvested_amount_and_freezes_vesting() {
    let (env, admin, beneficiary, treasury, vesting_id, token_id) = setup();
    let vesting = VestingContractClient::new(&env, &vesting_id);
    let token = MockTokenClient::new(&env, &token_id);
    let start = env.ledger().timestamp();
    let id = vesting.create_vesting_schedule(&admin, &beneficiary, &start, &0, &100, &1_000, &true);

    set_time(&env, start + 40);
    assert_eq!(vesting.revoke_vesting_schedule(&admin, &id), 600);
    assert_eq!(token.balance(&treasury), 600);
    set_time(&env, start + 90);
    assert_eq!(vesting.claim_vested_tokens(&beneficiary, &id), 400);
    assert_eq!(token.balance(&beneficiary), 400);
    assert_eq!(token.balance(&vesting_id), 0);
}

#[test]
fn future_start_and_full_duration_claims_are_bounded() {
    let (env, admin, beneficiary, _treasury, vesting_id, token_id) = setup();
    let vesting = VestingContractClient::new(&env, &vesting_id);
    let token = MockTokenClient::new(&env, &token_id);
    let now = env.ledger().timestamp();
    let id =
        vesting.create_vesting_schedule(&admin, &beneficiary, &(now + 50), &0, &100, &400, &false);
    set_time(&env, now + 49);
    assert_eq!(vesting.claim_vested_tokens(&beneficiary, &id), 0);
    set_time(&env, now + 200);
    assert_eq!(vesting.claim_vested_tokens(&beneficiary, &id), 400);
    assert_eq!(token.balance(&beneficiary), 400);
}
