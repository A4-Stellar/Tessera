//! Multi-signature resolution engine.
//!
//! Lifecycle: a board member proposes a resolution that commits to an
//! [`ExecutionPayload`] through `action_hash = sha256(payload XDR)`. Board
//! members approve it with their own wallet authorization, which signs the
//! resolution id *and* the action hash they reviewed. The approval that
//! reaches the m-of-n threshold invokes the payload in the same transaction.
//!
//! Signatures are verified by the Soroban host through `require_auth`, so any
//! account type works (hardware-wallet G-accounts, Stellar multisig accounts,
//! passkey contract accounts) and the host provides replay protection.
//!
//! This contract is meant to be the admin of the contracts it governs: a
//! target's `admin.require_auth()` succeeds because the host treats the
//! direct invoker contract as authorized.
//!
//! Costs, with n board members and k approvals so far (k < m <= n):
//! `propose_resolution` is O(n + |payload|) for the membership check and the
//! hash; `approve_resolution` is O(n + k) for the membership and duplicate
//! checks. Storage is O(n) for the board and O(|payload| + m) per resolution.

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error,
    xdr::ToXdr, Address, BytesN, Env, Symbol, Val, Vec,
};

/// The contract call a resolution executes once approved.
#[contracttype]
#[derive(Clone)]
pub struct ExecutionPayload {
    pub contract: Address,
    pub function: Symbol,
    pub args: Vec<Val>,
}

/// The m-of-n board: `threshold` approvals out of `members` execute a resolution.
#[contracttype]
#[derive(Clone)]
pub struct BoardConfig {
    pub members: Vec<Address>,
    pub threshold: u32,
}

#[contracttype]
#[derive(Clone)]
pub struct Resolution {
    pub proposer: Address,
    pub action_hash: BytesN<32>,
    pub payload: ExecutionPayload,
    /// Last ledger sequence at which the resolution can be approved.
    pub expiration_ledger: u32,
    pub approvals: Vec<Address>,
    pub executed: bool,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// The board is empty or lists a member twice.
    InvalidBoard = 1,
    /// The threshold is 0 or larger than the board.
    InvalidThreshold = 2,
    NotBoardMember = 3,
    /// `action_hash` is not the SHA-256 of the payload's XDR, or does not
    /// match the hash of the resolution being approved.
    ActionHashMismatch = 4,
    /// The expiration is not in the future, or is beyond the network's max TTL.
    InvalidExpiration = 5,
    /// The payload targets this contract, which the host's re-entry
    /// protection would make impossible to execute.
    SelfInvocation = 6,
    ResolutionNotFound = 7,
    ResolutionExpired = 8,
    AlreadyExecuted = 9,
    AlreadyApproved = 10,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Board,
    NextId,
    Resolution(u64),
}

#[contractevent]
pub struct ResolutionProposed {
    #[topic]
    pub id: u64,
    pub proposer: Address,
    pub action_hash: BytesN<32>,
    pub expiration_ledger: u32,
}

#[contractevent]
pub struct ResolutionApproved {
    #[topic]
    pub id: u64,
    pub member: Address,
    pub approvals: u32,
}

#[contractevent]
pub struct ResolutionExecuted {
    #[topic]
    pub id: u64,
    pub action_hash: BytesN<32>,
}

#[contract]
pub struct GovernanceContract;

#[contractimpl]
impl GovernanceContract {
    /// Set the board at deployment. A constructor runs atomically with the
    /// deploy (protocol 22+), so the board cannot be front-run.
    pub fn __constructor(env: Env, members: Vec<Address>, threshold: u32) {
        if members.is_empty() {
            panic_with_error!(env, Error::InvalidBoard);
        }
        for (i, member) in members.iter().enumerate() {
            if members.iter().skip(i + 1).any(|other| other == member) {
                panic_with_error!(env, Error::InvalidBoard);
            }
        }
        if threshold == 0 || threshold > members.len() {
            panic_with_error!(env, Error::InvalidThreshold);
        }
        env.storage()
            .instance()
            .set(&DataKey::Board, &BoardConfig { members, threshold });
        env.storage().instance().set(&DataKey::NextId, &0u64);
    }

    /// Propose a resolution and return its id. `action_hash` must be the
    /// SHA-256 of `execution_payload`'s XDR encoding, binding every approval
    /// to exactly this call. `expiration` is the last ledger sequence at
    /// which the resolution can be approved.
    pub fn propose_resolution(
        env: Env,
        proposer: Address,
        action_hash: BytesN<32>,
        execution_payload: ExecutionPayload,
        expiration: u32,
    ) -> u64 {
        proposer.require_auth();
        Self::require_member(&env, &Self::board(env.clone()), &proposer);

        if execution_payload.contract == env.current_contract_address() {
            panic_with_error!(env, Error::SelfInvocation);
        }
        let payload_hash: BytesN<32> = env
            .crypto()
            .sha256(&execution_payload.clone().to_xdr(&env))
            .into();
        if payload_hash != action_hash {
            panic_with_error!(env, Error::ActionHashMismatch);
        }

        let current = env.ledger().sequence();
        if expiration <= current || expiration - current > env.storage().max_ttl() {
            panic_with_error!(env, Error::InvalidExpiration);
        }

        let id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap();
        env.storage().instance().set(&DataKey::NextId, &(id + 1));

        let key = DataKey::Resolution(id);
        env.storage().persistent().set(
            &key,
            &Resolution {
                proposer: proposer.clone(),
                action_hash: action_hash.clone(),
                payload: execution_payload,
                expiration_ledger: expiration,
                approvals: Vec::new(&env),
                executed: false,
            },
        );
        // Keep the entry live through its expiration ledger.
        let ttl = expiration - current;
        env.storage().persistent().extend_ttl(&key, ttl, ttl);

        ResolutionProposed {
            id,
            proposer,
            action_hash,
            expiration_ledger: expiration,
        }
        .publish(&env);
        id
    }

    /// Record `member`'s approval of a resolution. `action_hash` is part of
    /// the authorization the member signs and must match the resolution's.
    /// The approval that reaches the threshold executes the payload in this
    /// transaction; if that call fails, the whole approval reverts.
    ///
    /// Returns whether the resolution was executed.
    pub fn approve_resolution(
        env: Env,
        member: Address,
        resolution_id: u64,
        action_hash: BytesN<32>,
    ) -> bool {
        member.require_auth();
        let board = Self::board(env.clone());
        Self::require_member(&env, &board, &member);

        let key = DataKey::Resolution(resolution_id);
        let mut resolution = Self::get_resolution(env.clone(), resolution_id);
        if resolution.executed {
            panic_with_error!(env, Error::AlreadyExecuted);
        }
        if env.ledger().sequence() > resolution.expiration_ledger {
            panic_with_error!(env, Error::ResolutionExpired);
        }
        if resolution.action_hash != action_hash {
            panic_with_error!(env, Error::ActionHashMismatch);
        }
        if resolution.approvals.contains(&member) {
            panic_with_error!(env, Error::AlreadyApproved);
        }

        resolution.approvals.push_back(member.clone());
        let approvals = resolution.approvals.len();
        ResolutionApproved {
            id: resolution_id,
            member,
            approvals,
        }
        .publish(&env);

        if approvals < board.threshold {
            env.storage().persistent().set(&key, &resolution);
            return false;
        }

        // Mark executed before the external call.
        resolution.executed = true;
        env.storage().persistent().set(&key, &resolution);
        let payload = resolution.payload;
        env.invoke_contract::<Val>(&payload.contract, &payload.function, payload.args);

        ResolutionExecuted {
            id: resolution_id,
            action_hash,
        }
        .publish(&env);
        true
    }

    pub fn get_resolution(env: Env, resolution_id: u64) -> Resolution {
        env.storage()
            .persistent()
            .get(&DataKey::Resolution(resolution_id))
            .unwrap_or_else(|| panic_with_error!(env, Error::ResolutionNotFound))
    }

    pub fn board(env: Env) -> BoardConfig {
        env.storage().instance().get(&DataKey::Board).unwrap()
    }

    // ---- internal ----

    fn require_member(env: &Env, board: &BoardConfig, address: &Address) {
        if !board.members.contains(address) {
            panic_with_error!(env, Error::NotBoardMember);
        }
    }
}
