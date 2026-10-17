//! Fork detection over a rolling window of recent ledger hashes.
//!
//! Each observed `(sequence, hash)` is compared with the window. A different
//! hash for a ledger already seen, or a lower sequence than the tip, means the
//! chain was reorganized (or RPC endpoints disagree); the detector then drops
//! the diverged ledgers and reports the common ancestor so the indexer can
//! resume from there.

use std::collections::VecDeque;

/// Number of recent ledgers retained.
pub const WINDOW_DEPTH: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub enum Observation {
    /// The ledger extends or repeats the known chain.
    Consistent,
    /// The chain diverged; re-index from `common_ancestor + 1`.
    /// `None` means no ledger in the window is shared.
    Fork { common_ancestor: Option<u32> },
}

/// True when RPC endpoints report different hashes for the same ledger.
pub fn endpoints_disagree(reports: &[(u32, &str)]) -> bool {
    reports
        .iter()
        .any(|(seq, hash)| reports.iter().any(|(s, h)| s == seq && h != hash))
}

#[derive(Default)]
pub struct ForkDetector {
    window: VecDeque<(u32, String)>,
}

impl ForkDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a ledger hash as reported by an RPC endpoint.
    pub fn observe(&mut self, sequence: u32, hash: &str) -> Observation {
        if let Some((_, known)) = self.window.iter().find(|(seq, _)| *seq == sequence) {
            if known == hash {
                return Observation::Consistent;
            }
            return self.rewind(sequence, hash);
        }
        if self.window.back().is_some_and(|(tip, _)| sequence < *tip) {
            // Older than the tip but unseen: the tip moved backwards.
            return self.rewind(sequence, hash);
        }
        self.window.push_back((sequence, hash.to_string()));
        while self.window.len() > WINDOW_DEPTH {
            self.window.pop_front();
        }
        Observation::Consistent
    }

    /// Drop every ledger at or above `sequence`, keep `hash` as the new
    /// entry, and report the highest ledger still shared with the old chain.
    fn rewind(&mut self, sequence: u32, hash: &str) -> Observation {
        self.window.retain(|(seq, _)| *seq < sequence);
        let common_ancestor = self.window.back().map(|(seq, _)| *seq);
        self.window.push_back((sequence, hash.to_string()));
        Observation::Fork { common_ancestor }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(detector: &mut ForkDetector, from: u32, to: u32) {
        for seq in from..=to {
            assert_eq!(
                detector.observe(seq, &format!("h{seq}")),
                Observation::Consistent
            );
        }
    }

    #[test]
    fn extending_and_repeating_the_chain_is_consistent() {
        let mut detector = ForkDetector::new();
        fill(&mut detector, 100, 105);
        assert_eq!(detector.observe(105, "h105"), Observation::Consistent);
    }

    #[test]
    fn endpoints_reporting_different_hashes_disagree() {
        assert!(!endpoints_disagree(&[(10, "a"), (10, "a"), (11, "b")]));
        assert!(endpoints_disagree(&[(10, "a"), (10, "b")]));
    }

    #[test]
    fn window_keeps_only_the_latest_ledgers() {
        let mut detector = ForkDetector::new();
        fill(&mut detector, 1, 100);
        assert_eq!(detector.window.len(), WINDOW_DEPTH);
        assert_eq!(detector.window.front().map(|(seq, _)| *seq), Some(69));
    }

    #[test]
    fn changed_hash_reports_common_ancestor() {
        let mut detector = ForkDetector::new();
        fill(&mut detector, 100, 105);
        assert_eq!(
            detector.observe(103, "other"),
            Observation::Fork {
                common_ancestor: Some(102)
            }
        );
        // Ledgers above the fork point were discarded.
        assert_eq!(detector.window.back(), Some(&(103, "other".to_string())));
    }

    #[test]
    fn fork_older_than_the_window_has_no_common_ancestor() {
        let mut detector = ForkDetector::new();
        fill(&mut detector, 1, 100);
        assert_eq!(
            detector.observe(69, "other"),
            Observation::Fork {
                common_ancestor: None
            }
        );
    }
}
