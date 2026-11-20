//! Timelocked governance execution (issue #363).
//!
//! The resolution engine next door executes the moment an m-of-n approval
//! threshold is met. This contract adds the missing safety rails:
//!
//! 1. **Voting.** Token holders / delegates vote on a proposal during a fixed
//!    voting period.
//! 2. **Mandatory timelock.** A proposal that passes cannot execute until
//!    `timelock_delay_ledgers` have elapsed, giving users time to exit.
//! 3. **Guardian veto.** During the veto window the guardian multisig can
//!    cancel a malicious proposal with Ed25519 signatures checked on-chain.
//! 4. **Emergency fast-track.** A proposal with strictly more than 90 % yes
//!    votes can skip the timelock, but only with the guardian multisig's
//!    co-signatures.
//! 5. **Versioned envelopes.** New proposals are stored as [`ProposalV1`];
//!    `migrate_proposal` upgrades them in place to [`ProposalV2`], which adds
//!    the reviewed action hash and an explicit schema version, without
//!    invalidating pending proposals.
//!
//! Every state transition emits a structured event.

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error,
    xdr::ToXdr, Address, BytesN, Env, IntoVal, Symbol, Val, Vec,
};

use crate::resolution::ExecutionPayload;

/// 100 % in basis points.
const BPS_SCALE: u32 = 10_000;
/// Emergency proposals require strictly more than 90 % yes votes.
const EMERGENCY_YES_BPS: u32 = 9_000;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum TimelockError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidConfig = 4,
    EmptyPayload = 5,
    ProposalNotFound = 6,
    VotingClosed = 7,
    VotingStillOpen = 8,
    AlreadyVoted = 9,
    NotPassed = 10,
    TimelockActive = 11,
    VetoWindowActive = 12,
    VetoWindowClosed = 13,
    AlreadyFinalized = 14,
    AlreadyMigrated = 15,
    InvalidSignature = 16,
    InsufficientGuardianSignatures = 17,
    NotEmergencyEligible = 18,
    AlreadyExecuted = 19,
    Vetoed = 20,
    SelfInvocation = 21,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub enum ProposalStatus {
    Pending,
    Passed,
    Defeated,
    Vetoed,
    Executed,
}

/// First schema: the proposal without the reviewed action hash. Existing
/// deployments and in-flight proposals keep this layout.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ProposalV1 {
    pub id: u64,
    pub proposer: Address,
    pub payload: ExecutionPayload,
    pub created_ledger: u32,
    pub voting_ends: u32,
    pub yes_votes: u32,
    pub no_votes: u32,
    pub status: ProposalStatus,
    pub passed_ledger: u32,
    pub execute_after: u32,
    pub veto_deadline: u32,
}

/// Second schema: adds the explicit version and the action hash that the
/// guardian signatures commit to.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ProposalV2 {
    pub version: u32,
    pub id: u64,
    pub proposer: Address,
    pub payload: ExecutionPayload,
    pub action_hash: BytesN<32>,
    pub created_ledger: u32,
    pub voting_ends: u32,
    pub yes_votes: u32,
    pub no_votes: u32,
    pub status: ProposalStatus,
    pub passed_ledger: u32,
    pub execute_after: u32,
    pub veto_deadline: u32,
}

/// Storage envelope that lets V1 and V2 proposals coexist.
#[contracttype]
#[derive(Clone, Debug)]
pub enum ProposalEnvelope {
    V1(ProposalV1),
    V2(ProposalV2),
}

impl ProposalEnvelope {
    fn into_v2(&self, env: &Env) -> ProposalV2 {
        match self {
            ProposalEnvelope::V1(p) => ProposalV2 {
                version: 2,
                id: p.id,
                proposer: p.proposer.clone(),
                payload: p.payload.clone(),
                action_hash: Self::hash_payload(env, &p.payload),
                created_ledger: p.created_ledger,
                voting_ends: p.voting_ends,
                yes_votes: p.yes_votes,
                no_votes: p.no_votes,
                status: p.status.clone(),
                passed_ledger: p.passed_ledger,
                execute_after: p.execute_after,
                veto_deadline: p.veto_deadline,
            },
            ProposalEnvelope::V2(p) => p.clone(),
        }
    }

    fn hash_payload(env: &Env, payload: &ExecutionPayload) -> BytesN<32> {
        env.crypto()
            .sha256(&payload.clone().to_xdr(env))
            .into()
    }
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelockConfig {
    pub voters: Vec<Address>,
    /// Ed25519 public keys of the guardians, in signature order.
    pub guardian_keys: Vec<BytesN<32>>,
    /// Number of guardian signatures required to veto or fast-track.
    pub guardian_threshold: u32,
    /// Ledgers a passed proposal must wait before execution.
    pub timelock_delay_ledgers: u32,
    /// Ledgers after passage during which the guardians may veto.
    pub veto_window_ledgers: u32,
    /// Voting period applied by `create_proposal`.
    pub voting_period_ledgers: u32,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    Config,
    NextId,
    Proposal(u64),
    Voted(u64, Address),
}

#[contractevent]
pub struct ProposalCreated {
    #[topic]
    pub id: u64,
    pub proposer: Address,
    pub voting_ends: u32,
    pub action_hash: BytesN<32>,
}

#[contractevent]
pub struct ProposalPassed {
    #[topic]
    pub id: u64,
    pub yes_votes: u32,
    pub no_votes: u32,
    pub execute_after: u32,
    pub veto_deadline: u32,
}

#[contractevent]
pub struct ProposalVetoed {
    #[topic]
    pub id: u64,
    pub signatures: u32,
}

#[contractevent]
pub struct ProposalExecuted {
    #[topic]
    pub id: u64,
    pub emergency: bool,
    pub ledger: u32,
}

#[contractevent]
pub struct ProposalMigrated {
    #[topic]
    pub id: u64,
    pub from_version: u32,
    pub to_version: u32,
}

#[contractevent]
pub struct ProposalDefeated {
    #[topic]
    pub id: u64,
    pub yes_votes: u32,
    pub no_votes: u32,
}

#[contract]
pub struct GovernanceTimelockContract;

#[contractimpl]
impl GovernanceTimelockContract {
    /// Configure voters, guardians and the timelock windows once.
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        env: Env,
        admin: Address,
        voters: Vec<Address>,
        guardian_keys: Vec<BytesN<32>>,
        guardian_threshold: u32,
        timelock_delay_ledgers: u32,
        veto_window_ledgers: u32,
        voting_period_ledgers: u32,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, TimelockError::AlreadyInitialized);
        }
        if voters.is_empty()
            || guardian_keys.is_empty()
            || guardian_threshold == 0
            || guardian_threshold > guardian_keys.len()
            || voting_period_ledgers == 0
        {
            panic_with_error!(env, TimelockError::InvalidConfig);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(
            &DataKey::Config,
            &TimelockConfig {
                voters,
                guardian_keys,
                guardian_threshold,
                timelock_delay_ledgers,
                veto_window_ledgers,
                voting_period_ledgers,
            },
        );
        env.storage().instance().set(&DataKey::NextId, &0u64);
    }

    /// Create a V1 proposal. Only configured voters may propose.
    pub fn create_proposal(
        env: Env,
        proposer: Address,
        payload: ExecutionPayload,
    ) -> u64 {
        proposer.require_auth();
        let config = Self::load_config(&env);
        Self::require_voter(&env, &config, &proposer);
        if payload.contract == env.current_contract_address() {
            panic_with_error!(env, TimelockError::SelfInvocation);
        }
        let now = env.ledger().sequence();
        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextId)
            .unwrap_or(0);
        env.storage().instance().set(&DataKey::NextId, &(id + 1));
        let proposal = ProposalV1 {
            id,
            proposer: proposer.clone(),
            payload: payload.clone(),
            created_ledger: now,
            voting_ends: now + config.voting_period_ledgers,
            yes_votes: 0,
            no_votes: 0,
            status: ProposalStatus::Pending,
            passed_ledger: 0,
            execute_after: 0,
            veto_deadline: 0,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(id), &ProposalEnvelope::V1(proposal));
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Proposal(id), 17_280, 535_680);
        ProposalCreated {
            id,
            proposer,
            voting_ends: now + config.voting_period_ledgers,
            action_hash: ProposalEnvelope::hash_payload(&env, &payload),
        }
        .publish(&env);
        id
    }

    /// Cast one vote per voter while the voting period is open.
    pub fn cast_vote(env: Env, voter: Address, proposal_id: u64, support: bool) {
        voter.require_auth();
        let config = Self::load_config(&env);
        Self::require_voter(&env, &config, &voter);
        let key = DataKey::Proposal(proposal_id);
        let mut proposal = Self::load(&env, proposal_id).into_v2(&env);
        if proposal.status != ProposalStatus::Pending {
            panic_with_error!(env, TimelockError::VotingClosed);
        }
        if env.ledger().sequence() >= proposal.voting_ends {
            panic_with_error!(env, TimelockError::VotingClosed);
        }
        let voted_key = DataKey::Voted(proposal_id, voter.clone());
        if env.storage().persistent().has(&voted_key) {
            panic_with_error!(env, TimelockError::AlreadyVoted);
        }
        env.storage().persistent().set(&voted_key, &true);
        if support {
            proposal.yes_votes += 1;
        } else {
            proposal.no_votes += 1;
        }
        env.storage()
            .persistent()
            .set(&key, &ProposalEnvelope::V2(proposal));
        env.storage().persistent().extend_ttl(&key, 17_280, 535_680);
    }

    /// Close voting. A proposal with more yes than no votes (and at least one
    /// vote) becomes `Passed`, with its timelock and veto deadlines set.
    pub fn finalize(env: Env, proposal_id: u64) -> ProposalStatus {
        let key = DataKey::Proposal(proposal_id);
        let mut proposal = Self::load(&env, proposal_id).into_v2(&env);
        if proposal.status != ProposalStatus::Pending {
            panic_with_error!(env, TimelockError::AlreadyFinalized);
        }
        let now = env.ledger().sequence();
        if now < proposal.voting_ends {
            panic_with_error!(env, TimelockError::VotingStillOpen);
        }
        let config = Self::load_config(&env);
        if proposal.yes_votes > proposal.no_votes {
            proposal.status = ProposalStatus::Passed;
            proposal.passed_ledger = now;
            proposal.execute_after = now + config.timelock_delay_ledgers;
            proposal.veto_deadline = now + config.veto_window_ledgers;
            env.storage()
                .persistent()
                .set(&key, &ProposalEnvelope::V2(proposal.clone()));
            ProposalPassed {
                id: proposal_id,
                yes_votes: proposal.yes_votes,
                no_votes: proposal.no_votes,
                execute_after: proposal.execute_after,
                veto_deadline: proposal.veto_deadline,
            }
            .publish(&env);
        } else {
            proposal.status = ProposalStatus::Defeated;
            env.storage()
                .persistent()
                .set(&key, &ProposalEnvelope::V2(proposal.clone()));
            ProposalDefeated {
                id: proposal_id,
                yes_votes: proposal.yes_votes,
                no_votes: proposal.no_votes,
            }
            .publish(&env);
        }
        proposal.status
    }

    /// Guardian multisig veto. Signatures must be ordered like
    /// `guardian_keys`; the first `guardian_threshold` signatures are checked
    /// against the corresponding keys over the veto domain message.
    pub fn veto(env: Env, proposal_id: u64, signatures: Vec<BytesN<64>>) {
        let key = DataKey::Proposal(proposal_id);
        let mut proposal = Self::load(&env, proposal_id).into_v2(&env);
        if proposal.status == ProposalStatus::Executed {
            panic_with_error!(env, TimelockError::AlreadyExecuted);
        }
        if proposal.status != ProposalStatus::Passed {
            panic_with_error!(env, TimelockError::NotPassed);
        }
        let now = env.ledger().sequence();
        if now > proposal.veto_deadline {
            panic_with_error!(env, TimelockError::VetoWindowClosed);
        }
        let config = Self::load_config(&env);
        let message = Self::domain_hash(&env, "veto", proposal_id);
        Self::verify_guardians(&env, &config, &message, &signatures);
        proposal.status = ProposalStatus::Vetoed;
        env.storage()
            .persistent()
            .set(&key, &ProposalEnvelope::V2(proposal));
        ProposalVetoed {
            id: proposal_id,
            signatures: config.guardian_threshold,
        }
        .publish(&env);
    }

    /// Execute a passed proposal once both the timelock and the veto window
    /// have elapsed.
    pub fn execute(env: Env, proposal_id: u64) -> Val {
        let key = DataKey::Proposal(proposal_id);
        let mut proposal = Self::load(&env, proposal_id).into_v2(&env);
        if proposal.status == ProposalStatus::Executed {
            panic_with_error!(env, TimelockError::AlreadyExecuted);
        }
        if proposal.status == ProposalStatus::Vetoed {
            panic_with_error!(env, TimelockError::Vetoed);
        }
        if proposal.status != ProposalStatus::Passed {
            panic_with_error!(env, TimelockError::NotPassed);
        }
        let now = env.ledger().sequence();
        if now < proposal.execute_after {
            panic_with_error!(env, TimelockError::TimelockActive);
        }
        if now <= proposal.veto_deadline {
            panic_with_error!(env, TimelockError::VetoWindowActive);
        }
        proposal.status = ProposalStatus::Executed;
        env.storage()
            .persistent()
            .set(&key, &ProposalEnvelope::V2(proposal.clone()));
        let result = Self::invoke(&env, &proposal.payload);
        ProposalExecuted {
            id: proposal_id,
            emergency: false,
            ledger: now,
        }
        .publish(&env);
        result
    }

    /// Skip the timelock when a proposal has strictly more than 90 % yes
    /// votes. Guardian co-signatures are mandatory even for emergencies.
    pub fn execute_emergency(
        env: Env,
        proposal_id: u64,
        signatures: Vec<BytesN<64>>,
    ) -> Val {
        let key = DataKey::Proposal(proposal_id);
        let mut proposal = Self::load(&env, proposal_id).into_v2(&env);
        if proposal.status == ProposalStatus::Executed {
            panic_with_error!(env, TimelockError::AlreadyExecuted);
        }
        if proposal.status == ProposalStatus::Vetoed {
            panic_with_error!(env, TimelockError::Vetoed);
        }
        if proposal.status != ProposalStatus::Passed {
            panic_with_error!(env, TimelockError::NotPassed);
        }
        let total = proposal.yes_votes + proposal.no_votes;
        // Strictly greater than 90 %: yes * 10000 > total * 9000.
        if total == 0
            || (proposal.yes_votes as u64) * (BPS_SCALE as u64)
                <= (total as u64) * (EMERGENCY_YES_BPS as u64)
        {
            panic_with_error!(env, TimelockError::NotEmergencyEligible);
        }
        let config = Self::load_config(&env);
        let message = Self::domain_hash(&env, "emergency", proposal_id);
        Self::verify_guardians(&env, &config, &message, &signatures);
        proposal.status = ProposalStatus::Executed;
        env.storage()
            .persistent()
            .set(&key, &ProposalEnvelope::V2(proposal.clone()));
        let result = Self::invoke(&env, &proposal.payload);
        ProposalExecuted {
            id: proposal_id,
            emergency: true,
            ledger: env.ledger().sequence(),
        }
        .publish(&env);
        result
    }

    /// Upgrade a stored V1 proposal to V2 in place. The payload, votes and
    /// status are preserved and the reviewed action hash is added.
    pub fn migrate_proposal(env: Env, admin: Address, proposal_id: u64) -> ProposalEnvelope {
        Self::require_admin(&env, &admin);
        let key = DataKey::Proposal(proposal_id);
        let stored = Self::load(&env, proposal_id);
        let upgraded = match &stored {
            ProposalEnvelope::V2(_) => panic_with_error!(env, TimelockError::AlreadyMigrated),
            ProposalEnvelope::V1(v1) => {
                let v2 = stored.into_v2(&env);
                ProposalMigrated {
                    id: proposal_id,
                    from_version: 1,
                    to_version: 2,
                }
                .publish(&env);
                ProposalEnvelope::V2(v2)
            }
        };
        env.storage().persistent().set(&key, &upgraded);
        env.storage()
            .persistent()
            .extend_ttl(&key, 17_280, 535_680);
        upgraded
    }

    /// Read a proposal in whichever envelope version is stored.
    pub fn get_proposal(env: Env, proposal_id: u64) -> ProposalEnvelope {
        Self::load(&env, proposal_id)
    }

    pub fn config(env: Env) -> TimelockConfig {
        Self::load_config(&env)
    }

    fn load_config(env: &Env) -> TimelockConfig {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .unwrap_or_else(|| panic_with_error!(env, TimelockError::NotInitialized))
    }

    // ------------------------------------------------------------------
    // Internal
    // ------------------------------------------------------------------

    fn load(env: &Env, proposal_id: u64) -> ProposalEnvelope {
        env.storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(env, TimelockError::ProposalNotFound))
    }

    fn require_admin(env: &Env, admin: &Address) {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, TimelockError::NotInitialized));
        admin.require_auth();
        if stored != *admin {
            panic_with_error!(env, TimelockError::Unauthorized);
        }
    }

    fn require_voter(env: &Env, config: &TimelockConfig, who: &Address) {
        if !config.voters.contains(who) {
            panic_with_error!(env, TimelockError::Unauthorized);
        }
    }

    /// Hash a domain-separated action so guardian signatures cannot be
    /// replayed from one proposal or action type to another.
    fn domain_hash(env: &Env, action: &str, proposal_id: u64) -> BytesN<32> {
        let domain: (Symbol, Address, u64) = (
            Symbol::new(env, action),
            env.current_contract_address(),
            proposal_id,
        );
        env.crypto().sha256(&domain.to_xdr(env)).into()
    }

    fn verify_guardians(
        env: &Env,
        config: &TimelockConfig,
        message: &BytesN<32>,
        signatures: &Vec<BytesN<64>>,
    ) {
        let threshold = config.guardian_threshold;
        if signatures.len() < threshold {
            panic_with_error!(env, TimelockError::InsufficientGuardianSignatures);
        }
        let message: soroban_sdk::Bytes = message.clone().into();
        for i in 0..threshold {
            let key = config.guardian_keys.get(i).unwrap();
            let signature = signatures.get(i).unwrap();
            env.crypto().ed25519_verify(&key, &message, &signature);
        }
    }

    fn invoke(env: &Env, payload: &ExecutionPayload) -> Val {
        let args: Vec<Val> = payload.args.clone();
        env.invoke_contract::<Val>(&payload.contract, &payload.function, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        contract as test_contract, contractimpl, testutils::Address as _, testutils::Ledger, BytesN,
        Env,
    };

    #[test_contract]
    struct MockTarget;

    #[contractimpl]
    impl MockTarget {
        pub fn set_value(env: Env, value: u32) {
            env.storage().instance().set(&0u32, &value);
        }

        pub fn value(env: Env) -> u32 {
            env.storage().instance().get(&0u32).unwrap_or(0)
        }
    }

    struct Setup<'a> {
        env: Env,
        client: GovernanceTimelockContractClient<'a>,
        admin: Address,
        voters: Vec<Address>,
        secrets: Vec<BytesN<32>>,
        keys: Vec<BytesN<32>>,
        target: MockTargetClient<'a>,
        target_id: Address,
    }

    fn setup<'a>() -> Setup<'a> {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_sequence_number(100);
        let admin = Address::generate(&env);
        let voters = {
            let mut v = Vec::new(&env);
            for _ in 0..3 {
                v.push_back(Address::generate(&env));
            }
            v
        };
        let mut secrets = Vec::new(&env);
        let mut keys = Vec::new(&env);
        for seed in 1u8..=2 {
            let secret = BytesN::from_array(&env, &[seed; 32]);
            keys.push_back(env.crypto().ed25519_public_key(&secret));
            secrets.push_back(secret);
        }

        let target_id = env.register(MockTarget, ());
        let target = MockTargetClient::new(&env, &target_id);
        let id = env.register(GovernanceTimelockContract, ());
        let client = GovernanceTimelockContractClient::new(&env, &id);
        client.initialize(
            &admin,
            &voters,
            &keys,
            &2,
            &10,
            &5,
            &5,
        );
        Setup {
            env,
            client,
            admin,
            voters,
            secrets,
            keys,
            target,
            target_id,
        }
    }

    fn payload(s: &Setup) -> ExecutionPayload {
        let args: Vec<Val> = (42u32,).into_val(&s.env);
        ExecutionPayload {
            contract: s.target_id.clone(),
            function: Symbol::new(&s.env, "set_value"),
            args,
        }
    }

    fn vote(s: &Setup, id: u64, support: [bool; 3]) {
        for (i, yes) in support.iter().enumerate() {
            s.client
                .cast_vote(&s.voters.get(i as u32).unwrap(), &id, yes);
        }
    }

    fn signatures(s: &Setup, proposal_id: u64, action: &str) -> Vec<BytesN<64>> {
        let hash = GovernanceTimelockContract::domain_hash(&s.env, action, proposal_id);
        let mut sigs = Vec::new(&s.env);
        for i in 0..s.secrets.len() {
            let secret = s.secrets.get(i).unwrap();
            sigs.push_back(s.env.crypto().ed25519_sign(&secret, &hash));
        }
        sigs
    }

    #[test]
    fn guardian_veto_blocks_execution() {
        let s = setup();
        let id = s.client.create_proposal(&s.voters.get(0).unwrap(), &payload(&s));
        vote(&s, id, [true, true, false]);
        s.env.ledger().set_sequence_number(106);
        assert_eq!(s.client.finalize(&id), ProposalStatus::Passed);

        s.client.veto(&id, &signatures(&s, id, "veto"));

        s.env.ledger().set_sequence_number(200);
        assert_eq!(
            s.client.try_execute(&id),
            Err(Ok(err(TimelockError::Vetoed)))
        );
        assert_eq!(s.target.value(), 0);
    }

    #[test]
    fn timelock_delays_execution() {
        let s = setup();
        let id = s.client.create_proposal(&s.voters.get(0).unwrap(), &payload(&s));
        vote(&s, id, [true, true, false]);
        s.env.ledger().set_sequence_number(106);
        s.client.finalize(&id);

        // Delay has not elapsed.
        assert_eq!(
            s.client.try_execute(&id),
            Err(Ok(err(TimelockError::TimelockActive)))
        );
        s.env.ledger().set_sequence_number(117);
        s.client.execute(&id);
        assert_eq!(s.target.value(), 42);
        assert_eq!(
            s.client.try_execute(&id),
            Err(Ok(err(TimelockError::AlreadyExecuted)))
        );
    }

    #[test]
    fn over_90_percent_fast_tracks_with_guardians() {
        let s = setup();
        let id = s.client.create_proposal(&s.voters.get(0).unwrap(), &payload(&s));
        vote(&s, id, [true, true, true]);
        s.env.ledger().set_sequence_number(106);
        s.client.finalize(&id);

        // No delay, but guardian co-signatures are mandatory.
        assert_eq!(
            s.client.try_execute_emergency(&id, &Vec::new(&s.env)),
            Err(Ok(err(TimelockError::InsufficientGuardianSignatures)))
        );
        s.client
            .execute_emergency(&id, &signatures(&s, id, "emergency"));
        assert_eq!(s.target.value(), 42);
    }

    #[test]
    fn simple_majority_cannot_fast_track() {
        let s = setup();
        let id = s.client.create_proposal(&s.voters.get(0).unwrap(), &payload(&s));
        // 2 yes, 1 no = 66 %, below the 90 % emergency bar.
        vote(&s, id, [true, true, false]);
        s.env.ledger().set_sequence_number(106);
        s.client.finalize(&id);
        assert_eq!(
            s.client
                .try_execute_emergency(&id, &signatures(&s, id, "emergency")),
            Err(Ok(err(TimelockError::NotEmergencyEligible)))
        );
    }

    #[test]
    fn migrate_upgrades_v1_to_v2() {
        let s = setup();
        let id = s.client.create_proposal(&s.voters.get(0).unwrap(), &payload(&s));
        assert!(matches!(
            s.client.get_proposal(&id),
            ProposalEnvelope::V1(_)
        ));
        let upgraded = s.client.migrate_proposal(&s.admin, &id);
        match upgraded {
            ProposalEnvelope::V2(v2) => assert_eq!(v2.version, 2),
            ProposalEnvelope::V1(_) => panic!("migration did not upgrade"),
        }
        assert_eq!(
            s.client.try_migrate_proposal(&s.admin, &id),
            Err(Ok(err(TimelockError::AlreadyMigrated)))
        );
    }

    #[test]
    fn one_guardian_signature_is_not_enough() {
        let s = setup();
        let id = s.client.create_proposal(&s.voters.get(0).unwrap(), &payload(&s));
        vote(&s, id, [true, true, false]);
        s.env.ledger().set_sequence_number(106);
        s.client.finalize(&id);
        let hash = GovernanceTimelockContract::domain_hash(&s.env, "veto", id);
        let mut one = Vec::new(&s.env);
        one.push_back(s.env.crypto().ed25519_sign(&s.secrets.get(0).unwrap(), &hash));
        assert_eq!(
            s.client.try_veto(&id, &one),
            Err(Ok(err(
                TimelockError::InsufficientGuardianSignatures
            )))
        );
    }

    fn err(e: TimelockError) -> soroban_sdk::Error {
        e.into()
    }
}
