//! Deterministic table-shard planning for high-volume event tables.
//!
//! The first production rollout can create all shards up front with
//! [`create_event_shard_sql`]. Writers choose a shard with [`event_shard`]
//! using tenant and ledger inputs, which keeps a tenant's ledger-adjacent
//! rows clustered while still spreading global write volume.

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardConfig {
    pub shard_count: u16,
}

impl Default for ShardConfig {
    fn default() -> Self {
        Self { shard_count: 32 }
    }
}

pub fn event_shard(tenant_id: &str, ledger_sequence: u32, config: ShardConfig) -> u16 {
    let shard_count = config.shard_count.max(1);
    let mut hasher = Sha256::new();
    hasher.update(tenant_id.as_bytes());
    hasher.update(ledger_sequence.to_be_bytes());
    let digest = hasher.finalize();
    let value = u64::from_be_bytes(digest[..8].try_into().expect("sha256 has 8 bytes"));
    (value % shard_count as u64) as u16
}

pub fn event_table_name(shard: u16) -> String {
    format!("events_{shard:04}")
}

pub fn create_event_shard_sql(shard: u16) -> String {
    let table = event_table_name(shard);
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (LIKE events INCLUDING ALL);\n\
         CREATE INDEX IF NOT EXISTS {table}_tenant_ledger_idx ON {table} (tenant_id, ledger_sequence DESC);"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_selection_is_stable_and_bounded() {
        let config = ShardConfig { shard_count: 16 };
        let shard = event_shard("tenant-a", 123_456, config);
        assert!(shard < 16);
        assert_eq!(shard, event_shard("tenant-a", 123_456, config));
    }

    #[test]
    fn generates_safe_table_names_and_sql() {
        assert_eq!(event_table_name(7), "events_0007");
        let sql = create_event_shard_sql(7);
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS events_0007"));
        assert!(sql.contains("events_0007_tenant_ledger_idx"));
    }
}
