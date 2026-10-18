use super::*;
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Ledger as _},
    Env,
};

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

// ========================================================================== //
// GrantVaultContract (Milestone + Option Exercise + Auto-Clawback) Tests
// ========================================================================== //

mod grant_vault_tests {
    use super::*;
    use crate::grant_vault::{GrantError, GrantVaultContract, GrantVaultContractClient};
    use soroban_sdk::vec;

    struct GrantSetup {
        env: Env,
        admin: Address,
        employee: Address,
        treasury: Address,
        vault_id: Address,
        vault: GrantVaultContractClient<'static>,
        equity: MockTokenClient<'static>,
        payment: MockTokenClient<'static>,
    }

    fn setup_grant() -> GrantSetup {
        let env = Env::default();
        env.mock_all_auths();
        set_time(&env, 1_000);

        let admin = Address::generate(&env);
        let employee = Address::generate(&env);
        let treasury = Address::generate(&env);

        let vault_id = env.register(GrantVaultContract, ());
        let equity_id = env.register(MockToken, ());
        let payment_id = env.register(MockToken, ());

        let vault = GrantVaultContractClient::new(&env, &vault_id);
        let equity = MockTokenClient::new(&env, &equity_id);
        let payment = MockTokenClient::new(&env, &payment_id);

        equity.mint(&admin, &100_000);
        payment.mint(&employee, &500_000);

        vault.initialize_grant_vault(&admin, &equity_id, &payment_id, &treasury);

        GrantSetup {
            env,
            admin,
            employee,
            treasury,
            vault_id,
            vault,
            equity,
            payment,
        }
    }

    #[test]
    fn milestone_and_cliff_vesting_with_strike_price_exercise() {
        let s = setup_grant();
        let start = s.env.ledger().timestamp();

        // 1,000 time-based options (cliff = 25s, total = 100s) + 2 milestones (300, 700), strike = 10
        let milestones = vec![&s.env, 300i128, 700i128];
        let grant_id = s.vault.create_option_grant(
            &s.admin,
            &s.employee,
            &start,
            &25,
            &100,
            &(start + 500),
            &1_000,
            &milestones,
            &10,
        );
        assert_eq!(s.equity.balance(&s.vault_id), 2_000);

        // Before cliff, 0 vested; trigger milestone 0 (300 shares)
        set_time(&s.env, start + 10);
        assert_eq!(s.vault.vested_options(&grant_id), 0);
        let unlocked = s.vault.vest_milestone(&s.admin, &grant_id, &0);
        assert_eq!(unlocked, 300);
        assert_eq!(s.vault.exercisable_options(&grant_id), 300);

        // Reach 50% time vesting (500 shares) + 300 milestone = 800 exercisable
        set_time(&s.env, start + 50);
        assert_eq!(s.vault.exercisable_options(&grant_id), 800);
        assert_eq!(s.vault.calculate_exercise_cost(&grant_id, &500), 5_000);

        // Exercise 500 options -> employee pays 5,000 payment tokens to treasury and receives 500 equity shares
        let paid = s.vault.exercise_options(&s.employee, &grant_id, &500);
        assert_eq!(paid, 5_000);
        assert_eq!(s.equity.balance(&s.employee), 500);
        assert_eq!(s.payment.balance(&s.treasury), 5_000);
        assert_eq!(s.vault.exercisable_options(&grant_id), 300);
    }

    #[test]
    fn auto_clawback_unvested_on_termination_and_expired_options() {
        let s = setup_grant();
        let start = s.env.ledger().timestamp();
        let milestones = vec![&s.env, 400i128, 600i128];

        // 1,000 time-based + 1,000 milestone = 2,000 total
        let grant_id = s.vault.create_option_grant(
            &s.admin,
            &s.employee,
            &start,
            &0,
            &100,
            &(start + 300),
            &1_000,
            &milestones,
            &5,
        );

        // Vest milestone 0 (400) and advance to 40% of time schedule (400) => 800 vested, 1,200 unvested
        s.vault.vest_milestone(&s.admin, &grant_id, &0);
        set_time(&s.env, start + 40);

        let clawed_back = s.vault.terminate_employee(&s.admin, &grant_id);
        assert_eq!(clawed_back, 1_200);
        assert_eq!(s.equity.balance(&s.treasury), 1_200);

        // Cannot vest further milestones after termination
        assert_eq!(
            s.vault.try_vest_milestone(&s.admin, &grant_id, &1),
            Err(Ok(GrantError::EmployeeTerminated.into()))
        );

        // Employee can still exercise already-vested options (300 of the 800) before expiration
        s.vault.exercise_options(&s.employee, &grant_id, &300);
        assert_eq!(s.equity.balance(&s.employee), 300);

        // Advance past expiration_time: remaining 500 unexercised vested options are clawed back
        set_time(&s.env, start + 301);
        assert_eq!(
            s.vault.try_exercise_options(&s.employee, &grant_id, &100),
            Err(Ok(GrantError::OptionsExpired.into()))
        );

        let expired_clawback = s.vault.clawback_expired_options(&s.admin, &grant_id);
        assert_eq!(expired_clawback, 500);
        assert_eq!(s.equity.balance(&s.treasury), 1_700);
        assert_eq!(s.equity.balance(&s.vault_id), 0);
    }
}
