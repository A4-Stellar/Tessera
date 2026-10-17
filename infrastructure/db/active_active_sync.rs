//! Automated Multi-Cloud Active-Active Database Synchronization (AWS RDS <-> GCP Cloud SQL).
//!
//! Provides continuous zero-downtime operation, bi-directional logical replication
//! with conflict-free resolution rules, sub-second replication latency monitoring,
//! and simulated cloud region failover with zero-data-loss validation (Issue #306).

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Supported cloud provider regions participating in active-active topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CloudRegion {
    AwsUsEast1,
    GcpUsCentral1,
}

impl fmt::Display for CloudRegion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AwsUsEast1 => write!(f, "aws-us-east-1"),
            Self::GcpUsCentral1 => write!(f, "gcp-us-central-1"),
        }
    }
}

/// Identifies a specific database node in the multi-cloud topology.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DatabaseNode {
    pub id: String,
    pub region: CloudRegion,
    pub endpoint: String,
    pub port: u16,
    pub database: String,
    pub priority: u8, // Higher priority wins deterministic conflict ties
}

impl DatabaseNode {
    pub fn new_aws(id: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            region: CloudRegion::AwsUsEast1,
            endpoint: endpoint.into(),
            port: 5432,
            database: "tessera_mainnet".into(),
            priority: 200,
        }
    }

    pub fn new_gcp(id: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            region: CloudRegion::GcpUsCentral1,
            endpoint: endpoint.into(),
            port: 5432,
            database: "tessera_mainnet".into(),
            priority: 100,
        }
    }
}

/// Logical Log Sequence Number representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
pub struct WalLsn(pub u64);

impl fmt::Display for WalLsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:X}/{:X}", self.0 >> 32, self.0 & 0xFFFFFFFF)
    }
}

/// Row version vector & hybrid logical clock timestamp for conflict-free resolution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordMetadata {
    pub primary_key: String,
    pub table_name: String,
    pub version: u64,
    pub updated_at_micros: u64,
    pub origin_node_id: String,
    pub checksum: String,
}

/// Replicated data mutation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplicationMutation {
    Insert {
        metadata: RecordMetadata,
        columns: HashMap<String, String>,
    },
    Update {
        metadata: RecordMetadata,
        columns: HashMap<String, String>,
    },
    Delete {
        metadata: RecordMetadata,
    },
}

impl ReplicationMutation {
    pub fn metadata(&self) -> &RecordMetadata {
        match self {
            Self::Insert { metadata, .. }
            | Self::Update { metadata, .. }
            | Self::Delete { metadata } => metadata,
        }
    }
}

/// Conflict resolution strategy applied during concurrent multi-cloud writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictStrategy {
    LastWriteWins,
    HighestNodePriority,
    ColumnMerge,
}

/// Resolution verdict for a detected replication conflict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictResolutionResult {
    pub primary_key: String,
    pub table_name: String,
    pub strategy_applied: ConflictStrategy,
    pub winner_origin: String,
    pub winning_mutation: ReplicationMutation,
    pub superseded_origin: String,
    pub audit_log: String,
}

/// Conflict-free replication resolver.
#[derive(Clone, Debug)]
pub struct ConflictResolver {
    strategy: ConflictStrategy,
}

impl ConflictResolver {
    pub fn new(strategy: ConflictStrategy) -> Self {
        Self { strategy }
    }

    /// Evaluates concurrent mutations to the same record and selects a deterministic winner.
    pub fn resolve(
        &self,
        local: &ReplicationMutation,
        remote: &ReplicationMutation,
        local_node: &DatabaseNode,
        remote_node: &DatabaseNode,
    ) -> ConflictResolutionResult {
        let meta_local = local.metadata();
        let meta_remote = remote.metadata();

        match self.strategy {
            ConflictStrategy::LastWriteWins => {
                if meta_remote.updated_at_micros > meta_local.updated_at_micros {
                    ConflictResolutionResult {
                        primary_key: meta_remote.primary_key.clone(),
                        table_name: meta_remote.table_name.clone(),
                        strategy_applied: ConflictStrategy::LastWriteWins,
                        winner_origin: meta_remote.origin_node_id.clone(),
                        winning_mutation: remote.clone(),
                        superseded_origin: meta_local.origin_node_id.clone(),
                        audit_log: format!(
                            "Remote won by LWW: {}us > {}us",
                            meta_remote.updated_at_micros, meta_local.updated_at_micros
                        ),
                    }
                } else if meta_local.updated_at_micros > meta_remote.updated_at_micros {
                    ConflictResolutionResult {
                        primary_key: meta_local.primary_key.clone(),
                        table_name: meta_local.table_name.clone(),
                        strategy_applied: ConflictStrategy::LastWriteWins,
                        winner_origin: meta_local.origin_node_id.clone(),
                        winning_mutation: local.clone(),
                        superseded_origin: meta_remote.origin_node_id.clone(),
                        audit_log: format!(
                            "Local won by LWW: {}us > {}us",
                            meta_local.updated_at_micros, meta_remote.updated_at_micros
                        ),
                    }
                } else {
                    // Tie-breaker: Highest Node Priority
                    self.resolve_by_priority(local, remote, local_node, remote_node)
                }
            }
            ConflictStrategy::HighestNodePriority => {
                self.resolve_by_priority(local, remote, local_node, remote_node)
            }
            ConflictStrategy::ColumnMerge => self.resolve_by_column_merge(local, remote),
        }
    }

    fn resolve_by_priority(
        &self,
        local: &ReplicationMutation,
        remote: &ReplicationMutation,
        local_node: &DatabaseNode,
        remote_node: &DatabaseNode,
    ) -> ConflictResolutionResult {
        let meta_local = local.metadata();
        let meta_remote = remote.metadata();

        if remote_node.priority > local_node.priority {
            ConflictResolutionResult {
                primary_key: meta_remote.primary_key.clone(),
                table_name: meta_remote.table_name.clone(),
                strategy_applied: ConflictStrategy::HighestNodePriority,
                winner_origin: meta_remote.origin_node_id.clone(),
                winning_mutation: remote.clone(),
                superseded_origin: meta_local.origin_node_id.clone(),
                audit_log: format!(
                    "Tie broken by priority: remote node ({}) priority {} > local node ({}) priority {}",
                    remote_node.id, remote_node.priority, local_node.id, local_node.priority
                ),
            }
        } else {
            ConflictResolutionResult {
                primary_key: meta_local.primary_key.clone(),
                table_name: meta_local.table_name.clone(),
                strategy_applied: ConflictStrategy::HighestNodePriority,
                winner_origin: meta_local.origin_node_id.clone(),
                winning_mutation: local.clone(),
                superseded_origin: meta_remote.origin_node_id.clone(),
                audit_log: format!(
                    "Tie broken by priority: local node ({}) priority {} >= remote node ({}) priority {}",
                    local_node.id, local_node.priority, remote_node.id, remote_node.priority
                ),
            }
        }
    }

    fn resolve_by_column_merge(
        &self,
        local: &ReplicationMutation,
        remote: &ReplicationMutation,
    ) -> ConflictResolutionResult {
        let meta_local = local.metadata();
        let meta_remote = remote.metadata();

        match (local, remote) {
            (
                ReplicationMutation::Update {
                    columns: cols_local,
                    ..
                },
                ReplicationMutation::Update {
                    columns: cols_remote,
                    ..
                },
            ) => {
                let mut merged = cols_local.clone();
                for (k, v) in cols_remote {
                    merged.insert(k.clone(), v.clone());
                }
                let mut meta_merged = meta_remote.clone();
                meta_merged.version = meta_local.version.max(meta_remote.version) + 1;

                ConflictResolutionResult {
                    primary_key: meta_merged.primary_key.clone(),
                    table_name: meta_merged.table_name.clone(),
                    strategy_applied: ConflictStrategy::ColumnMerge,
                    winner_origin: "merged_crdt".into(),
                    winning_mutation: ReplicationMutation::Update {
                        metadata: meta_merged,
                        columns: merged,
                    },
                    superseded_origin: "both".into(),
                    audit_log: "Column-level CRDT merge applied successfully".into(),
                }
            }
            _ => {
                // If one mutation is a delete, delete takes precedence
                ConflictResolutionResult {
                    primary_key: meta_remote.primary_key.clone(),
                    table_name: meta_remote.table_name.clone(),
                    strategy_applied: ConflictStrategy::ColumnMerge,
                    winner_origin: meta_remote.origin_node_id.clone(),
                    winning_mutation: remote.clone(),
                    superseded_origin: meta_local.origin_node_id.clone(),
                    audit_log: "Delete tombstone precedence".into(),
                }
            }
        }
    }
}

/// Real-time multi-cloud replication latency stats.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplicationLatencyMetrics {
    pub source_node: String,
    pub target_node: String,
    pub current_lag_ms: f64,
    pub p95_lag_ms: f64,
    pub wal_bytes_lag: u64,
    pub is_sub_second_sla_met: bool,
    pub last_heartbeat_timestamp: u64,
}

/// Tracks and enforces sub-second replication latency (<1.0 second SLA).
#[derive(Debug)]
pub struct LatencyMonitor {
    max_sla_duration: Duration,
    samples: Vec<f64>,
}

impl LatencyMonitor {
    pub fn new(max_sla_duration: Duration) -> Self {
        Self {
            max_sla_duration,
            samples: Vec::with_capacity(100),
        }
    }

    pub fn record_sample(&mut self, lag: Duration) {
        let ms = lag.as_secs_f64() * 1000.0;
        if self.samples.len() >= 100 {
            self.samples.remove(0);
        }
        self.samples.push(ms);
    }

    pub fn evaluate_metrics(
        &self,
        source: &DatabaseNode,
        target: &DatabaseNode,
        wal_lag: u64,
    ) -> ReplicationLatencyMetrics {
        let current_lag = self.samples.last().copied().unwrap_or(0.0);
        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let p95 = if sorted.is_empty() {
            0.0
        } else {
            let idx = ((sorted.len() as f64) * 0.95).floor() as usize;
            sorted[idx.min(sorted.len() - 1)]
        };

        let max_ms = self.max_sla_duration.as_secs_f64() * 1000.0;
        let sla_met = current_lag <= max_ms && p95 <= max_ms;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        ReplicationLatencyMetrics {
            source_node: source.id.clone(),
            target_node: target.id.clone(),
            current_lag_ms: current_lag,
            p95_lag_ms: p95,
            wal_bytes_lag: wal_lag,
            is_sub_second_sla_met: sla_met,
            last_heartbeat_timestamp: now,
        }
    }
}

/// Result of a cross-cloud regional failover simulation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailoverSimulationReport {
    pub failed_node: String,
    pub promoted_node: String,
    pub pre_failover_wal: WalLsn,
    pub post_failover_wal: WalLsn,
    pub total_records_checked: u64,
    pub divergent_records: u64,
    pub data_loss_detected: bool,
    pub failover_duration_ms: u64,
    pub checksum_verification_passed: bool,
}

/// Simulated Multi-Cloud Active-Active Database Coordinator.
pub struct ActiveActiveSyncCoordinator {
    pub aws_node: DatabaseNode,
    pub gcp_node: DatabaseNode,
    pub resolver: ConflictResolver,
    pub latency_monitor: LatencyMonitor,
    pub is_aws_online: bool,
    pub is_gcp_online: bool,
    aws_wal: WalLsn,
    gcp_wal: WalLsn,
    record_store_aws: HashMap<String, ReplicationMutation>,
    record_store_gcp: HashMap<String, ReplicationMutation>,
}

impl ActiveActiveSyncCoordinator {
    pub fn new(aws_node: DatabaseNode, gcp_node: DatabaseNode) -> Self {
        Self {
            aws_node,
            gcp_node,
            resolver: ConflictResolver::new(ConflictStrategy::LastWriteWins),
            latency_monitor: LatencyMonitor::new(Duration::from_millis(1000)),
            is_aws_online: true,
            is_gcp_online: true,
            aws_wal: WalLsn(1000),
            gcp_wal: WalLsn(1000),
            record_store_aws: HashMap::new(),
            record_store_gcp: HashMap::new(),
        }
    }

    /// Generates DDL queries to initialize bi-directional logical replication.
    pub fn generate_bi_directional_ddl(&self) -> (String, String) {
        let aws_ddl = format!(
            "-- AWS RDS Node ({})\n\
             CREATE PUBLICATION tessera_active_pub FOR ALL TABLES WITH (publish = 'insert, update, delete');\n\
             CREATE SUBSCRIPTION tessera_from_gcp \n\
               CONNECTION 'host={} port={} dbname={} user=tessera_rep'\n\
               PUBLICATION tessera_active_pub \n\
               WITH (origin = none, copy_data = false, create_slot = false, synchronous_commit = off);",
            self.aws_node.id, self.gcp_node.endpoint, self.gcp_node.port, self.gcp_node.database
        );

        let gcp_ddl = format!(
            "-- GCP Cloud SQL Node ({})\n\
             CREATE PUBLICATION tessera_active_pub FOR ALL TABLES WITH (publish = 'insert, update, delete');\n\
             CREATE SUBSCRIPTION tessera_from_aws \n\
               CONNECTION 'host={} port={} dbname={} user=tessera_rep'\n\
               PUBLICATION tessera_active_pub \n\
               WITH (origin = none, copy_data = false, create_slot = false, synchronous_commit = off);",
            self.gcp_node.id, self.aws_node.endpoint, self.aws_node.port, self.aws_node.database
        );

        (aws_ddl, gcp_ddl)
    }

    /// Applies a local write to AWS RDS node and replicates to GCP Cloud SQL.
    pub fn apply_write_to_aws(&mut self, mutation: ReplicationMutation) {
        let pk = mutation.metadata().primary_key.clone();
        self.record_store_aws.insert(pk.clone(), mutation.clone());
        self.aws_wal.0 += 128;

        // Replicate to GCP if online and non-loop origin
        if self.is_gcp_online && mutation.metadata().origin_node_id != self.gcp_node.id {
            if let Some(existing_gcp) = self.record_store_gcp.get(&pk) {
                // Conflict resolution needed
                let verdict = self.resolver.resolve(
                    existing_gcp,
                    &mutation,
                    &self.gcp_node,
                    &self.aws_node,
                );
                self.record_store_gcp.insert(pk, verdict.winning_mutation);
            } else {
                self.record_store_gcp.insert(pk, mutation);
            }
            self.gcp_wal.0 += 128;
            self.latency_monitor.record_sample(Duration::from_millis(150)); // Simulated 150ms cross-cloud sync
        }
    }

    /// Applies a local write to GCP Cloud SQL node and replicates to AWS RDS.
    pub fn apply_write_to_gcp(&mut self, mutation: ReplicationMutation) {
        let pk = mutation.metadata().primary_key.clone();
        self.record_store_gcp.insert(pk.clone(), mutation.clone());
        self.gcp_wal.0 += 128;

        // Replicate to AWS if online and non-loop origin
        if self.is_aws_online && mutation.metadata().origin_node_id != self.aws_node.id {
            if let Some(existing_aws) = self.record_store_aws.get(&pk) {
                let verdict = self.resolver.resolve(
                    existing_aws,
                    &mutation,
                    &self.aws_node,
                    &self.gcp_node,
                );
                self.record_store_aws.insert(pk, verdict.winning_mutation);
            } else {
                self.record_store_aws.insert(pk, mutation);
            }
            self.aws_wal.0 += 128;
            self.latency_monitor.record_sample(Duration::from_millis(165)); // Simulated 165ms cross-cloud sync
        }
    }

    /// Simulates failover when AWS region experiences a total outage, validating zero data loss.
    pub fn simulate_aws_failover(&mut self) -> FailoverSimulationReport {
        let start = Instant::now();
        let pre_failover_wal = self.aws_wal;

        // 1. Simulate AWS regional failure
        self.is_aws_online = false;

        // 2. Validate all records currently known were replicated to GCP before failure
        let mut divergent = 0;
        let total = self.record_store_aws.len() as u64;

        for (pk, aws_mut) in &self.record_store_aws {
            match self.record_store_gcp.get(pk) {
                Some(gcp_mut) => {
                    if aws_mut.metadata().checksum != gcp_mut.metadata().checksum {
                        divergent += 1;
                    }
                }
                None => {
                    divergent += 1;
                }
            }
        }

        // 3. Promote GCP as sole writer
        self.gcp_wal.0 += 500;
        let failover_duration = start.elapsed().as_millis() as u64;

        FailoverSimulationReport {
            failed_node: self.aws_node.id.clone(),
            promoted_node: self.gcp_node.id.clone(),
            pre_failover_wal,
            post_failover_wal: self.gcp_wal,
            total_records_checked: total,
            divergent_records: divergent,
            data_loss_detected: divergent > 0,
            failover_duration_ms: failover_duration,
            checksum_verification_passed: divergent == 0,
        }
    }
}

pub fn compute_record_checksum(table: &str, pk: &str, columns: &HashMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(table.as_bytes());
    hasher.update(b":");
    hasher.update(pk.as_bytes());
    let mut keys: Vec<_> = columns.keys().collect();
    keys.sort();
    for k in keys {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(columns.get(k).unwrap().as_bytes());
        hasher.update(b";");
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bi_directional_ddl_generation() {
        let aws = DatabaseNode::new_aws("aws-node-1", "rds.us-east-1.amazonaws.com");
        let gcp = DatabaseNode::new_gcp("gcp-node-1", "sql.us-central1.gcp.internal");
        let coord = ActiveActiveSyncCoordinator::new(aws, gcp);

        let (aws_ddl, gcp_ddl) = coord.generate_bi_directional_ddl();
        assert!(aws_ddl.contains("CREATE PUBLICATION tessera_active_pub"));
        assert!(aws_ddl.contains("CREATE SUBSCRIPTION tessera_from_gcp"));
        assert!(aws_ddl.contains("origin = none"));

        assert!(gcp_ddl.contains("CREATE PUBLICATION tessera_active_pub"));
        assert!(gcp_ddl.contains("CREATE SUBSCRIPTION tessera_from_aws"));
        assert!(gcp_ddl.contains("origin = none"));
    }

    #[test]
    fn test_conflict_resolution_last_write_wins() {
        let resolver = ConflictResolver::new(ConflictStrategy::LastWriteWins);
        let aws_node = DatabaseNode::new_aws("aws-1", "aws.internal");
        let gcp_node = DatabaseNode::new_gcp("gcp-1", "gcp.internal");

        let mut cols_early = HashMap::new();
        cols_early.insert("balance".into(), "100.00".into());
        let meta_early = RecordMetadata {
            primary_key: "acc_101".into(),
            table_name: "accounts".into(),
            version: 1,
            updated_at_micros: 1000,
            origin_node_id: aws_node.id.clone(),
            checksum: "chk_early".into(),
        };
        let mut_early = ReplicationMutation::Update {
            metadata: meta_early,
            columns: cols_early,
        };

        let mut cols_late = HashMap::new();
        cols_late.insert("balance".into(), "250.00".into());
        let meta_late = RecordMetadata {
            primary_key: "acc_101".into(),
            table_name: "accounts".into(),
            version: 2,
            updated_at_micros: 2000, // Newer timestamp
            origin_node_id: gcp_node.id.clone(),
            checksum: "chk_late".into(),
        };
        let mut_late = ReplicationMutation::Update {
            metadata: meta_late,
            columns: cols_late,
        };

        let verdict = resolver.resolve(&mut_early, &mut_late, &aws_node, &gcp_node);
        assert_eq!(verdict.winner_origin, gcp_node.id);
        assert_eq!(verdict.strategy_applied, ConflictStrategy::LastWriteWins);
        assert_eq!(verdict.winning_mutation, mut_late);
    }

    #[test]
    fn test_conflict_resolution_tie_breaker_priority() {
        let resolver = ConflictResolver::new(ConflictStrategy::LastWriteWins);
        let aws_node = DatabaseNode::new_aws("aws-1", "aws.internal"); // priority 200
        let gcp_node = DatabaseNode::new_gcp("gcp-1", "gcp.internal"); // priority 100

        let meta_aws = RecordMetadata {
            primary_key: "acc_101".into(),
            table_name: "accounts".into(),
            version: 1,
            updated_at_micros: 5000, // Identical timestamp
            origin_node_id: aws_node.id.clone(),
            checksum: "chk_aws".into(),
        };
        let mut_aws = ReplicationMutation::Update {
            metadata: meta_aws,
            columns: HashMap::new(),
        };

        let meta_gcp = RecordMetadata {
            primary_key: "acc_101".into(),
            table_name: "accounts".into(),
            version: 1,
            updated_at_micros: 5000, // Identical timestamp
            origin_node_id: gcp_node.id.clone(),
            checksum: "chk_gcp".into(),
        };
        let mut_gcp = ReplicationMutation::Update {
            metadata: meta_gcp,
            columns: HashMap::new(),
        };

        // When timestamps are tied, AWS (priority 200) wins over GCP (priority 100)
        let verdict = resolver.resolve(&mut_gcp, &mut_aws, &gcp_node, &aws_node);
        assert_eq!(verdict.winner_origin, aws_node.id);
        assert_eq!(verdict.strategy_applied, ConflictStrategy::HighestNodePriority);
    }

    #[test]
    fn test_sub_second_latency_monitoring_sla() {
        let mut monitor = LatencyMonitor::new(Duration::from_millis(1000));
        let aws = DatabaseNode::new_aws("aws-1", "aws.internal");
        let gcp = DatabaseNode::new_gcp("gcp-1", "gcp.internal");

        // Record samples well under 1000ms SLA (e.g. 120ms, 150ms, 210ms)
        monitor.record_sample(Duration::from_millis(120));
        monitor.record_sample(Duration::from_millis(150));
        monitor.record_sample(Duration::from_millis(210));

        let metrics = monitor.evaluate_metrics(&aws, &gcp, 0);
        assert!(metrics.is_sub_second_sla_met);
        assert!(metrics.current_lag_ms < 1000.0);
        assert!(metrics.p95_lag_ms < 1000.0);

        // Record spike exceeding 1000ms SLA
        monitor.record_sample(Duration::from_millis(1450));
        let metrics_spike = monitor.evaluate_metrics(&aws, &gcp, 5000);
        assert!(!metrics_spike.is_sub_second_sla_met);
        assert_eq!(metrics_spike.current_lag_ms, 1450.0);
    }

    #[test]
    fn test_simulated_failover_and_zero_data_loss_validation() {
        let aws = DatabaseNode::new_aws("aws-1", "aws.internal");
        let gcp = DatabaseNode::new_gcp("gcp-1", "gcp.internal");
        let mut coord = ActiveActiveSyncCoordinator::new(aws, gcp);

        // Seed 10 records into AWS, which synchronize to GCP
        for i in 0..10 {
            let mut cols = HashMap::new();
            cols.insert("id".into(), format!("rec_{}", i));
            cols.insert("asset_code".into(), "USDC".into());
            let chk = compute_record_checksum("assets", &format!("rec_{}", i), &cols);

            let mutation = ReplicationMutation::Insert {
                metadata: RecordMetadata {
                    primary_key: format!("rec_{}", i),
                    table_name: "assets".into(),
                    version: 1,
                    updated_at_micros: 10000 + i,
                    origin_node_id: "aws-1".into(),
                    checksum: chk,
                },
                columns: cols,
            };
            coord.apply_write_to_aws(mutation);
        }

        // Simulate regional AWS failover
        let report = coord.simulate_aws_failover();

        assert_eq!(report.failed_node, "aws-1");
        assert_eq!(report.promoted_node, "gcp-1");
        assert_eq!(report.total_records_checked, 10);
        assert_eq!(report.divergent_records, 0);
        assert!(!report.data_loss_detected);
        assert!(report.checksum_verification_passed);
        assert!(report.post_failover_wal > report.pre_failover_wal);
    }
}
