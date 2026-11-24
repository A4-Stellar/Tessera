//! Board-approved transfers for regulated assets (issue #478).
//!
//! A [`MultiSigTransferPolicy`] names the asset token it governs, an amount
//! threshold and an `m`-of-`n` list of board signers. Above the threshold:
//!
//! 1. The holder calls `request_transfer(from, to, amount)`, which records a
//!    [`PendingTransfer`] in **temporary storage** that expires after
//!    `expiry_ledgers` (24 hours by default).
//! 2. Board members call `co_sign(signer, transfer_id)`. When the `m`-th
//!    distinct signature arrives, the compliance contract re-checks the KYC
//!    allowlist and velocity caps for both parties and executes the transfer
//!    through the asset token's `board_transfer`.
//!
//! The token's ordinary `transfer` / `transfer_from` refuse amounts above the
//! threshold (they ask [`requires_board_approval`]), so the board path cannot
//! be bypassed.
//!
//! Every step publishes an event (`ms_req`, `ms_sign`, `ms_exec`, `ms_pol`),
//! which the API indexes into the audit trail.
//!
//! Why `board_transfer` and not `transfer`: the token's `transfer` needs the
//! holder's signature in the executing transaction and calls back into this
//! contract for the compliance check. Soroban does not allow a contract to be
//! re-entered, so the compliance contract runs those checks itself and the
//! token accepts `board_transfer` only from its configured compliance
//! contract.

// Same event API as the rest of this contract (see `add_to_allowlist`).
#![allow(deprecated)]

use soroban_sdk::{
    contracttype, panic_with_error, symbol_short, Address, Env, IntoVal, String, Symbol, Val, Vec,
};

use crate::{ComplianceContract, Error};

/// Stellar closes a ledger about every five seconds: 24 h ≈ 17 280 ledgers.
pub const DEFAULT_EXPIRY_LEDGERS: u32 = 17_280;
/// Upper bound on the board size, which keeps `co_sign` cheap.
pub const MAX_BOARD_SIGNERS: u32 = 20;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigTransferPolicy {
    /// Asset token whose large transfers need board approval.
    pub token: Address,
    /// Transfers strictly above this amount need `required_signatures`.
    pub threshold_amount: i128,
    /// `m` in m-of-n.
    pub required_signatures: u32,
    /// `BoardSigners`: the `n` addresses allowed to co-sign.
    pub board: Vec<Address>,
    /// Lifetime of a pending transfer, in ledgers.
    pub expiry_ledgers: u32,
    /// Bumped on every policy change. Pending transfers created under an older
    /// version can no longer be signed.
    pub version: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingTransfer {
    pub id: u64,
    pub token: Address,
    pub from: Address,
    pub to: Address,
    pub amount: i128,
    pub created_at: u32,
    /// Last ledger on which the transfer can still be signed.
    pub expires_at: u32,
    pub required_signatures: u32,
    pub signers: Vec<Address>,
    pub policy_version: u32,
}

#[contracttype]
#[derive(Clone)]
pub(crate) enum MultiSigKey {
    Policy,
    NextTransferId,
    Pending(u64),
}

fn has_duplicates(env: &Env, board: &Vec<Address>) -> bool {
    let mut seen: Vec<Address> = Vec::new(env);
    for member in board.iter() {
        if seen.contains(&member) {
            return true;
        }
        seen.push_back(member);
    }
    false
}

pub(crate) fn set_policy(
    env: &Env,
    token: Address,
    threshold_amount: i128,
    required_signatures: u32,
    board: Vec<Address>,
    expiry_ledgers: u32,
) -> MultiSigTransferPolicy {
    let n = board.len();
    if threshold_amount <= 0
        || n == 0
        || n > MAX_BOARD_SIGNERS
        || required_signatures == 0
        || required_signatures > n
        || expiry_ledgers == 0
        || has_duplicates(env, &board)
    {
        panic_with_error!(env, Error::MultisigPolicyInvalid);
    }
    let version = policy(env).map(|p| p.version + 1).unwrap_or(1);
    let policy = MultiSigTransferPolicy {
        token,
        threshold_amount,
        required_signatures,
        board,
        expiry_ledgers,
        version,
    };
    env.storage().instance().set(&MultiSigKey::Policy, &policy);
    env.events().publish(
        (symbol_short!("ms_pol"), policy.token.clone()),
        (
            policy.threshold_amount,
            policy.required_signatures,
            policy.board.clone(),
            version,
            policy.expiry_ledgers,
        ),
    );
    policy
}

pub(crate) fn policy(env: &Env) -> Option<MultiSigTransferPolicy> {
    env.storage().instance().get(&MultiSigKey::Policy)
}

fn policy_or_panic(env: &Env) -> MultiSigTransferPolicy {
    policy(env).unwrap_or_else(|| panic_with_error!(env, Error::MultisigPolicyNotSet))
}

/// Whether a transfer of `amount` of `token` must go through the board.
pub fn requires_board_approval(env: &Env, token: &Address, amount: i128) -> bool {
    policy(env).is_some_and(|p| &p.token == token && amount > p.threshold_amount)
}

fn store_pending(env: &Env, pending: &PendingTransfer, ttl: u32) {
    let key = MultiSigKey::Pending(pending.id);
    env.storage().temporary().set(&key, pending);
    env.storage().temporary().extend_ttl(&key, ttl, ttl);
}

pub(crate) fn request(env: &Env, from: Address, to: Address, amount: i128) -> u64 {
    from.require_auth();
    let policy = policy_or_panic(env);
    if amount <= 0 {
        panic_with_error!(env, Error::MultisigInvalidAmount);
    }
    if amount <= policy.threshold_amount {
        panic_with_error!(env, Error::BelowMultisigThreshold);
    }
    // Fail early rather than collect signatures for a transfer that cannot
    // settle. Both are checked again on execution.
    if !ComplianceContract::is_allowed(env.clone(), from.clone())
        || !ComplianceContract::is_allowed(env.clone(), to.clone())
    {
        panic_with_error!(env, Error::MultisigTransferNotCompliant);
    }

    let id: u64 = env
        .storage()
        .instance()
        .get(&MultiSigKey::NextTransferId)
        .unwrap_or(1);
    env.storage()
        .instance()
        .set(&MultiSigKey::NextTransferId, &(id + 1));

    let now = env.ledger().sequence();
    let pending = PendingTransfer {
        id,
        token: policy.token.clone(),
        from: from.clone(),
        to: to.clone(),
        amount,
        created_at: now,
        expires_at: now.saturating_add(policy.expiry_ledgers),
        required_signatures: policy.required_signatures,
        signers: Vec::new(env),
        policy_version: policy.version,
    };
    store_pending(env, &pending, policy.expiry_ledgers);
    env.events().publish(
        (symbol_short!("ms_req"), id),
        (from, to, amount, pending.expires_at),
    );
    id
}

pub(crate) fn pending(env: &Env, id: u64) -> Option<PendingTransfer> {
    env.storage()
        .temporary()
        .get::<_, PendingTransfer>(&MultiSigKey::Pending(id))
        .filter(|p| env.ledger().sequence() <= p.expires_at)
}

/// Adds `signer`'s approval. Returns `true` when this signature executed the
/// transfer.
pub(crate) fn co_sign(env: &Env, signer: Address, id: u64) -> bool {
    signer.require_auth();
    let policy = policy_or_panic(env);
    let key = MultiSigKey::Pending(id);
    let mut pending: PendingTransfer = env
        .storage()
        .temporary()
        .get(&key)
        .unwrap_or_else(|| panic_with_error!(env, Error::PendingTransferNotFound));
    let now = env.ledger().sequence();
    if now > pending.expires_at {
        env.storage().temporary().remove(&key);
        panic_with_error!(env, Error::PendingTransferExpired);
    }
    if pending.policy_version != policy.version {
        panic_with_error!(env, Error::MultisigPolicyChanged);
    }
    if !policy.board.contains(&signer) {
        panic_with_error!(env, Error::NotBoardSigner);
    }
    if pending.signers.contains(&signer) {
        panic_with_error!(env, Error::AlreadySigned);
    }
    pending.signers.push_back(signer.clone());
    let count = pending.signers.len();
    env.events()
        .publish((symbol_short!("ms_sign"), id), (signer, count));

    if count < pending.required_signatures {
        // Keep the original expiry: only the remaining lifetime is re-applied.
        store_pending(env, &pending, pending.expires_at - now);
        return false;
    }

    execute(env, &pending);
    env.storage().temporary().remove(&key);
    true
}

fn execute(env: &Env, pending: &PendingTransfer) {
    if !ComplianceContract::is_allowed(env.clone(), pending.from.clone())
        || !ComplianceContract::is_allowed(env.clone(), pending.to.clone())
    {
        panic_with_error!(env, Error::MultisigTransferNotCompliant);
    }
    // Board approval is in addition to the velocity caps, not a bypass.
    if !crate::velocity::check_and_record(
        env,
        &pending.from,
        pending.amount,
        &String::from_str(env, ""),
    ) {
        panic_with_error!(env, Error::MultisigTransferNotCompliant);
    }
    let args: Vec<Val> = (pending.from.clone(), pending.to.clone(), pending.amount).into_val(env);
    env.invoke_contract::<()>(&pending.token, &Symbol::new(env, "board_transfer"), args);
    env.events().publish(
        (symbol_short!("ms_exec"), pending.id),
        (
            pending.from.clone(),
            pending.to.clone(),
            pending.amount,
            pending.signers.clone(),
        ),
    );
}
