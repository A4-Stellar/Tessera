//! Cap-table snapshot & Merkle proof verification contract (issue #13).
//!
//! This is new functionality, not a reconstruction of an already-deployed
//! contract — there is no `cap-table` entry among the four documented,
//! deployed contracts. It exists so distributing dividends (or otherwise
//! proving a holder's balance) to thousands of holders doesn't require
//! putting every holder's balance on-chain: an admin periodically publishes
//! a Merkle root committing to the full holder set for an asset token, and
//! any holder can then prove their balance against that root with an
//! `O(log n)`-sized proof instead of an on-chain enumeration.
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Bytes, BytesN, Env, Vec,
};

#[contracttype]
#[derive(Clone)]
pub struct Snapshot {
    pub id: u64,
    pub asset_token: Address,
    pub merkle_root: BytesN<32>,
    pub ledger: u32,
    pub total_holders: u32,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    SnapshotNotFound = 4,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    NextId,
    Snapshot(u64),
}

#[contract]
pub struct CapTableContract;

#[contractimpl]
impl CapTableContract {
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::NextId, &0u64);
    }

    /// Record a new cap-table snapshot for `asset_token`: `merkle_root`
    /// commits (via [`Self::verify_holder`]) to every holder's balance at
    /// the current ledger.
    pub fn submit_snapshot(
        env: Env,
        admin: Address,
        asset_token: Address,
        merkle_root: BytesN<32>,
        total_holders: u32,
    ) -> u64 {
        Self::require_admin(&env, &admin);

        let id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap();
        env.storage().instance().set(&DataKey::NextId, &(id + 1));

        let snapshot = Snapshot {
            id,
            asset_token: asset_token.clone(),
            merkle_root,
            ledger: env.ledger().sequence(),
            total_holders,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Snapshot(id), &snapshot);

        env.events()
            .publish((symbol_short!("snapshot"), admin), (id, asset_token));
        id
    }

    pub fn get_snapshot(env: Env, id: u64) -> Snapshot {
        env.storage()
            .persistent()
            .get(&DataKey::Snapshot(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::SnapshotNotFound))
    }

    /// Verify that `holder` held `balance` at `snapshot_id`'s recorded
    /// Merkle root.
    ///
    /// The leaf is `sha256(holder XDR bytes ++ balance big-endian bytes)`.
    /// The proof is then folded bottom-up to the root using SORTED-PAIR
    /// hashing at each level (`sha256(min(a, b) ++ max(a, b))`) rather than
    /// naive left/right concatenation, so a verifier does not need to also
    /// carry left/right-branch metadata, and the tree is not vulnerable to
    /// the classic ambiguous-ordering second-preimage trick. Returns
    /// `false` (never panics) for an unknown snapshot or a proof that
    /// doesn't fold to the stored root.
    pub fn verify_holder(
        env: Env,
        snapshot_id: u64,
        holder: Address,
        balance: i128,
        proof: Vec<BytesN<32>>,
    ) -> bool {
        let snapshot: Option<Snapshot> = env.storage().persistent().get(&DataKey::Snapshot(snapshot_id));
        let snapshot = match snapshot {
            Some(s) => s,
            None => return false,
        };

        let mut leaf_input = Bytes::new(&env);
        leaf_input.append(&holder.to_xdr(&env));
        leaf_input.append(&Bytes::from_array(&env, &balance.to_be_bytes()));
        let mut node: BytesN<32> = env.crypto().sha256(&leaf_input).into();

        for sibling in proof.iter() {
            node = Self::hash_pair(&env, &node, &sibling);
        }

        node == snapshot.merkle_root
    }

    /// Current contract ABI version, polled by the off-chain indexer.
    pub fn version(_env: Env) -> u64 {
        1
    }

    // ---- internal ----

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    /// `sha256(min(a, b) ++ max(a, b))`, ordered by raw byte value so that
    /// the same pair of siblings always folds to the same parent regardless
    /// of which side of the tree each one came from.
    fn hash_pair(env: &Env, a: &BytesN<32>, b: &BytesN<32>) -> BytesN<32> {
        let (lo, hi) = if a.to_array() <= b.to_array() {
            (a, b)
        } else {
            (b, a)
        };
        let mut combined = Bytes::new(env);
        combined.append(&Bytes::from(lo.clone()));
        combined.append(&Bytes::from(hi.clone()));
        env.crypto().sha256(&combined).into()
    }
}
