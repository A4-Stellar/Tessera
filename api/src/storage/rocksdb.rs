//! Local RocksDB index for fast cold starts and atomic ledger snapshots.

use rocksdb::{ColumnFamilyDescriptor, IteratorMode, Options, WriteBatch, DB};
use std::{path::Path, sync::Arc};

pub const LEDGER_STATE: &str = "ledger_state";
pub const ASSET_METADATA: &str = "asset_metadata";
pub const HOLDER_BALANCES: &str = "holder_balances";
/// Append-only snapshots written when the shadow indexer diverges from the
/// primary snapshot, kept for post-incident analysis.
pub const FORENSICS: &str = "forensics";
/// Persistent key for the append-only audit MMR peak snapshot.
pub const AUDIT_MMR_PEAKS_KEY: &str = "mmr:peaks:v1";
/// Persistent key for the full audit MMR snapshot used to restore proof state.
pub const AUDIT_MMR_STATE_KEY: &str = "mmr:state:v1";
/// Prefix for persisted audit entries needed to serve proofs after restart.
pub const AUDIT_ENTRY_PREFIX: &str = "audit:entry:v1:";

#[derive(Clone)]
pub struct RocksLedger {
    db: Arc<DB>,
}

impl RocksLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, rocksdb::Error> {
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let families = [LEDGER_STATE, ASSET_METADATA, HOLDER_BALANCES, FORENSICS]
            .into_iter()
            .map(|name| ColumnFamilyDescriptor::new(name, Options::default()));
        Ok(Self { db: Arc::new(DB::open_cf_descriptors(&options, path, families)?) })
    }

    pub fn get_holder_balance(&self, key: &[u8]) -> Result<Option<Vec<u8>>, rocksdb::Error> {
        self.get(HOLDER_BALANCES, key)
    }

    /// All values in a column family, ordered by key. Used by the shadow
    /// forensics reader to enumerate captured divergence snapshots.
    pub fn scan_family(&self, family: &str) -> Result<Vec<Vec<u8>>, rocksdb::Error> {
        let handle = self.db.cf_handle(family).expect("configured column family");
        self.db
            .iterator_cf(handle, IteratorMode::Start)
            .map(|item| item.map(|(_, value)| value.to_vec()))
            .collect()
    }

    pub fn get(&self, family: &str, key: &[u8]) -> Result<Option<Vec<u8>>, rocksdb::Error> {
        let handle = self.db.cf_handle(family).expect("configured column family");
        self.db.get_cf(handle, key)
    }

    pub fn put(&self, family: &str, key: &[u8], value: &[u8]) -> Result<(), rocksdb::Error> {
        let handle = self.db.cf_handle(family).expect("configured column family");
        self.db.put_cf(handle, key, value)
    }

    pub fn put_audit_mmr_peaks(&self, peaks: &[[u8; 32]]) -> Result<(), rocksdb::Error> {
        let mut encoded = Vec::with_capacity(peaks.len() * 32);
        for peak in peaks {
            encoded.extend_from_slice(peak);
        }
        self.put(LEDGER_STATE, AUDIT_MMR_PEAKS_KEY.as_bytes(), &encoded)
    }

    pub fn get_audit_mmr_peaks(&self) -> Result<Option<Vec<[u8; 32]>>, rocksdb::Error> {
        let Some(encoded) = self.get(LEDGER_STATE, AUDIT_MMR_PEAKS_KEY.as_bytes())? else {
            return Ok(None);
        };
        if encoded.len() % 32 != 0 {
            return Ok(None);
        }
        Ok(Some(
            encoded
                .chunks_exact(32)
                .map(|chunk| {
                    let mut hash = [0u8; 32];
                    hash.copy_from_slice(chunk);
                    hash
                })
                .collect(),
        ))
    }

    pub fn put_audit_mmr_state(&self, state_json: &[u8]) -> Result<(), rocksdb::Error> {
        self.put(LEDGER_STATE, AUDIT_MMR_STATE_KEY.as_bytes(), state_json)
    }

    pub fn get_audit_mmr_state(&self) -> Result<Option<Vec<u8>>, rocksdb::Error> {
        self.get(LEDGER_STATE, AUDIT_MMR_STATE_KEY.as_bytes())
    }

    pub fn get_audit_entries(&self) -> Result<Vec<Vec<u8>>, rocksdb::Error> {
        let handle = self.db.cf_handle(LEDGER_STATE).expect("configured column family");
        let mut entries = Vec::new();
        for item in self.db.iterator_cf(handle, IteratorMode::Start) {
            let (key, value) = item?;
            if key.starts_with(AUDIT_ENTRY_PREFIX.as_bytes()) {
                entries.push((key.to_vec(), value.to_vec()));
            }
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries.into_iter().map(|(_, value)| value).collect())
    }

    pub fn write_audit_mmr_snapshot(
        &self,
        peaks: &[[u8; 32]],
        state_json: &[u8],
        sequence: u64,
        entry_json: &[u8],
    ) -> Result<(), rocksdb::Error> {
        let handle = self.db.cf_handle(LEDGER_STATE).expect("configured column family");
        let mut encoded_peaks = Vec::with_capacity(peaks.len() * 32);
        for peak in peaks {
            encoded_peaks.extend_from_slice(peak);
        }
        let mut batch = WriteBatch::default();
        batch.put_cf(handle, AUDIT_MMR_PEAKS_KEY.as_bytes(), encoded_peaks);
        batch.put_cf(handle, AUDIT_MMR_STATE_KEY.as_bytes(), state_json);
        let entry_key = format!("{AUDIT_ENTRY_PREFIX}{sequence:020}");
        batch.put_cf(handle, entry_key.as_bytes(), entry_json);
        self.db.write(batch)
    }

    pub fn write_ledger_batch<'a, I>(&self, entries: I) -> Result<(), rocksdb::Error>
    where
        I: IntoIterator<Item = (&'a str, &'a [u8], &'a [u8])>,
    {
        let mut batch = WriteBatch::default();
        for (family, key, value) in entries {
            let handle = self.db.cf_handle(family).expect("configured column family");
            batch.put_cf(handle, key, value);
        }
        self.db.write(batch)
    }
}