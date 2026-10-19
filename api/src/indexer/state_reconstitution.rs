//! Historical Soroban contract-state reconstruction from ordered ledger diffs.
//!
//! Feed adapters provide the decoded key/value changes and each ledger's
//! state commitment. The commitment hasher is injectable so an adapter can use
//! the network's exact ledger-state commitment format rather than substituting
//! a transaction or ledger-header hash.

use std::collections::BTreeMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};

pub const SNAPSHOT_INTERVAL: u32 = 10_000;

pub type StateEntries = BTreeMap<Vec<u8>, Vec<u8>>;
pub type StateHash = [u8; 32];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateChange {
    pub key: Vec<u8>,
    pub old_value: Option<Vec<u8>>,
    pub new_value: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstitutedState {
    pub ledger: u32,
    pub entries: StateEntries,
    pub state_hash: StateHash,
}

pub trait StateCommitmentHasher: Send + Sync {
    fn hash_state(&self, entries: &StateEntries) -> StateHash;
}

/// Deterministic digest for feeds whose commitment is SHA-256 over sorted,
/// length-prefixed key/value pairs. Use a protocol-specific hasher otherwise.
#[derive(Debug, Default)]
pub struct CanonicalSha256Hasher;

impl StateCommitmentHasher for CanonicalSha256Hasher {
    fn hash_state(&self, entries: &StateEntries) -> StateHash {
        let mut hasher = Sha256::new();
        for (key, value) in entries {
            hasher.update((key.len() as u64).to_be_bytes());
            hasher.update(key);
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value);
        }
        hasher.finalize().into()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReconstructionError {
    #[error("ledger sequence must be greater than zero")]
    InvalidBaseLedger,
    #[error("cannot append a ledger after the sequence limit")]
    LedgerSequenceExhausted,
    #[error("diff ledger {actual} is not contiguous after ledger {expected}")]
    NonContiguous { expected: u32, actual: u32 },
    #[error("state change precondition failed at ledger {ledger} for key {key:?}")]
    ChangeMismatch { ledger: u32, key: Vec<u8> },
    #[error("state commitment mismatch at ledger {ledger}")]
    CommitmentMismatch { ledger: u32 },
    #[error("ledger {ledger} is outside the retained state history")]
    LedgerUnavailable { ledger: u32 },
}

/// Retains periodic full snapshots and the ordered forward/backward deltas
/// needed to reconstruct any ledger from the base through the latest feed.
pub struct StateReconstructor {
    base_ledger: u32,
    latest_ledger: u32,
    latest_entries: StateEntries,
    snapshots: BTreeMap<u32, StateEntries>,
    changes: BTreeMap<u32, Vec<StateChange>>,
    commitments: BTreeMap<u32, StateHash>,
    hasher: Arc<dyn StateCommitmentHasher>,
}

impl StateReconstructor {
    pub fn new(
        base_ledger: u32,
        base_entries: StateEntries,
        base_commitment: StateHash,
        hasher: Arc<dyn StateCommitmentHasher>,
    ) -> Result<Self, ReconstructionError> {
        if base_ledger == 0 {
            return Err(ReconstructionError::InvalidBaseLedger);
        }
        if hasher.hash_state(&base_entries) != base_commitment {
            return Err(ReconstructionError::CommitmentMismatch {
                ledger: base_ledger,
            });
        }
        Ok(Self {
            base_ledger,
            latest_ledger: base_ledger,
            latest_entries: base_entries.clone(),
            snapshots: BTreeMap::from([(base_ledger, base_entries)]),
            changes: BTreeMap::new(),
            commitments: BTreeMap::from([(base_ledger, base_commitment)]),
            hasher,
        })
    }

    pub fn with_canonical_sha256(
        base_ledger: u32,
        base_entries: StateEntries,
        base_commitment: StateHash,
    ) -> Result<Self, ReconstructionError> {
        Self::new(
            base_ledger,
            base_entries,
            base_commitment,
            Arc::new(CanonicalSha256Hasher),
        )
    }

    pub fn latest_ledger(&self) -> u32 {
        self.latest_ledger
    }

    /// Applies one ledger's changes atomically and checks its committed state
    /// hash before recording the feed or creating a periodic snapshot.
    pub fn apply_ledger(
        &mut self,
        ledger: u32,
        changes: Vec<StateChange>,
        commitment: StateHash,
    ) -> Result<(), ReconstructionError> {
        let expected = self
            .latest_ledger
            .checked_add(1)
            .ok_or(ReconstructionError::LedgerSequenceExhausted)?;
        if ledger != expected {
            return Err(ReconstructionError::NonContiguous {
                expected,
                actual: ledger,
            });
        }

        for (index, change) in changes.iter().enumerate() {
            if let Err(error) = apply_change(&mut self.latest_entries, change, true, ledger) {
                for applied in changes[..index].iter().rev() {
                    restore_previous(&mut self.latest_entries, applied);
                }
                return Err(error);
            }
        }
        if self.hasher.hash_state(&self.latest_entries) != commitment {
            for change in changes.iter().rev() {
                restore_previous(&mut self.latest_entries, change);
            }
            return Err(ReconstructionError::CommitmentMismatch { ledger });
        }

        self.changes.insert(ledger, changes);
        self.commitments.insert(ledger, commitment);
        self.latest_ledger = ledger;
        if ledger % SNAPSHOT_INTERVAL == 0 {
            self.snapshots.insert(ledger, self.latest_entries.clone());
        }
        Ok(())
    }

    /// Reconstructs from the nearest retained checkpoint in either direction.
    pub fn reconstitute(
        &self,
        ledger: u32,
    ) -> Result<ReconstitutedState, ReconstructionError> {
        if ledger < self.base_ledger || ledger > self.latest_ledger {
            return Err(ReconstructionError::LedgerUnavailable { ledger });
        }
        let previous = self.snapshots.range(..=ledger).next_back();
        let next = self.snapshots.range(ledger..).next();
        let (&checkpoint_ledger, checkpoint) = match (previous, next) {
            (Some(previous), Some(next)) => {
                if ledger - *previous.0 <= *next.0 - ledger {
                    previous
                } else {
                    next
                }
            }
            (Some(previous), None) => previous,
            (None, Some(next)) => next,
            (None, None) => return Err(ReconstructionError::LedgerUnavailable { ledger }),
        };

        let mut entries = checkpoint.clone();
        if checkpoint_ledger < ledger {
            for sequence in checkpoint_ledger + 1..=ledger {
                let changes = self
                    .changes
                    .get(&sequence)
                    .ok_or(ReconstructionError::LedgerUnavailable { ledger: sequence })?;
                for change in changes {
                    apply_change(&mut entries, change, true, sequence)?;
                }
            }
        } else if checkpoint_ledger > ledger {
            for sequence in (ledger + 1..=checkpoint_ledger).rev() {
                let changes = self
                    .changes
                    .get(&sequence)
                    .ok_or(ReconstructionError::LedgerUnavailable { ledger: sequence })?;
                for change in changes.iter().rev() {
                    apply_change(&mut entries, change, false, sequence)?;
                }
            }
        }

        let state_hash = self.hasher.hash_state(&entries);
        let expected = self
            .commitments
            .get(&ledger)
            .ok_or(ReconstructionError::LedgerUnavailable { ledger })?;
        if &state_hash != expected {
            return Err(ReconstructionError::CommitmentMismatch { ledger });
        }
        Ok(ReconstitutedState {
            ledger,
            entries,
            state_hash,
        })
    }
}

fn restore_previous(entries: &mut StateEntries, change: &StateChange) {
    match &change.old_value {
        Some(value) => {
            entries.insert(change.key.clone(), value.clone());
        }
        None => {
            entries.remove(&change.key);
        }
    }
}

fn apply_change(
    entries: &mut StateEntries,
    change: &StateChange,
    forward: bool,
    ledger: u32,
) -> Result<(), ReconstructionError> {
    let (expected, replacement) = if forward {
        (&change.old_value, &change.new_value)
    } else {
        (&change.new_value, &change.old_value)
    };
    if entries.get(&change.key) != expected.as_ref() {
        return Err(ReconstructionError::ChangeMismatch {
            ledger,
            key: change.key.clone(),
        });
    }
    match replacement {
        Some(value) => {
            entries.insert(change.key.clone(), value.clone());
        }
        None => {
            entries.remove(&change.key);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(entries: &StateEntries) -> StateHash {
        CanonicalSha256Hasher.hash_state(entries)
    }

    fn change(key: &[u8], old: Option<&[u8]>, new: Option<&[u8]>) -> StateChange {
        StateChange {
            key: key.to_vec(),
            old_value: old.map(<[u8]>::to_vec),
            new_value: new.map(<[u8]>::to_vec),
        }
    }

    fn state() -> StateEntries {
        BTreeMap::from([(b"a".to_vec(), b"one".to_vec())])
    }

    #[test]
    fn reconstructs_forward_and_backward_including_deletes() {
        let base = state();
        let mut reconstructor =
            StateReconstructor::with_canonical_sha256(1, base.clone(), digest(&base)).unwrap();
        let mut second = base.clone();
        second.insert(b"a".to_vec(), b"two".to_vec());
        second.insert(b"b".to_vec(), b"new".to_vec());
        reconstructor
            .apply_ledger(
                2,
                vec![
                    change(b"a", Some(b"one"), Some(b"two")),
                    change(b"b", None, Some(b"new")),
                ],
                digest(&second),
            )
            .unwrap();
        let mut third = second.clone();
        third.remove(&b"a".to_vec());
        reconstructor
            .apply_ledger(3, vec![change(b"a", Some(b"two"), None)], digest(&third))
            .unwrap();

        assert_eq!(reconstructor.reconstitute(1).unwrap().entries, base);
        assert_eq!(reconstructor.reconstitute(2).unwrap().entries, second);
        assert_eq!(reconstructor.reconstitute(3).unwrap().entries, third);
    }

    #[test]
    fn bad_commitment_or_change_does_not_advance_state() {
        let base = state();
        let mut reconstructor =
            StateReconstructor::with_canonical_sha256(1, base.clone(), digest(&base)).unwrap();
        assert!(matches!(
            reconstructor.apply_ledger(
                2,
                vec![
                    change(b"a", Some(b"one"), Some(b"two")),
                    change(b"b", Some(b"wrong"), Some(b"new")),
                ],
                digest(&base)
            ),
            Err(ReconstructionError::ChangeMismatch { ledger: 2, key }) if key == b"b".to_vec()
        ));
        assert!(matches!(
            reconstructor.apply_ledger(
                2,
                vec![change(b"a", Some(b"one"), Some(b"two"))],
                [0; 32]
            ),
            Err(ReconstructionError::CommitmentMismatch { ledger: 2 })
        ));
        assert_eq!(reconstructor.latest_ledger(), 1);
        assert_eq!(reconstructor.reconstitute(1).unwrap().entries, base);
    }

    #[test]
    fn creates_checkpoints_at_ten_thousand_ledger_boundaries() {
        let base = state();
        let mut reconstructor =
            StateReconstructor::with_canonical_sha256(1, base.clone(), digest(&base)).unwrap();
        let mut entries = base;
        for ledger in 2..=SNAPSHOT_INTERVAL {
            reconstructor
                .apply_ledger(ledger, Vec::new(), digest(&entries))
                .unwrap();
        }
        assert_eq!(reconstructor.snapshots.len(), 2);
        assert_eq!(reconstructor.reconstitute(9_999).unwrap().entries, entries);
        assert_eq!(reconstructor.reconstitute(10_000).unwrap().entries, entries);
    }

    #[test]
    fn rejects_non_contiguous_feeds() {
        let base = state();
        let mut reconstructor =
            StateReconstructor::with_canonical_sha256(1, base.clone(), digest(&base)).unwrap();
        assert_eq!(
            reconstructor.apply_ledger(3, Vec::new(), digest(&base)),
            Err(ReconstructionError::NonContiguous {
                expected: 2,
                actual: 3,
            })
        );
    }
}
