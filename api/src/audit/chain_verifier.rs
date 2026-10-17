//! Sequential hash-chain verifier for the audit trail (Issue #300).
//!
//! Walks a slice of entries in order and checks, for every entry `n`:
//!
//! 1. the sequence number is `n` (1-indexed, no gaps or reordering);
//! 2. `prev_hash` equals the previous entry's `entry_hash` (`GENESIS_HASH`
//!    for the first entry), i.e. `H_n = SHA256(H_{n-1} || LogData_n)`;
//! 3. the stored `entry_hash` equals the hash recomputed from the entry's
//!    canonical fields.
//!
//! The first violation stops the walk and is reported with its sequence, so an
//! auditor learns exactly where the chain was broken.

use serde::Serialize;

use super::{AuditEntry, GENESIS_HASH};

/// Outcome of a full sequential chain walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChainReport {
    /// `true` when every entry passed all three checks.
    pub is_valid: bool,
    /// Number of entries that passed verification before the walk stopped.
    pub verified_entries: usize,
    /// Hash of the last verified entry, or `GENESIS_HASH` for an empty chain.
    pub tip_hash: String,
    /// Sequence number of the first failing entry, if any.
    pub first_broken_sequence: Option<u64>,
    /// Human-readable explanation of the first failure, if any.
    pub reason: Option<String>,
}

/// Verifies `entries` as one contiguous, hash-linked chain starting at genesis.
pub fn verify_chain(entries: &[AuditEntry]) -> ChainReport {
    let mut expected_prev = GENESIS_HASH.to_string();

    for (index, entry) in entries.iter().enumerate() {
        let expected_seq = (index + 1) as u64;

        let failure = if entry.sequence != expected_seq {
            Some(format!(
                "sequence discontinuity: expected {expected_seq}, found {}",
                entry.sequence
            ))
        } else if entry.prev_hash != expected_prev {
            Some(format!(
                "hash-chain broken: expected prev_hash {expected_prev}, found {}",
                entry.prev_hash
            ))
        } else if !entry.verify_internal_hash() {
            Some("entry hash mismatch: stored entry_hash does not match content".to_string())
        } else {
            None
        };

        if let Some(reason) = failure {
            return ChainReport {
                is_valid: false,
                verified_entries: index,
                tip_hash: expected_prev,
                first_broken_sequence: Some(entry.sequence),
                reason: Some(reason),
            };
        }

        expected_prev = entry.entry_hash.clone();
    }

    ChainReport {
        is_valid: true,
        verified_entries: entries.len(),
        tip_hash: expected_prev,
        first_broken_sequence: None,
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    use crate::audit::AuditAction;

    fn build_chain(len: usize) -> Vec<AuditEntry> {
        let timestamp = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let mut chain: Vec<AuditEntry> = Vec::with_capacity(len);
        let mut prev = GENESIS_HASH.to_string();

        for seq in 1..=len as u64 {
            let action = AuditAction::AllowlistRemove {
                address: format!("GADDR{seq}"),
            };
            let payload = serde_json::json!({ "seq": seq });
            let entry_hash = AuditEntry::compute_hash(
                seq,
                &timestamp,
                &action,
                "admin",
                None,
                &payload,
                &prev,
            );
            chain.push(AuditEntry {
                sequence: seq,
                timestamp,
                action,
                actor: "admin".to_string(),
                target: None,
                payload,
                prev_hash: prev.clone(),
                entry_hash: entry_hash.clone(),
            });
            prev = entry_hash;
        }
        chain
    }

    #[test]
    fn empty_chain_is_valid_and_tips_at_genesis() {
        let report = verify_chain(&[]);
        assert!(report.is_valid);
        assert_eq!(report.verified_entries, 0);
        assert_eq!(report.tip_hash, GENESIS_HASH);
        assert!(report.first_broken_sequence.is_none());
    }

    #[test]
    fn intact_chain_verifies_fully() {
        let chain = build_chain(5);
        let report = verify_chain(&chain);
        assert!(report.is_valid);
        assert_eq!(report.verified_entries, 5);
        assert_eq!(report.tip_hash, chain[4].entry_hash);
    }

    #[test]
    fn tampered_payload_is_detected_at_its_sequence() {
        let mut chain = build_chain(4);
        chain[2].payload = serde_json::json!({ "seq": 999 });
        let report = verify_chain(&chain);
        assert!(!report.is_valid);
        assert_eq!(report.first_broken_sequence, Some(3));
        assert_eq!(report.verified_entries, 2);
    }

    #[test]
    fn broken_prev_link_is_detected() {
        let mut chain = build_chain(3);
        chain[1].prev_hash = GENESIS_HASH.to_string();
        let report = verify_chain(&chain);
        assert!(!report.is_valid);
        assert_eq!(report.first_broken_sequence, Some(2));
        assert!(report.reason.unwrap().contains("hash-chain broken"));
    }

    #[test]
    fn sequence_gap_is_detected() {
        let mut chain = build_chain(3);
        chain.remove(1);
        let report = verify_chain(&chain);
        assert!(!report.is_valid);
        assert_eq!(report.first_broken_sequence, Some(3));
        assert!(report.reason.unwrap().contains("sequence discontinuity"));
    }
}
