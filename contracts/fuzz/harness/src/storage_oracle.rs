//! Mock host-function layer that records Soroban storage access.
//!
//! The WASM-level fuzzer runs contracts inside a host whose `storage().get()`
//! and `storage().set()` calls are mirrored into this oracle. A read of a key
//! that was never written is a panic-equivalent in production (contracts call
//! `.unwrap()` on such reads), so the oracle turns it into a fuzz finding
//! before the host gets a chance to trap.

use std::collections::{HashSet, VecDeque};
use std::fmt;

/// A storage key, qualified by the contract that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorageKey {
    pub contract: String,
    pub key: Vec<u8>,
}

impl StorageKey {
    pub fn new(contract: &str, key: &[u8]) -> Self {
        Self {
            contract: contract.to_owned(),
            key: key.to_vec(),
        }
    }
}

impl fmt::Display for StorageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.contract, hex_encode(&self.key))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// A finding: a key was read before any contract wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsetRead {
    pub key: StorageKey,
    /// Index in the access log, useful for shrinking a crashing input.
    pub access_index: usize,
}

/// The mock host's storage surface.
pub trait MockHost {
    fn storage_set(&mut self, contract: &str, key: &[u8], value: &[u8]);
    /// Returns `true` when the key exists, `false` otherwise.
    fn storage_get(&mut self, contract: &str, key: &[u8]) -> bool;
}

/// Records every `set`/`get` and detects reads of unset keys.
#[derive(Debug, Default)]
pub struct StorageOracle {
    written: HashSet<StorageKey>,
    /// `(was_a_write, key)` in call order; useful for shrinking a crash.
    log: VecDeque<(bool, StorageKey)>,
    unset_reads: Vec<UnsetRead>,
}

impl StorageOracle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, contract: &str, key: &[u8], value: &[u8]) {
        let key = StorageKey::new(contract, key);
        // An empty value is a legitimate "tombstone" write in some contracts;
        // it still counts as initialised for the no-unset-read invariant.
        let _ = value;
        self.written.insert(key.clone());
        self.log.push_back((true, key));
    }

    pub fn get(&mut self, contract: &str, key: &[u8]) -> bool {
        let key = StorageKey::new(contract, key);
        let exists = self.written.contains(&key);
        if !exists {
            self.unset_reads.push(UnsetRead {
                key: key.clone(),
                access_index: self.log.len(),
            });
        }
        self.log.push_back((false, key));
        exists
    }

    pub fn unset_reads(&self) -> &[UnsetRead] {
        &self.unset_reads
    }

    pub fn access_count(&self) -> usize {
        self.log.len()
    }

    pub fn written_keys(&self) -> usize {
        self.written.len()
    }

    /// Fail the fuzz case if the contract read an unwritten key.
    pub fn assert_no_unset_reads(&self) {
        assert!(
            self.unset_reads.is_empty(),
            "contract read {} unset keys: {:?}",
            self.unset_reads.len(),
            self.unset_reads
        );
    }
}

impl MockHost for StorageOracle {
    fn storage_set(&mut self, contract: &str, key: &[u8], value: &[u8]) {
        self.set(contract, key, value);
    }

    fn storage_get(&mut self, contract: &str, key: &[u8]) -> bool {
        self.get(contract, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_read_before_write() {
        let mut oracle = StorageOracle::new();
        assert!(!oracle.get("asset", b"total_supply"));
        oracle.assert_no_unset_reads_should_fail();
    }

    #[test]
    fn accepts_reads_after_writes_and_scopes_by_contract() {
        let mut oracle = StorageOracle::new();
        oracle.set("asset", b"balance", &[1, 2, 3]);
        assert!(oracle.get("asset", b"balance"));
        assert!(!oracle.get("dividend", b"balance"));
        assert_eq!(oracle.written_keys(), 1);
        assert_eq!(oracle.access_count(), 3);
        assert_eq!(oracle.unset_reads().len(), 1);
    }

    #[test]
    fn mock_host_trait_is_usable() {
        let mut host = StorageOracle::default();
        host.storage_set("c", b"k", b"v");
        assert!(host.storage_get("c", b"k"));
    }

    impl StorageOracle {
        fn assert_no_unset_reads_should_fail(&self) {
            assert!(!self.unset_reads().is_empty());
        }
    }
}
