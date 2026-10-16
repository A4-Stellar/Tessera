//! Merkle Mountain Range checkpoints of indexed ledgers (issue #318).
//!
//! Mobile apps and external auditors cannot run the indexer, so after every
//! successful refresh the indexer appends one leaf committing to
//! `(ledger_sequence, ledger_hash, contract_state_root)` to an append-only
//! MMR. A light client that holds a trusted MMR root can then verify, from a
//! logarithmic-size proof, that a given cap-table entry was what the indexer
//! saw at a given ledger — without trusting the API response itself.
//!
//! ## Hashing (all SHA-256, domain-separated by a one-byte prefix)
//!
//! | value | preimage |
//! |-------|----------|
//! | MMR leaf | `0x00 ‖ ledger_seq (u32 BE) ‖ ledger_hash (32) ‖ state_root (32)` |
//! | MMR node | `0x01 ‖ left (32) ‖ right (32)` |
//! | peak bag | `0x02 ‖ peak (32) ‖ acc (32)`, folded right to left |
//! | MMR root | `0x03 ‖ leaf_count (u64 BE) ‖ bagged_peaks (32)` |
//! | state leaf | `0x10 ‖ asset_id (u64 BE) ‖ address_len (u32 BE) ‖ address (utf-8) ‖ balance (i128 BE)` |
//! | state node | `0x11 ‖ left (32) ‖ right (32)` |
//! | empty state | `0x12` |
//!
//! The state tree is a binary Merkle tree over the snapshot's holder
//! entries sorted by `(asset_id, address)`; an odd node at the end of a
//! level is carried up unchanged. The MMR is a list of perfect binary
//! "mountains", one per set bit of the leaf count, largest first. Both proof
//! formats carry only sibling hashes plus `(leaf_index, leaf_count)`; the
//! verifier derives the left/right order from those, so a proof cannot
//! smuggle in a different position.
//!
//! ## Incremental maintenance
//!
//! Nodes are stored per level (`levels[h][i]` = root of the `i`-th perfect
//! subtree of height `h`). Appending a leaf pushes it to level 0 and merges
//! upward only while the level just written has an even length, so each
//! append costs `O(log n)` hashes and never touches an existing node.
//!
//! ## Scope
//!
//! The MMR itself is kept for every checkpoint since start-up. The per-ledger
//! state entries needed to answer `?account=` proofs are retained only for
//! the most recent [`DEFAULT_STATE_RETENTION`] checkpoints (configurable via
//! `RWA_MMR_STATE_RETENTION`); older ledgers still get their MMR inclusion
//! proof. State is in memory and rebuilt from the first refresh after a
//! restart, so a restart starts a new MMR — clients must re-anchor their
//! trusted root.

use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::models::Holder;

pub type Hash = [u8; 32];

/// Checkpoints whose full state entries are kept for `?account=` proofs.
/// At the 10 s poll interval this is roughly one day.
pub const DEFAULT_STATE_RETENTION: usize = 8_640;

const MMR_LEAF: u8 = 0x00;
const MMR_NODE: u8 = 0x01;
const MMR_BAG: u8 = 0x02;
const MMR_ROOT: u8 = 0x03;
const STATE_LEAF: u8 = 0x10;
const STATE_NODE: u8 = 0x11;
const STATE_EMPTY: u8 = 0x12;

fn hash_parts(parts: &[&[u8]]) -> Hash {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn node_hash(prefix: u8, left: &Hash, right: &Hash) -> Hash {
    hash_parts(&[&[prefix], left, right])
}

pub fn mmr_leaf_hash(ledger_seq: u32, ledger_hash: &Hash, state_root: &Hash) -> Hash {
    hash_parts(&[
        &[MMR_LEAF],
        &ledger_seq.to_be_bytes(),
        ledger_hash,
        state_root,
    ])
}

/// Root over the peaks (largest mountain first) and the leaf count.
pub fn bag_peaks(peaks: &[Hash], leaf_count: u64) -> Hash {
    let bagged = match peaks.split_last() {
        None => [0u8; 32],
        Some((last, rest)) => rest
            .iter()
            .rev()
            .fold(*last, |acc, peak| node_hash(MMR_BAG, peak, &acc)),
    };
    hash_parts(&[&[MMR_ROOT], &leaf_count.to_be_bytes(), &bagged])
}

/// Mountains of an MMR with `leaf_count` leaves, largest first, as
/// `(first_leaf_index, height)`.
fn mountains(leaf_count: u64) -> Vec<(u64, u32)> {
    let mut out = Vec::new();
    let mut start = 0u64;
    for height in (0..64u32).rev() {
        if leaf_count & (1u64 << height) != 0 {
            out.push((start, height));
            start += 1u64 << height;
        }
    }
    out
}

/// Append-only Merkle Mountain Range.
#[derive(Debug, Default, Clone)]
pub struct Mmr {
    /// `levels[h][i]`: root of the `i`-th perfect subtree of height `h`.
    levels: Vec<Vec<Hash>>,
}

/// Inclusion proof of one leaf in an MMR of `leaf_count` leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmrProof {
    pub leaf_index: u64,
    pub leaf_count: u64,
    /// Siblings from the leaf up to its mountain's peak.
    pub path: Vec<Hash>,
    /// Every peak, largest mountain first (the leaf's own peak included).
    pub peaks: Vec<Hash>,
}

impl Mmr {
    pub fn leaf_count(&self) -> u64 {
        self.levels.first().map_or(0, |l| l.len() as u64)
    }

    /// Append a leaf; returns its index. `O(log n)` hashes, and no existing
    /// node is ever recomputed.
    pub fn append(&mut self, leaf: Hash) -> u64 {
        if self.levels.is_empty() {
            self.levels.push(Vec::new());
        }
        self.levels[0].push(leaf);
        let index = self.levels[0].len() as u64 - 1;

        let mut h = 0;
        while self.levels[h].len() % 2 == 0 {
            let len = self.levels[h].len();
            let parent = node_hash(MMR_NODE, &self.levels[h][len - 2], &self.levels[h][len - 1]);
            if self.levels.len() == h + 1 {
                self.levels.push(Vec::new());
            }
            self.levels[h + 1].push(parent);
            h += 1;
        }
        index
    }

    pub fn peaks(&self) -> Vec<Hash> {
        mountains(self.leaf_count())
            .into_iter()
            .map(|(start, height)| self.levels[height as usize][(start >> height) as usize])
            .collect()
    }

    pub fn root(&self) -> Hash {
        bag_peaks(&self.peaks(), self.leaf_count())
    }

    pub fn proof(&self, leaf_index: u64) -> Option<MmrProof> {
        let leaf_count = self.leaf_count();
        if leaf_index >= leaf_count {
            return None;
        }
        let (start, height) = mountains(leaf_count)
            .into_iter()
            .find(|(start, height)| leaf_index < start + (1u64 << height))?;
        debug_assert!(leaf_index >= start);

        let mut path = Vec::with_capacity(height as usize);
        let mut idx = leaf_index;
        for h in 0..height as usize {
            path.push(self.levels[h][(idx ^ 1) as usize]);
            idx >>= 1;
        }
        Some(MmrProof {
            leaf_index,
            leaf_count,
            path,
            peaks: self.peaks(),
        })
    }
}

/// Recompute the MMR root from a leaf and its proof (the same algorithm the
/// TypeScript verifier in `sdk/typescript/src/mmr.ts` implements).
#[allow(
    dead_code,
    reason = "reference verifier; the server only produces proofs"
)]
pub fn verify_mmr_proof(leaf: &Hash, proof: &MmrProof, root: &Hash) -> bool {
    let all = mountains(proof.leaf_count);
    if all.len() != proof.peaks.len() || proof.leaf_index >= proof.leaf_count {
        return false;
    }
    let Some(pos) = all
        .iter()
        .position(|(start, height)| proof.leaf_index < start + (1u64 << height))
    else {
        return false;
    };
    let (start, height) = all[pos];
    if proof.path.len() != height as usize {
        return false;
    }
    let mut acc = *leaf;
    let mut idx = proof.leaf_index - start;
    for sibling in &proof.path {
        acc = if idx & 1 == 0 {
            node_hash(MMR_NODE, &acc, sibling)
        } else {
            node_hash(MMR_NODE, sibling, &acc)
        };
        idx >>= 1;
    }
    acc == proof.peaks[pos] && bag_peaks(&proof.peaks, proof.leaf_count) == *root
}

/// One cap-table entry committed to by a state root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StateEntry {
    pub asset_id: u64,
    pub address: String,
    pub balance: i128,
}

impl StateEntry {
    pub fn leaf_hash(&self) -> Hash {
        hash_parts(&[
            &[STATE_LEAF],
            &self.asset_id.to_be_bytes(),
            &(self.address.len() as u32).to_be_bytes(),
            self.address.as_bytes(),
            &self.balance.to_be_bytes(),
        ])
    }
}

/// Sorted cap-table entries and their Merkle tree.
#[derive(Debug, Clone)]
pub struct StateTree {
    entries: Vec<StateEntry>,
    /// `levels[0]` = leaf hashes; last level has one element (or none).
    levels: Vec<Vec<Hash>>,
}

impl StateTree {
    pub fn new(mut entries: Vec<StateEntry>) -> Self {
        entries.sort();
        entries.dedup();
        let mut levels = vec![entries
            .iter()
            .map(StateEntry::leaf_hash)
            .collect::<Vec<_>>()];
        while levels.last().is_some_and(|l| l.len() > 1) {
            let prev = levels.last().expect("non-empty");
            let next = prev
                .chunks(2)
                .map(|pair| match pair {
                    [l, r] => node_hash(STATE_NODE, l, r),
                    [only] => *only,
                    _ => unreachable!(),
                })
                .collect();
            levels.push(next);
        }
        StateTree { entries, levels }
    }

    /// Build from the indexer snapshot's per-asset holder lists. Balances
    /// that fail to parse are skipped (they cannot be committed faithfully).
    pub fn from_holders<'a>(holders: impl IntoIterator<Item = (u64, &'a [Holder])>) -> Self {
        let entries = holders
            .into_iter()
            .flat_map(|(asset_id, list)| {
                list.iter().filter_map(move |h| {
                    h.balance.parse::<i128>().ok().map(|balance| StateEntry {
                        asset_id,
                        address: h.address.clone(),
                        balance,
                    })
                })
            })
            .collect();
        StateTree::new(entries)
    }

    pub fn root(&self) -> Hash {
        match self.levels.last().and_then(|l| l.first()) {
            Some(root) => *root,
            None => hash_parts(&[&[STATE_EMPTY]]),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Entries for `address`, each with its index and sibling path.
    pub fn proofs_for(&self, address: &str) -> Vec<(StateEntry, u64, Vec<Hash>)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.address == address)
            .map(|(i, e)| (e.clone(), i as u64, self.path(i)))
            .collect()
    }

    fn path(&self, index: usize) -> Vec<Hash> {
        let mut out = Vec::new();
        let mut idx = index;
        for level in &self.levels[..self.levels.len().saturating_sub(1)] {
            let sibling = idx ^ 1;
            if sibling < level.len() {
                out.push(level[sibling]);
            }
            idx >>= 1;
        }
        out
    }
}

/// Recompute a state root from one entry and its sibling path.
#[allow(
    dead_code,
    reason = "reference verifier; the server only produces proofs"
)]
pub fn verify_state_proof(
    leaf: &Hash,
    leaf_index: u64,
    leaf_count: u64,
    path: &[Hash],
    root: &Hash,
) -> bool {
    if leaf_index >= leaf_count {
        return false;
    }
    let mut acc = *leaf;
    let mut idx = leaf_index;
    let mut size = leaf_count;
    let mut siblings = path.iter();
    while size > 1 {
        let sibling_idx = idx ^ 1;
        if sibling_idx < size {
            let Some(sibling) = siblings.next() else {
                return false;
            };
            acc = if idx & 1 == 0 {
                node_hash(STATE_NODE, &acc, sibling)
            } else {
                node_hash(STATE_NODE, sibling, &acc)
            };
        }
        idx >>= 1;
        size = size.div_ceil(2);
    }
    siblings.next().is_none() && acc == *root
}

/// A committed checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub ledger_sequence: u32,
    pub ledger_hash: Hash,
    pub state_root: Hash,
    pub leaf_index: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CheckpointError {
    #[error("ledger {0} is not after the last checkpointed ledger {1}")]
    NotMonotonic(u32, u32),
}

/// The MMR plus the index and retained state needed to serve proofs.
#[derive(Debug)]
pub struct MmrCheckpoints {
    mmr: Mmr,
    by_ledger: BTreeMap<u32, Checkpoint>,
    states: VecDeque<(u32, StateTree)>,
    retention: usize,
}

impl Default for MmrCheckpoints {
    fn default() -> Self {
        Self::new(
            std::env::var("RWA_MMR_STATE_RETENTION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_STATE_RETENTION),
        )
    }
}

impl MmrCheckpoints {
    pub fn new(retention: usize) -> Self {
        MmrCheckpoints {
            mmr: Mmr::default(),
            by_ledger: BTreeMap::new(),
            states: VecDeque::new(),
            retention,
        }
    }

    /// Append a checkpoint for `ledger_sequence`. Ledgers must strictly
    /// increase; a repeat of the last ledger is ignored (`Ok(None)`), since a
    /// refresh that did not advance has nothing new to commit.
    pub fn record(
        &mut self,
        ledger_sequence: u32,
        ledger_hash: Hash,
        state: StateTree,
    ) -> Result<Option<Checkpoint>, CheckpointError> {
        if let Some((&last, _)) = self.by_ledger.last_key_value() {
            if ledger_sequence == last {
                return Ok(None);
            }
            if ledger_sequence < last {
                return Err(CheckpointError::NotMonotonic(ledger_sequence, last));
            }
        }
        let state_root = state.root();
        let leaf_index = self
            .mmr
            .append(mmr_leaf_hash(ledger_sequence, &ledger_hash, &state_root));
        let cp = Checkpoint {
            ledger_sequence,
            ledger_hash,
            state_root,
            leaf_index,
        };
        self.by_ledger.insert(ledger_sequence, cp.clone());
        if self.retention > 0 {
            self.states.push_back((ledger_sequence, state));
            while self.states.len() > self.retention {
                self.states.pop_front();
            }
        }
        Ok(Some(cp))
    }

    pub fn leaf_count(&self) -> u64 {
        self.mmr.leaf_count()
    }

    pub fn root(&self) -> Hash {
        self.mmr.root()
    }

    pub fn latest(&self) -> Option<&Checkpoint> {
        self.by_ledger.values().next_back()
    }

    pub fn checkpoint(&self, ledger_sequence: u32) -> Option<&Checkpoint> {
        self.by_ledger.get(&ledger_sequence)
    }

    pub fn proof(&self, ledger_sequence: u32) -> Option<(Checkpoint, MmrProof)> {
        let cp = self.by_ledger.get(&ledger_sequence)?;
        let proof = self.mmr.proof(cp.leaf_index)?;
        Some((cp.clone(), proof))
    }

    /// State tree retained for `ledger_sequence`, if still within retention.
    pub fn state(&self, ledger_sequence: u32) -> Option<&StateTree> {
        self.states
            .iter()
            .find(|(seq, _)| *seq == ledger_sequence)
            .map(|(_, s)| s)
    }
}

// ---- JSON response shapes -------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MmrRootResponse {
    pub leaf_count: u64,
    pub root: String,
    pub latest_ledger_sequence: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct StateEntryProof {
    pub asset_id: u64,
    pub address: String,
    pub balance: String,
    pub leaf_index: u64,
    pub path: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AccountProof {
    pub address: String,
    pub state_leaf_count: u64,
    pub entries: Vec<StateEntryProof>,
}

#[derive(Debug, Serialize)]
pub struct MmrProofResponse {
    pub ledger_sequence: u32,
    pub ledger_hash: String,
    pub contract_state_root: String,
    pub leaf_hash: String,
    pub leaf_index: u64,
    pub leaf_count: u64,
    pub path: Vec<String>,
    pub peaks: Vec<String>,
    pub root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountProof>,
}

pub fn hex32(h: &Hash) -> String {
    hex::encode(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(i: u32) -> Hash {
        mmr_leaf_hash(i, &[i as u8; 32], &[0xAB; 32])
    }

    #[test]
    fn every_leaf_proves_against_the_root_for_many_sizes() {
        let mut mmr = Mmr::default();
        for n in 1..=70u32 {
            mmr.append(leaf(n));
            let root = mmr.root();
            for i in 0..n as u64 {
                let proof = mmr.proof(i).expect("leaf exists");
                assert!(
                    verify_mmr_proof(&leaf(i as u32 + 1), &proof, &root),
                    "leaf {i} of {n}"
                );
            }
        }
    }

    #[test]
    fn append_never_rewrites_existing_nodes() {
        let mut mmr = Mmr::default();
        for n in 1..=40u32 {
            let before = mmr.levels.clone();
            mmr.append(leaf(n));
            for (h, level) in before.iter().enumerate() {
                assert_eq!(&mmr.levels[h][..level.len()], &level[..], "level {h}");
            }
        }
    }

    #[test]
    fn peaks_match_binary_decomposition() {
        let mut mmr = Mmr::default();
        for n in 1..=11u32 {
            mmr.append(leaf(n));
        }
        // 11 = 8 + 2 + 1
        assert_eq!(mmr.peaks().len(), 3);
        assert_eq!(mountains(11), vec![(0, 3), (8, 1), (10, 0)]);
    }

    #[test]
    fn tampered_proofs_fail() {
        let mut mmr = Mmr::default();
        for n in 1..=13u32 {
            mmr.append(leaf(n));
        }
        let root = mmr.root();
        let good = mmr.proof(5).unwrap();
        assert!(verify_mmr_proof(&leaf(6), &good, &root));

        assert!(!verify_mmr_proof(&leaf(7), &good, &root), "wrong leaf");

        let mut moved = good.clone();
        moved.leaf_index = 4;
        assert!(!verify_mmr_proof(&leaf(6), &moved, &root), "wrong index");

        let mut bad_path = good.clone();
        bad_path.path[0][0] ^= 1;
        assert!(!verify_mmr_proof(&leaf(6), &bad_path, &root));

        let mut wrong_count = good.clone();
        wrong_count.leaf_count = 12;
        assert!(!verify_mmr_proof(&leaf(6), &wrong_count, &root));

        let mut stale = Mmr::default();
        for n in 1..=12u32 {
            stale.append(leaf(n));
        }
        assert!(!verify_mmr_proof(&leaf(6), &good, &stale.root()));
    }

    fn entries(n: u64) -> Vec<StateEntry> {
        (0..n)
            .map(|i| StateEntry {
                asset_id: i % 3,
                address: format!("G{i:055}"),
                balance: (i as i128 + 1) * 1_000,
            })
            .collect()
    }

    #[test]
    fn state_proofs_verify_for_odd_and_even_sizes() {
        for n in 1..=17u64 {
            let tree = StateTree::new(entries(n));
            let root = tree.root();
            for e in entries(n) {
                let proofs = tree.proofs_for(&e.address);
                assert_eq!(proofs.len(), 1);
                let (entry, idx, path) = &proofs[0];
                assert!(verify_state_proof(
                    &entry.leaf_hash(),
                    *idx,
                    tree.len() as u64,
                    path,
                    &root
                ));
                // A different balance must not verify.
                let mut forged = entry.clone();
                forged.balance += 1;
                assert!(!verify_state_proof(
                    &forged.leaf_hash(),
                    *idx,
                    tree.len() as u64,
                    path,
                    &root
                ));
            }
        }
    }

    #[test]
    fn state_root_is_order_independent() {
        let mut shuffled = entries(9);
        shuffled.reverse();
        assert_eq!(
            StateTree::new(entries(9)).root(),
            StateTree::new(shuffled).root()
        );
        assert_ne!(
            StateTree::new(entries(9)).root(),
            StateTree::new(entries(8)).root()
        );
    }

    #[test]
    fn checkpoints_are_monotonic_and_retained() {
        let mut cps = MmrCheckpoints::new(2);
        assert!(cps
            .record(10, [1; 32], StateTree::new(entries(2)))
            .unwrap()
            .is_some());
        assert_eq!(
            cps.record(10, [1; 32], StateTree::new(entries(2))),
            Ok(None)
        );
        assert_eq!(
            cps.record(9, [1; 32], StateTree::new(vec![])).unwrap_err(),
            CheckpointError::NotMonotonic(9, 10)
        );
        cps.record(11, [2; 32], StateTree::new(entries(3))).unwrap();
        cps.record(12, [3; 32], StateTree::new(entries(4))).unwrap();
        assert_eq!(cps.leaf_count(), 3);
        assert!(cps.state(10).is_none(), "evicted past retention");
        assert!(cps.state(12).is_some());

        // Old ledgers keep their MMR proof even after their state is evicted.
        let (cp, proof) = cps.proof(10).unwrap();
        let leaf = mmr_leaf_hash(cp.ledger_sequence, &cp.ledger_hash, &cp.state_root);
        assert!(verify_mmr_proof(&leaf, &proof, &cps.root()));
    }

    /// Fixed vector shared with `sdk/typescript/src/mmr.test.ts`, so the two
    /// verifiers are pinned to the same hashing rules.
    #[test]
    fn cross_language_vector() {
        let mut cps = MmrCheckpoints::new(16);
        for seq in 100..105u32 {
            let state = StateTree::new(vec![
                StateEntry {
                    asset_id: 1,
                    address: "GALICE".into(),
                    balance: 500 + seq as i128,
                },
                StateEntry {
                    asset_id: 2,
                    address: "GBOB".into(),
                    balance: 42,
                },
                StateEntry {
                    asset_id: 1,
                    address: "GBOB".into(),
                    balance: 7,
                },
            ]);
            cps.record(seq, [seq as u8; 32], state).unwrap();
        }
        let (cp, proof) = cps.proof(102).unwrap();
        let state = cps.state(102).unwrap();
        let alice = &state.proofs_for("GALICE")[0];
        let vector = serde_json::json!({
            "root": hex32(&cps.root()),
            "ledger_sequence": cp.ledger_sequence,
            "ledger_hash": hex32(&cp.ledger_hash),
            "contract_state_root": hex32(&cp.state_root),
            "leaf_index": proof.leaf_index,
            "leaf_count": proof.leaf_count,
            "path": proof.path.iter().map(hex32).collect::<Vec<_>>(),
            "peaks": proof.peaks.iter().map(hex32).collect::<Vec<_>>(),
            "state_leaf_count": state.len(),
            "alice": {
                "asset_id": alice.0.asset_id,
                "balance": alice.0.balance.to_string(),
                "leaf_index": alice.1,
                "path": alice.2.iter().map(hex32).collect::<Vec<_>>(),
            }
        });
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../../sdk/typescript/src/__fixtures__/mmr_vector.json"
        ))
        .expect("fixture parses");
        assert_eq!(
            vector, expected,
            "regenerate the fixture if hashing changed:\n{vector:#}"
        );
    }
}
