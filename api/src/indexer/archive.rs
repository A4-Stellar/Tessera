//! Cold-storage archive planning for historical ledger rows.
//!
//! The API keeps recent indexer rows hot in Postgres, but full historical
//! ledgers eventually need to move to object storage in bounded chunks. This
//! module is deliberately storage-provider neutral: callers can use the plan
//! to stream rows to S3/GCS/R2 and then mark `end_ledger` as archived only
//! after the object manifest is committed.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveConfig {
    /// Ledgers newer than `current_ledger - hot_retention_ledgers` remain hot.
    pub hot_retention_ledgers: u32,
    /// Maximum ledger span per archive object.
    pub batch_ledgers: u32,
    /// Safety cap per worker tick.
    pub max_batches: u32,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            hot_retention_ledgers: 172_800,
            batch_ledgers: 4_096,
            max_batches: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveBatch {
    pub start_ledger: u32,
    pub end_ledger: u32,
    pub object_key: String,
}

pub fn plan_archive_batches(
    network: &str,
    current_ledger: u32,
    archived_through: u32,
    config: ArchiveConfig,
) -> Vec<ArchiveBatch> {
    if config.batch_ledgers == 0 || config.max_batches == 0 {
        return Vec::new();
    }

    let cold_through = current_ledger.saturating_sub(config.hot_retention_ledgers);
    if cold_through <= archived_through {
        return Vec::new();
    }

    let mut batches = Vec::new();
    let mut start = archived_through.saturating_add(1);
    while start <= cold_through && (batches.len() as u32) < config.max_batches {
        let end = start
            .saturating_add(config.batch_ledgers.saturating_sub(1))
            .min(cold_through);
        batches.push(ArchiveBatch {
            start_ledger: start,
            end_ledger: end,
            object_key: archive_object_key(network, start, end),
        });
        start = end.saturating_add(1);
    }
    batches
}

pub fn archive_object_key(network: &str, start_ledger: u32, end_ledger: u32) -> String {
    let network = network
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_ascii_lowercase();
    format!("ledger-archive/{network}/{start_ledger:010}-{end_ledger:010}.jsonl.zst")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_recent_ledgers_hot() {
        let batches = plan_archive_batches(
            "testnet",
            10_000,
            0,
            ArchiveConfig {
                hot_retention_ledgers: 10_000,
                batch_ledgers: 100,
                max_batches: 10,
            },
        );
        assert!(batches.is_empty());
    }

    #[test]
    fn plans_bounded_contiguous_batches() {
        let batches = plan_archive_batches(
            "Test SDF Network",
            1_000,
            100,
            ArchiveConfig {
                hot_retention_ledgers: 300,
                batch_ledgers: 250,
                max_batches: 2,
            },
        );
        assert_eq!(
            batches,
            vec![
                ArchiveBatch {
                    start_ledger: 101,
                    end_ledger: 350,
                    object_key: "ledger-archive/test-sdf-network/0000000101-0000000350.jsonl.zst"
                        .to_string(),
                },
                ArchiveBatch {
                    start_ledger: 351,
                    end_ledger: 600,
                    object_key: "ledger-archive/test-sdf-network/0000000351-0000000600.jsonl.zst"
                        .to_string(),
                },
            ]
        );
    }
}
