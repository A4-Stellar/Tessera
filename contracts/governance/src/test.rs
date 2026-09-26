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
