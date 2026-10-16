//! Validation and execution of a multilateral settlement batch.
//!
//! A batch is a list of [`TradeLeg`]s. Before any funds move, [`validate`]
//! enforces, in order:
//!
//! 1. **Shape** — 1..=[`MAX_LEGS`] legs, every amount positive, no leg sending
//!    a token from an account to itself, every token registered.
//! 2. **Directed cycles** — accounts are nodes and each leg is an edge
//!    `from -> to`. Every edge must lie on a directed cycle (its recipient can
//!    reach its sender), so each participant who gives also receives and no
//!    leg is a one-way gift smuggled into someone else's trade. Reachability
//!    is a bitset transitive closure: with at most 32 legs there are at most
//!    64 accounts, so each node's reach set fits one `u64`.
//! 3. **Conservation of balance** — per token, total inflow equals total
//!    outflow (checked arithmetic, so a batch cannot overflow its way past
//!    the check), and every *intermediary* — an account that both receives
//!    and sends the same token in the batch — passes that token through in
//!    full: its net inflow equals its net outflow.
//! 4. **Compliance** — for every leg in a regulated token, the token's
//!    compliance contract must answer `is_allowed` with `true` for both the
//!    sender and the recipient. Answers are cached per (token, account) for
//!    the duration of the call.
//! 5. **Funding** — the legs are ordered so that no sender is ever overdrawn,
//!    starting from live on-chain balances and crediting each leg's recipient
//!    as it runs. Pass-through intermediaries therefore do not need to
//!    pre-fund what they forward. If no such order exists the batch fails.
//!
//! [`settle`] additionally requires every distinct sender's authorization
//! and then executes the legs in the funded order. Because all of this runs
//! in one Soroban invocation, any failure — including one raised by a token
//! contract mid-execution (e.g. a paused token or a lockup the engine cannot
//! see) — reverts every leg already applied.

use soroban_sdk::{
    contracttype, panic_with_error, token, Address, Env, IntoVal, Map, Symbol, Val, Vec,
};

use crate::{DataKey, Error};

/// Maximum number of legs in one batch. Bounds every loop below and keeps the
/// account graph within 64 nodes (one `u64` reach set per node).
pub const MAX_LEGS: u32 = 32;

/// One transfer within a batch: `amount` of `token` from `from` to `to`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradeLeg {
    pub from: Address,
    pub to: Address,
    pub token: Address,
    pub amount: i128,
}

/// How a registered token is gated.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetPolicy {
    /// Every leg's sender and recipient must pass this compliance contract's
    /// `is_allowed(address) -> bool`.
    Regulated(Address),
    /// Explicitly approved by the admin as ungated (e.g. a USDC cash leg).
    Unregulated,
}

/// A leg resolved to node/token indices so the checks can use bitsets and
/// small maps instead of repeated address comparisons.
struct IndexedLegs {
    accounts: Vec<Address>,
    tokens: Vec<Address>,
    /// Per leg: (from node, to node, token index).
    edges: Vec<(u32, u32, u32)>,
    /// Per token index: its policy.
    policies: Vec<AssetPolicy>,
}

/// Check every rule and return the leg indices in a funded execution order.
pub(crate) fn validate(env: &Env, legs: &Vec<TradeLeg>) -> Vec<u32> {
    let indexed = index_legs(env, legs);
    check_cycles(env, &indexed);
    check_conservation(env, legs, &indexed);
    check_compliance(env, &indexed);
    funded_order(env, legs, &indexed)
}

/// Authorize, validate and execute the batch atomically.
pub(crate) fn settle(env: &Env, legs: &Vec<TradeLeg>) {
    // Validation first so a malformed batch fails with a precise error, then
    // auth: one `require_auth` per distinct sender.
    let order = validate(env, legs);

    let mut authorized: Vec<Address> = Vec::new(env);
    for leg in legs.iter() {
        if !authorized.contains(&leg.from) {
            leg.from.require_auth();
            authorized.push_back(leg.from.clone());
        }
    }

    for i in order.iter() {
        let leg = legs.get_unchecked(i);
        token::TokenClient::new(env, &leg.token).transfer(&leg.from, &leg.to, &leg.amount);
    }
}

fn index_of(list: &mut Vec<Address>, addr: &Address) -> u32 {
    match list.first_index_of(addr) {
        Some(i) => i,
        None => {
            list.push_back(addr.clone());
            list.len() - 1
        }
    }
}

fn index_legs(env: &Env, legs: &Vec<TradeLeg>) -> IndexedLegs {
    if legs.is_empty() || legs.len() > MAX_LEGS {
        panic_with_error!(env, Error::InvalidBatchSize);
    }

    let mut indexed = IndexedLegs {
        accounts: Vec::new(env),
        tokens: Vec::new(env),
        edges: Vec::new(env),
        policies: Vec::new(env),
    };

    for leg in legs.iter() {
        if leg.amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        if leg.from == leg.to {
            panic_with_error!(env, Error::SelfTransfer);
        }

        let token_count = indexed.tokens.len();
        let t = index_of(&mut indexed.tokens, &leg.token);
        if t == token_count {
            let policy: AssetPolicy = env
                .storage()
                .persistent()
                .get(&DataKey::Asset(leg.token.clone()))
                .unwrap_or_else(|| panic_with_error!(env, Error::AssetNotRegistered));
            indexed.policies.push_back(policy);
        }

        let u = index_of(&mut indexed.accounts, &leg.from);
        let v = index_of(&mut indexed.accounts, &leg.to);
        indexed.edges.push_back((u, v, t));
    }
    indexed
}

fn check_cycles(env: &Env, indexed: &IndexedLegs) {
    // At most 2 * MAX_LEGS = 64 accounts, so a u64 holds one reach set.
    let n = indexed.accounts.len() as usize;
    let mut reach = [0u64; 64];
    for (u, v, _) in indexed.edges.iter() {
        reach[u as usize] |= 1u64 << v;
    }
    // Warshall's transitive closure over bitsets: O(n^2) word operations.
    for k in 0..n {
        let via = reach[k];
        let bit = 1u64 << k;
        for row in reach.iter_mut().take(n) {
            if *row & bit != 0 {
                *row |= via;
            }
        }
    }
    // Edge u -> v is on a cycle iff v reaches u.
    for (u, v, _) in indexed.edges.iter() {
        if reach[v as usize] & (1u64 << u) == 0 {
            panic_with_error!(env, Error::LegNotInCycle);
        }
    }
}

fn check_conservation(env: &Env, legs: &Vec<TradeLeg>, indexed: &IndexedLegs) {
    // (account, token) -> (inflow, outflow)
    let mut flows: Map<(u32, u32), (i128, i128)> = Map::new(env);
    // token -> (total inflow, total outflow)
    let mut totals: Map<u32, (i128, i128)> = Map::new(env);

    for (i, (u, v, t)) in indexed.edges.iter().enumerate() {
        let amount = legs.get_unchecked(i as u32).amount;

        let (in_u, out_u) = flows.get((u, t)).unwrap_or((0, 0));
        flows.set((u, t), (in_u, add(env, out_u, amount)));
        let (in_v, out_v) = flows.get((v, t)).unwrap_or((0, 0));
        flows.set((v, t), (add(env, in_v, amount), out_v));

        let (tin, tout) = totals.get(t).unwrap_or((0, 0));
        totals.set(t, (add(env, tin, amount), add(env, tout, amount)));
    }

    for (_, (tin, tout)) in totals.iter() {
        if tin != tout {
            panic_with_error!(env, Error::ConservationViolation);
        }
    }
    for (_, (inflow, outflow)) in flows.iter() {
        if inflow > 0 && outflow > 0 && inflow != outflow {
            panic_with_error!(env, Error::ConservationViolation);
        }
    }
}

fn check_compliance(env: &Env, indexed: &IndexedLegs) {
    let is_allowed = Symbol::new(env, "is_allowed");
    // (token, account) -> verdict, so each pair costs one cross-contract call.
    let mut cache: Map<(u32, u32), bool> = Map::new(env);

    let mut allowed = |token: u32, account: u32, compliance: &Address| -> bool {
        if let Some(v) = cache.get((token, account)) {
            return v;
        }
        let who = indexed.accounts.get_unchecked(account);
        let args: Vec<Val> = (who,).into_val(env);
        let v: bool = env.invoke_contract(compliance, &is_allowed, args);
        cache.set((token, account), v);
        v
    };

    for (u, v, t) in indexed.edges.iter() {
        if let AssetPolicy::Regulated(compliance) = indexed.policies.get_unchecked(t) {
            if !allowed(t, u, &compliance) {
                panic_with_error!(env, Error::SenderNotCompliant);
            }
            if !allowed(t, v, &compliance) {
                panic_with_error!(env, Error::RecipientNotCompliant);
            }
        }
    }
}

/// Greedily order the legs so that every sender can fund its leg when it
/// runs, simulating balances from live chain state. Returns leg indices.
fn funded_order(env: &Env, legs: &Vec<TradeLeg>, indexed: &IndexedLegs) -> Vec<u32> {
    // Simulated balance for every (sender, token) position.
    let mut balances: Map<(u32, u32), i128> = Map::new(env);
    for (u, _, t) in indexed.edges.iter() {
        if !balances.contains_key((u, t)) {
            let holder = indexed.accounts.get_unchecked(u);
            let token = indexed.tokens.get_unchecked(t);
            let bal = token::TokenClient::new(env, &token).balance(&holder);
            balances.set((u, t), bal);
        }
    }

    let count = indexed.edges.len();
    let mut pending: u64 = if count == 64 {
        u64::MAX
    } else {
        (1u64 << count) - 1
    };
    let mut order: Vec<u32> = Vec::new(env);

    while pending != 0 {
        let mut progressed = false;
        for i in 0..count {
            if pending & (1u64 << i) == 0 {
                continue;
            }
            let (u, v, t) = indexed.edges.get_unchecked(i);
            let amount = legs.get_unchecked(i).amount;
            let bal = balances.get((u, t)).unwrap_or(0);
            if bal < amount {
                continue;
            }
            balances.set((u, t), bal - amount);
            // Only senders' positions matter; a recipient that never sends
            // this token has no entry and needs none.
            if let Some(to_bal) = balances.get((v, t)) {
                balances.set((v, t), add(env, to_bal, amount));
            }
            pending &= !(1u64 << i);
            order.push_back(i);
            progressed = true;
        }
        if !progressed {
            panic_with_error!(env, Error::InsufficientBalance);
        }
    }
    order
}

fn add(env: &Env, a: i128, b: i128) -> i128 {
    a.checked_add(b)
        .unwrap_or_else(|| panic_with_error!(env, Error::Overflow))
}
