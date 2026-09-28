//! PostgreSQL implementations of [`HealthProbe`] and [`Promoter`].
//!
//! The monitoring role needs `pg_monitor` (for `pg_stat_replication` and
//! `pg_stat_wal_receiver`). Promotion needs `EXECUTE` on `pg_promote` and
//! fencing needs superuser, or the equivalent grants.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

use crate::failover::{HealthProbe, Promoter};
use crate::types::{
    Lsn, Node, PrimaryStatus, ProbeError, PromoteError, ReplicaStatus, ReplicationPeer, SyncState,
};

const PRIMARY_SQL: &str = "SELECT pg_is_in_recovery() AS in_recovery, \
     CASE WHEN pg_is_in_recovery() THEN NULL ELSE pg_current_wal_lsn()::text END AS current_lsn";

const PEERS_SQL: &str = "SELECT application_name, sync_state, replay_lsn::text AS replay_lsn \
     FROM pg_stat_replication";

const REPLICA_SQL: &str = "SELECT pg_is_in_recovery() AS in_recovery, \
     pg_last_wal_receive_lsn()::text AS receive_lsn, \
     pg_last_wal_replay_lsn()::text AS replay_lsn, \
     EXTRACT(EPOCH FROM (now() - pg_last_xact_replay_timestamp()))::float8 AS replay_delay, \
     (SELECT status FROM pg_stat_wal_receiver LIMIT 1) AS receiver_status, \
     (SELECT EXTRACT(EPOCH FROM (now() - last_msg_receipt_time))::float8 \
        FROM pg_stat_wal_receiver LIMIT 1) AS upstream_silence";

/// Connection pools for every node in the cluster, keyed by node name.
pub struct PgCluster {
    pools: HashMap<String, PgPool>,
}

impl PgCluster {
    /// Build lazy pools; nothing connects until the first probe, so a dead
    /// node does not prevent the controller from starting.
    pub fn connect_lazy(
        dsns: impl IntoIterator<Item = (String, String)>,
        connect_timeout: Duration,
    ) -> Result<Self, sqlx::Error> {
        let mut pools = HashMap::new();
        for (name, dsn) in dsns {
            let pool = PgPoolOptions::new()
                .max_connections(2)
                .acquire_timeout(connect_timeout)
                .connect_lazy(&dsn)?;
            pools.insert(name, pool);
        }
        Ok(Self { pools })
    }

    fn pool(&self, node: &Node) -> Result<&PgPool, String> {
        self.pools
            .get(&node.name)
            .ok_or_else(|| format!("no connection configured for {}", node.name))
    }
}

fn unreachable(node: &Node, reason: impl ToString) -> ProbeError {
    ProbeError::Unreachable {
        node: node.name.clone(),
        reason: reason.to_string(),
    }
}

fn parse_lsn(node: &Node, value: Option<String>) -> Result<Option<Lsn>, ProbeError> {
    value
        .map(|v| v.parse::<Lsn>().map_err(|e| unreachable(node, e)))
        .transpose()
}

fn secs(value: Option<f64>) -> Option<Duration> {
    value.map(|s| Duration::from_secs_f64(s.max(0.0)))
}

#[async_trait]
impl HealthProbe for PgCluster {
    async fn probe_primary(&self, node: &Node) -> Result<PrimaryStatus, ProbeError> {
        let pool = self.pool(node).map_err(|e| unreachable(node, e))?;
        let row = sqlx::query(PRIMARY_SQL)
            .fetch_one(pool)
            .await
            .map_err(|e| unreachable(node, e))?;
        let in_recovery: bool = row
            .try_get("in_recovery")
            .map_err(|e| unreachable(node, e))?;
        let current_lsn = parse_lsn(
            node,
            row.try_get("current_lsn")
                .map_err(|e| unreachable(node, e))?,
        )?
        .unwrap_or_default();
        if in_recovery {
            return Ok(PrimaryStatus {
                in_recovery,
                current_lsn,
                peers: vec![],
            });
        }

        let peers = sqlx::query(PEERS_SQL)
            .fetch_all(pool)
            .await
            .map_err(|e| unreachable(node, e))?
            .into_iter()
            .map(|row| {
                let sync_state: String = row
                    .try_get("sync_state")
                    .map_err(|e| unreachable(node, e))?;
                Ok(ReplicationPeer {
                    application_name: row
                        .try_get("application_name")
                        .map_err(|e| unreachable(node, e))?,
                    sync_state: sync_state
                        .parse::<SyncState>()
                        .map_err(|e| unreachable(node, e))?,
                    replay_lsn: parse_lsn(
                        node,
                        row.try_get("replay_lsn")
                            .map_err(|e| unreachable(node, e))?,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, ProbeError>>()?;

        Ok(PrimaryStatus {
            in_recovery,
            current_lsn,
            peers,
        })
    }

    async fn probe_replica(&self, node: &Node) -> Result<ReplicaStatus, ProbeError> {
        let pool = self.pool(node).map_err(|e| unreachable(node, e))?;
        let row = sqlx::query(REPLICA_SQL)
            .fetch_one(pool)
            .await
            .map_err(|e| unreachable(node, e))?;
        let get_text = |col: &str| -> Result<Option<String>, ProbeError> {
            row.try_get(col).map_err(|e| unreachable(node, e))
        };
        let get_f64 = |col: &str| -> Result<Option<f64>, ProbeError> {
            row.try_get(col).map_err(|e| unreachable(node, e))
        };
        Ok(ReplicaStatus {
            in_recovery: row
                .try_get("in_recovery")
                .map_err(|e| unreachable(node, e))?,
            receive_lsn: parse_lsn(node, get_text("receive_lsn")?)?,
            replay_lsn: parse_lsn(node, get_text("replay_lsn")?)?,
            replay_delay: secs(get_f64("replay_delay")?),
            wal_receiver_streaming: get_text("receiver_status")?.as_deref() == Some("streaming"),
            upstream_silence: secs(get_f64("upstream_silence")?),
        })
    }
}

#[async_trait]
impl Promoter for PgCluster {
    async fn promote(&self, node: &Node, wait: Duration) -> Result<(), PromoteError> {
        let failed = |reason: String| PromoteError::Failed {
            node: node.name.clone(),
            reason,
        };
        let pool = self.pool(node).map_err(failed)?;
        let wait_seconds = i32::try_from(wait.as_secs().max(1)).unwrap_or(i32::MAX);
        let promoted: bool = sqlx::query_scalar("SELECT pg_promote(true, $1)")
            .bind(wait_seconds)
            .fetch_one(pool)
            .await
            .map_err(|e| failed(e.to_string()))?;
        if promoted {
            Ok(())
        } else {
            Err(failed(format!(
                "pg_promote did not complete within {wait_seconds}s"
            )))
        }
    }

    async fn fence(&self, node: &Node) -> Result<(), PromoteError> {
        let failed = |reason: String| PromoteError::FenceFailed {
            node: node.name.clone(),
            reason,
        };
        let pool = self.pool(node).map_err(failed)?;
        // New sessions default to read-only, then existing writers are cut off.
        for sql in [
            "ALTER SYSTEM SET default_transaction_read_only = on",
            "SELECT pg_reload_conf()",
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE backend_type = 'client backend' AND pid <> pg_backend_pid()",
        ] {
            sqlx::query(sql)
                .execute(pool)
                .await
                .map_err(|e| failed(e.to_string()))?;
        }
        Ok(())
    }
}
