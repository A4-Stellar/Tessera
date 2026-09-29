//! Local RocksDB index for fast cold starts and atomic ledger snapshots.

use rocksdb::{ColumnFamilyDescriptor, Options, WriteBatch, DB};
use std::{path::Path, sync::Arc};

pub const LEDGER_STATE: &str = "ledger_state";
pub const ASSET_METADATA: &str = "asset_metadata";
pub const HOLDER_BALANCES: &str = "holder_balances";

#[derive(Clone)]
pub struct RocksLedger {
    db: Arc<DB>,
}

impl RocksLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, rocksdb::Error> {
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let families = [LEDGER_STATE, ASSET_METADATA, HOLDER_BALANCES]
            .into_iter()
            .map(|name| ColumnFamilyDescriptor::new(name, Options::default()));
        Ok(Self { db: Arc::new(DB::open_cf_descriptors(&options, path, families)?) })
    }

    pub fn get_holder_balance(&self, key: &[u8]) -> Result<Option<Vec<u8>>, rocksdb::Error> {
        self.get(HOLDER_BALANCES, key)
    }

    pub fn get(&self, family: &str, key: &[u8]) -> Result<Option<Vec<u8>>, rocksdb::Error> {
        let handle = self.db.cf_handle(family).expect("configured column family");
        self.db.get_cf(handle, key)
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