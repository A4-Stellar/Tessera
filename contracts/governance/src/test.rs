extern crate std;

use soroban_sdk::{
    contract, contractimpl, symbol_short,
    testutils::{Address as _, AuthorizedFunction, Ledger as _, MockAuth, MockAuthInvoke},
    vec,
    xdr::ToXdr,
    Address, BytesN, Env, IntoVal, Symbol, Vec,
};

use crate::{Error, ExecutionPayload, GovernanceContract, GovernanceContractClient};

/// A governed contract whose admin is the governance contract.
#[contract]
struct FeeContract;

#[contractimpl]
impl FeeContract {
    pub fn __constructor(env: Env, admin: Address) {
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
    }

    pub fn set_fee(env: Env, fee: u32) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap();
        admin.require_auth();
        if fee > 10_000 {
            panic!("fee above 100%");
        }
        env.storage().instance().set(&symbol_short!("fee"), &fee);
    }

    pub fn fee(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&symbol_short!("fee"))
            .unwrap_or(0)
    }
}

struct Setup {
    env: Env,
    gov: GovernanceContractClient<'static>,
    fee_contract: Address,
    members: std::vec::Vec<Address>,
}

/// A 2-of-3 board governing a `FeeContract`.
fn setup() -> Setup {
    let env = Env::default();
    let members: std::vec::Vec<Address> = (0..3).map(|_| Address::generate(&env)).collect();
    let board = Vec::from_slice(&env, &members);
    let gov_id = env.register(GovernanceContract, (board, 2u32));
    let fee_contract = env.register(FeeContract, (gov_id.clone(),));
    Setup {
        gov: GovernanceContractClient::new(&env, &gov_id),
        env,
        fee_contract,
        members,
    }
}

fn set_fee_payload(s: &Setup, fee: u32) -> ExecutionPayload {
    ExecutionPayload {
        contract: s.fee_contract.clone(),
        function: Symbol::new(&s.env, "set_fee"),
        args: vec![&s.env, fee.into_val(&s.env)],
    }
}

/// How an off-chain client computes the action hash: SHA-256 of the
/// payload's `ScVal` XDR.
fn hash(env: &Env, payload: &ExecutionPayload) -> BytesN<32> {
    env.crypto().sha256(&payload.clone().to_xdr(env)).into()
}

/// Propose `payload` as member 0, expiring 100 ledgers from now.
fn propose(s: &Setup, payload: &ExecutionPayload) -> (u64, BytesN<32>) {
    let action_hash = hash(&s.env, payload);
    let expiration = s.env.ledger().sequence() + 100;
    s.env.mock_all_auths();
    let id = s
        .gov
        .propose_resolution(&s.members[0], &action_hash, payload, &expiration);
    (id, action_hash)
}

#[test]
fn executes_at_threshold_with_member_signatures_only() {
    let s = setup();
    let payload = set_fee_payload(&s, 250);
    let (id, action_hash) = propose(&s, &payload);
    let fee = FeeContractClient::new(&s.env, &s.fee_contract);

    // Only the approving member's signature is mocked. The governance
    // contract authorizes `set_fee` as the direct invoker, not via a mock.
    for (member, expect_executed) in [(&s.members[1], false), (&s.members[2], true)] {
        let args = (member.clone(), id, action_hash.clone()).into_val(&s.env);
        s.env.mock_auths(&[MockAuth {
            address: member,
            invoke: &MockAuthInvoke {
                contract: &s.gov.address,
                fn_name: "approve_resolution",
                args,
                sub_invokes: &[],
            },
        }]);
        let executed = s.gov.approve_resolution(member, &id, &action_hash);
        assert_eq!(executed, expect_executed);

        // The member signed over the resolution id and the action hash.
        let auths = s.env.auths();
        assert_eq!(auths.len(), 1);
        assert_eq!(auths[0].0, member.clone());
        match &auths[0].1.function {
            AuthorizedFunction::Contract((contract, function, signed_args)) => {
                assert_eq!(contract, &s.gov.address);
                assert_eq!(function, &Symbol::new(&s.env, "approve_resolution"));
                assert_eq!(
                    signed_args,
                    &(member.clone(), id, action_hash.clone()).into_val(&s.env)
                );
            }
            _ => panic!("expected a contract authorization"),
        }
        if !expect_executed {
            assert_eq!(fee.fee(), 0);
        }
    }

    assert_eq!(fee.fee(), 250);
    let resolution = s.gov.get_resolution(&id);
    assert!(resolution.executed);
    assert_eq!(resolution.approvals.len(), 2);
}

#[test]
fn rejects_duplicate_and_post_execution_approvals() {
    let s = setup();
    let (id, action_hash) = propose(&s, &set_fee_payload(&s, 1));

    s.gov.approve_resolution(&s.members[0], &id, &action_hash);
    assert_eq!(
        s.gov
            .try_approve_resolution(&s.members[0], &id, &action_hash),
        Err(Ok(Error::AlreadyApproved.into()))
    );

    assert!(s.gov.approve_resolution(&s.members[1], &id, &action_hash));
    assert_eq!(
        s.gov
            .try_approve_resolution(&s.members[2], &id, &action_hash),
        Err(Ok(Error::AlreadyExecuted.into()))
    );
}

#[test]
fn rejects_non_members() {
    let s = setup();
    let payload = set_fee_payload(&s, 1);
    let (id, action_hash) = propose(&s, &payload);
    let outsider = Address::generate(&s.env);

    assert_eq!(
        s.gov.try_propose_resolution(
            &outsider,
            &action_hash,
            &payload,
            &(s.env.ledger().sequence() + 10)
        ),
        Err(Ok(Error::NotBoardMember.into()))
    );
    assert_eq!(
        s.gov.try_approve_resolution(&outsider, &id, &action_hash),
        Err(Ok(Error::NotBoardMember.into()))
    );
}

#[test]
fn binds_approvals_to_the_payload_hash() {
    let s = setup();
    let payload = set_fee_payload(&s, 1);
    let expiration = s.env.ledger().sequence() + 10;
    s.env.mock_all_auths();

    // The hash of a different payload cannot front a proposal.
    let other_hash = hash(&s.env, &set_fee_payload(&s, 9_999));
    assert_eq!(
        s.gov
            .try_propose_resolution(&s.members[0], &other_hash, &payload, &expiration),
        Err(Ok(Error::ActionHashMismatch.into()))
    );

    // An approval signed for another hash does not count.
    let (id, _) = propose(&s, &payload);
    assert_eq!(
        s.gov
            .try_approve_resolution(&s.members[1], &id, &other_hash),
        Err(Ok(Error::ActionHashMismatch.into()))
    );
}

#[test]
fn rejects_approvals_after_expiration() {
    let s = setup();
    let (id, action_hash) = propose(&s, &set_fee_payload(&s, 1));
    let expiration = s.gov.get_resolution(&id).expiration_ledger;

    s.env.ledger().set_sequence_number(expiration);
    s.gov.approve_resolution(&s.members[0], &id, &action_hash);

    s.env.ledger().set_sequence_number(expiration + 1);
    assert_eq!(
        s.gov
            .try_approve_resolution(&s.members[1], &id, &action_hash),
        Err(Ok(Error::ResolutionExpired.into()))
    );
}

#[test]
fn rejects_invalid_expirations() {
    let s = setup();
    let payload = set_fee_payload(&s, 1);
    let action_hash = hash(&s.env, &payload);
    let current = s.env.ledger().sequence();
    let beyond_max_ttl = current
        + s.env
            .as_contract(&s.gov.address, || s.env.storage().max_ttl())
        + 1;
    s.env.mock_all_auths();

    for expiration in [current, beyond_max_ttl] {
        assert_eq!(
            s.gov
                .try_propose_resolution(&s.members[0], &action_hash, &payload, &expiration),
            Err(Ok(Error::InvalidExpiration.into()))
        );
    }
}

#[test]
fn rejects_payloads_targeting_itself() {
    let s = setup();
    let payload = ExecutionPayload {
        contract: s.gov.address.clone(),
        function: Symbol::new(&s.env, "board"),
        args: Vec::new(&s.env),
    };
    let action_hash = hash(&s.env, &payload);
    s.env.mock_all_auths();
    assert_eq!(
        s.gov.try_propose_resolution(
            &s.members[0],
            &action_hash,
            &payload,
            &(s.env.ledger().sequence() + 10)
        ),
        Err(Ok(Error::SelfInvocation.into()))
    );
}

#[test]
fn failed_execution_reverts_the_final_approval() {
    let s = setup();
    // set_fee panics above 10_000, so executing this payload fails.
    let (id, action_hash) = propose(&s, &set_fee_payload(&s, 10_001));

    s.gov.approve_resolution(&s.members[0], &id, &action_hash);
    assert!(s
        .gov
        .try_approve_resolution(&s.members[1], &id, &action_hash)
        .is_err());

    let resolution = s.gov.get_resolution(&id);
    assert!(!resolution.executed);
    assert_eq!(resolution.approvals.len(), 1);
}

#[test]
fn unknown_resolution() {
    let s = setup();
    s.env.mock_all_auths();
    assert_eq!(
        s.gov
            .try_approve_resolution(&s.members[0], &7, &BytesN::from_array(&s.env, &[0; 32])),
        Err(Ok(Error::ResolutionNotFound.into()))
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn constructor_rejects_duplicate_members() {
    let env = Env::default();
    let member = Address::generate(&env);
    env.register(
        GovernanceContract,
        (vec![&env, member.clone(), member], 1u32),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn constructor_rejects_threshold_above_board_size() {
    let env = Env::default();
    let board = vec![&env, Address::generate(&env)];
    env.register(GovernanceContract, (board, 2u32));
}

// ========================================================================== //
// Vote-Escrowed Staking Vault (veTESSERA) Tests
// ========================================================================== //

mod ve_tests {
    use soroban_sdk::{
        contract, contractimpl, contracttype,
        testutils::{Address as _, Ledger as _},
        Address, Env,
    };

    use crate::ve_vault::{
        VeError, VeVaultContract, VeVaultContractClient, DEFAULT_MAX_PENALTY_BPS,
        MAX_LOCK_DURATION, SECONDS_PER_DAY,
    };

    #[derive(Clone)]
    #[contracttype]
    enum TokenKey {
        Balance(Address),
    }

    #[contract]
    pub struct MockTesseraToken;

    #[contractimpl]
    impl MockTesseraToken {
        pub fn mint(env: Env, to: Address, amount: i128) {
            let bal = Self::balance(env.clone(), to.clone());
            env.storage()
                .persistent()
                .set(&TokenKey::Balance(to), &(bal + amount));
        }

        pub fn balance(env: Env, id: Address) -> i128 {
            env.storage()
                .persistent()
                .get(&TokenKey::Balance(id))
                .unwrap_or(0)
        }

        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            from.require_auth();
            let from_bal = Self::balance(env.clone(), from.clone());
            assert!(from_bal >= amount, "insufficient token balance");
            let to_bal = Self::balance(env.clone(), to.clone());
            env.storage()
                .persistent()
                .set(&TokenKey::Balance(from), &(from_bal - amount));
            env.storage()
                .persistent()
                .set(&TokenKey::Balance(to), &(to_bal + amount));
        }
    }

    struct VeSetup {
        env: Env,
        admin: Address,
        staker: Address,
        treasury: Address,
        vault_id: Address,
        vault: VeVaultContractClient<'static>,
        token: MockTesseraTokenClient<'static>,
    }

    fn setup_ve() -> VeSetup {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);

        let admin = Address::generate(&env);
        let staker = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token_id = env.register(MockTesseraToken, ());
        let vault_id = env.register(VeVaultContract, ());

        let token = MockTesseraTokenClient::new(&env, &token_id);
        let vault = VeVaultContractClient::new(&env, &vault_id);

        token.mint(&staker, &1_000_000);
        vault.initialize_vault(&admin, &token_id, &treasury);

        VeSetup {
            env,
            admin,
            staker,
            treasury,
            vault_id,
            vault,
            token,
        }
    }

    #[test]
    fn voting_weight_and_non_linear_dividend_weight_computation() {
        let s = setup_ve();
        let amount = 100_000i128;
        let one_year = 365 * SECONDS_PER_DAY;
        let two_years = 2 * one_year;
        let four_years = MAX_LOCK_DURATION;

        // 1 year (25% of T_max): W = 100,000 * 0.25 = 25,000; D = 100,000 * (0.25)^2 = 6,250
        assert_eq!(s.vault.compute_voting_weight(&amount, &one_year), 25_000);
        assert_eq!(s.vault.compute_dividend_weight(&amount, &one_year), 6_250);

        // 2 years (50% of T_max): W = 50,000; D = 100,000 * (0.50)^2 = 25,000
        assert_eq!(s.vault.compute_voting_weight(&amount, &two_years), 50_000);
        assert_eq!(s.vault.compute_dividend_weight(&amount, &two_years), 25_000);

        // 4 years (100% of T_max): W = 100,000; D = 100,000
        assert_eq!(s.vault.compute_voting_weight(&amount, &four_years), 100_000);
        assert_eq!(
            s.vault.compute_dividend_weight(&amount, &four_years),
            100_000
        );
    }

    #[test]
    fn linear_voting_power_decay_preserves_locked_balance() {
        let s = setup_ve();
        let amount = 400_000i128;
        let start = s.env.ledger().timestamp();

        let lock = s
            .vault
            .create_lock(&s.staker, &amount, &MAX_LOCK_DURATION);
        assert_eq!(lock.initial_voting_power, 400_000);
        assert_eq!(lock.initial_dividend_weight, 400_000);
        assert_eq!(s.vault.voting_power(&s.staker), 400_000);
        assert_eq!(s.token.balance(&s.vault_id), 400_000);

        // 1 year elapsed (75% time remaining): W = 300,000, D = 400,000 * (0.75)^2 = 225,000
        s.env.ledger().set_timestamp(start + MAX_LOCK_DURATION / 4);
        assert_eq!(s.vault.voting_power(&s.staker), 300_000);
        assert_eq!(s.vault.dividend_weight(&s.staker), 225_000);
        assert_eq!(s.vault.get_lock(&s.staker).amount, 400_000);
        assert_eq!(s.token.balance(&s.vault_id), 400_000);

        // 2 years elapsed (50% time remaining): W = 200,000, D = 100,000
        s.env.ledger().set_timestamp(start + MAX_LOCK_DURATION / 2);
        assert_eq!(s.vault.voting_power(&s.staker), 200_000);
        assert_eq!(s.vault.dividend_weight(&s.staker), 100_000);

        // 3 years elapsed (25% time remaining): W = 100,000, D = 25,000
        s.env
            .ledger()
            .set_timestamp(start + (MAX_LOCK_DURATION * 3) / 4);
        assert_eq!(s.vault.voting_power(&s.staker), 100_000);
        assert_eq!(s.vault.dividend_weight(&s.staker), 25_000);

        // At expiration: voting power is 0, but principal remains safely escrowed until withdrawal
        s.env.ledger().set_timestamp(start + MAX_LOCK_DURATION);
        assert_eq!(s.vault.voting_power(&s.staker), 0);
        assert_eq!(s.vault.dividend_weight(&s.staker), 0);
        assert_eq!(s.token.balance(&s.vault_id), 400_000);

        // Full withdrawal at maturity returns 100% of principal with zero penalty
        let withdrawn = s.vault.withdraw(&s.staker);
        assert_eq!(withdrawn, 400_000);
        assert_eq!(s.token.balance(&s.staker), 1_000_000);
        assert_eq!(s.token.balance(&s.treasury), 0);
        assert_eq!(s.vault.total_locked(), 0);
    }

    #[test]
    fn early_withdrawal_applies_progressive_penalty_to_treasury() {
        let s = setup_ve();
        let amount = 200_000i128;
        let start = s.env.ledger().timestamp();
        let duration = 2 * 365 * SECONDS_PER_DAY; // 2 years

        s.vault.create_lock(&s.staker, &amount, &duration);
        assert_eq!(s.vault.max_penalty_bps(), DEFAULT_MAX_PENALTY_BPS); // 50% max penalty

        // Immediately at t = start (100% of lock remaining): penalty is 50% = 100,000
        assert_eq!(s.vault.calculate_early_exit_penalty(&s.staker), 100_000);

        // At 25% of lock elapsed (75% remaining): penalty is 50% * 0.75 = 37.5% = 75,000
        s.env.ledger().set_timestamp(start + duration / 4);
        assert_eq!(s.vault.calculate_early_exit_penalty(&s.staker), 75_000);

        // At 50% of lock elapsed (50% remaining): penalty is 50% * 0.50 = 25% = 50,000
        s.env.ledger().set_timestamp(start + duration / 2);
        assert_eq!(s.vault.calculate_early_exit_penalty(&s.staker), 50_000);

        let (payout, penalty) = s.vault.early_withdraw(&s.staker);
        assert_eq!(payout, 150_000);
        assert_eq!(penalty, 50_000);
        assert_eq!(s.token.balance(&s.staker), 800_000 + 150_000);
        assert_eq!(s.token.balance(&s.treasury), 50_000);
        assert_eq!(s.vault.total_penalties_collected(), 50_000);
        assert_eq!(s.vault.total_locked(), 0);
        assert_eq!(s.vault.voting_power(&s.staker), 0);
    }

    #[test]
    fn increase_amount_and_extend_duration_and_ttl_bump() {
        let s = setup_ve();
        let one_year = 365 * SECONDS_PER_DAY;
        let two_years = 2 * one_year;

        s.vault.create_lock(&s.staker, &100_000, &one_year);
        assert_eq!(s.vault.voting_power(&s.staker), 25_000);

        // Increase amount to 200,000 without changing unlock_time
        let updated = s.vault.increase_amount(&s.staker, &100_000);
        assert_eq!(updated.amount, 200_000);
        assert_eq!(s.vault.voting_power(&s.staker), 50_000);

        // Extend duration to 2 years
        let extended = s.vault.extend_lock_duration(&s.staker, &two_years);
        assert_eq!(extended.lock_duration, two_years);
        assert_eq!(s.vault.voting_power(&s.staker), 100_000);

        // Bump persistent storage TTL for long-lived 4-year state expiry protection
        let ttl = s.vault.extend_lock_ttl(&s.staker, &s.staker);
        assert!(ttl > 0);

        s.vault.set_max_penalty_bps(&s.admin, &4_000);
        assert_eq!(s.vault.max_penalty_bps(), 4_000);
    }

    #[test]
    fn enforces_duration_bounds_and_withdrawal_guards() {
        let s = setup_ve();
        let one_year = 365 * SECONDS_PER_DAY;

        assert_eq!(
            s.vault.try_create_lock(&s.staker, &100_000, &0),
            Err(Ok(VeError::InvalidDuration.into()))
        );
        assert_eq!(
            s.vault
                .try_create_lock(&s.staker, &100_000, &(MAX_LOCK_DURATION + 1)),
            Err(Ok(VeError::InvalidDuration.into()))
        );
        assert_eq!(
            s.vault.try_create_lock(&s.staker, &0, &one_year),
            Err(Ok(VeError::InvalidAmount.into()))
        );

        s.vault.create_lock(&s.staker, &100_000, &one_year);

        // Cannot create duplicate active lock
        assert_eq!(
            s.vault.try_create_lock(&s.staker, &50_000, &one_year),
            Err(Ok(VeError::LockAlreadyExists.into()))
        );

        // Cannot standard-withdraw before maturity
        assert_eq!(
            s.vault.try_withdraw(&s.staker),
            Err(Ok(VeError::LockNotExpired.into()))
        );
    }
}
