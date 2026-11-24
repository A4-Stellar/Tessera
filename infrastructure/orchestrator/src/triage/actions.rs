//! The three automated runbook actions of issue #474, each written against a
//! small backend trait so they can be exercised without a cluster:
//!
//! | step | backend | rollback |
//! |---|---|---|
//! | [`PostgresConnectionCheck`] | [`ConnectionStats`] | none (read-only) |
//! | [`RestartStuckIndexer`] | [`Workloads`] | restore the previous `restartedAt` pod-template annotation, which rolls the deployment back to the pods it had |
//! | [`FlushRedisCache`] | [`CacheStore`] | `RESTORE` every key from the snapshot taken before the flush |

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

use super::{Alert, Outcome, RunbookStep, StepSuccess, UndoToken};

// ── PostgreSQL connection count ─────────────────────────────────────────────

/// Connection usage of the API database.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionUsage {
    /// Client backends currently connected (`pg_stat_activity`).
    pub in_use: u32,
    /// `max_connections` minus `superuser_reserved_connections`.
    pub max: u32,
}

impl ConnectionUsage {
    pub fn ratio(self) -> f64 {
        if self.max == 0 {
            1.0
        } else {
            f64::from(self.in_use) / f64::from(self.max)
        }
    }
}

#[async_trait]
pub trait ConnectionStats: Send + Sync {
    async fn connection_usage(&self) -> Result<ConnectionUsage, String>;
}

/// Checks the PostgreSQL connection count. It never changes anything: a pool
/// that is close to exhaustion is reported as `degraded` so the next steps
/// (and the on-call engineer) know the database is part of the incident.
pub struct PostgresConnectionCheck {
    stats: Arc<dyn ConnectionStats>,
    /// Usage at or above this fraction of `max` is `degraded`.
    pub degraded_ratio: f64,
    pub timeout: Duration,
}

impl PostgresConnectionCheck {
    pub fn new(stats: Arc<dyn ConnectionStats>) -> Self {
        Self {
            stats,
            degraded_ratio: 0.9,
            timeout: Duration::from_secs(5),
        }
    }
}

#[async_trait]
impl RunbookStep for PostgresConnectionCheck {
    fn action(&self) -> &'static str {
        "check_postgres_connections"
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    async fn execute(&self, _alert: &Alert) -> Result<StepSuccess, String> {
        let usage = self.stats.connection_usage().await?;
        let pct = usage.ratio() * 100.0;
        let outcome = if usage.ratio() >= self.degraded_ratio {
            Outcome::Degraded
        } else {
            Outcome::Healthy
        };
        Ok(StepSuccess::new(
            outcome,
            format!(
                "{} of {} connections in use ({pct:.0}%)",
                usage.in_use, usage.max
            ),
        ))
    }
}

// ── Indexer restart ─────────────────────────────────────────────────────────

/// Health of one pod of the indexer deployment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodHealth {
    pub name: String,
    pub ready: bool,
    /// Container waiting reason, e.g. `CrashLoopBackOff`.
    pub waiting_reason: Option<String>,
}

/// Rollout state of a deployment.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutStatus {
    pub desired: u32,
    pub updated: u32,
    pub ready: u32,
    /// `status.observedGeneration >= metadata.generation`.
    pub observed: bool,
}

impl RolloutStatus {
    /// Every desired replica runs the current template and is ready.
    pub fn complete(self) -> bool {
        self.observed && self.updated >= self.desired && self.ready >= self.desired
    }
}

/// The part of the Kubernetes API the indexer restart needs.
#[async_trait]
pub trait Workloads: Send + Sync {
    async fn pods(&self, deployment: &str) -> Result<Vec<PodHealth>, String>;
    /// Current `kubectl.kubernetes.io/restartedAt` pod-template annotation.
    async fn restart_annotation(&self, deployment: &str) -> Result<Option<String>, String>;
    /// Set (or, with `None`, remove) the annotation. Changing it starts a
    /// rolling replacement of every pod, like `kubectl rollout restart`.
    async fn set_restart_annotation(
        &self,
        deployment: &str,
        value: Option<&str>,
    ) -> Result<(), String>;
    async fn rollout_status(&self, deployment: &str) -> Result<RolloutStatus, String>;
}

/// Restarts the indexer when a pod is stuck, then waits for the new pods to
/// become ready.
///
/// A pod counts as stuck when it is not ready or is waiting in
/// `CrashLoopBackOff`/`Error`. An indexer-lag alert is itself evidence that
/// the indexer is stuck (a ready pod that stopped advancing), so for alerts
/// named in `lag_alerts` the restart happens even when every pod reports
/// ready.
pub struct RestartStuckIndexer {
    workloads: Arc<dyn Workloads>,
    pub deployment: String,
    pub lag_alerts: Vec<String>,
    pub poll_interval: Duration,
    pub timeout: Duration,
    /// Produces the new annotation value (an RFC 3339 timestamp in
    /// production; injectable for deterministic tests).
    clock: Arc<dyn Fn() -> String + Send + Sync>,
}

impl RestartStuckIndexer {
    pub fn new(workloads: Arc<dyn Workloads>, deployment: impl Into<String>) -> Self {
        Self {
            workloads,
            deployment: deployment.into(),
            lag_alerts: vec!["HighIndexerLag".into(), "IndexerStalled".into()],
            poll_interval: Duration::from_secs(2),
            timeout: Duration::from_secs(60),
            clock: Arc::new(|| chrono::Utc::now().to_rfc3339()),
        }
    }

    pub fn with_clock(mut self, clock: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }
}

#[derive(Serialize, Deserialize)]
struct RestartUndo {
    deployment: String,
    previous: Option<String>,
}

fn is_stuck(pod: &PodHealth) -> bool {
    !pod.ready
        || matches!(
            pod.waiting_reason.as_deref(),
            Some("CrashLoopBackOff" | "Error" | "ImagePullBackOff")
        )
}

#[async_trait]
impl RunbookStep for RestartStuckIndexer {
    fn action(&self) -> &'static str {
        "restart_indexer_pod"
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    async fn prepare(&self, _alert: &Alert) -> Result<Option<UndoToken>, String> {
        let previous = self.workloads.restart_annotation(&self.deployment).await?;
        serde_json::to_value(RestartUndo {
            deployment: self.deployment.clone(),
            previous,
        })
        .map(Some)
        .map_err(|e| e.to_string())
    }

    async fn execute(&self, alert: &Alert) -> Result<StepSuccess, String> {
        let pods = self.workloads.pods(&self.deployment).await?;
        let stuck: Vec<&str> = pods
            .iter()
            .filter(|p| is_stuck(p))
            .map(|p| p.name.as_str())
            .collect();
        let lagging = self.lag_alerts.iter().any(|a| a == alert.name());
        if stuck.is_empty() && !lagging {
            return Ok(StepSuccess::new(
                Outcome::Skipped,
                format!("all {} indexer pods are ready", pods.len()),
            ));
        }

        let stamp = (self.clock)();
        self.workloads
            .set_restart_annotation(&self.deployment, Some(&stamp))
            .await?;

        // Wait for the rollout. The runner's timeout bounds this loop; when it
        // fires, the annotation is restored by `rollback`.
        loop {
            sleep(self.poll_interval).await;
            if self
                .workloads
                .rollout_status(&self.deployment)
                .await?
                .complete()
            {
                break;
            }
        }

        let reason = if stuck.is_empty() {
            format!("{} reported indexer lag", alert.name())
        } else {
            format!("stuck pods: {}", stuck.join(", "))
        };
        Ok(StepSuccess::new(
            Outcome::Remediated,
            format!(
                "restarted deployment/{} ({reason}); new pods ready",
                self.deployment
            ),
        ))
    }

    async fn rollback(&self, undo: UndoToken) -> Result<(), String> {
        let undo: RestartUndo = serde_json::from_value(undo).map_err(|e| e.to_string())?;
        self.workloads
            .set_restart_annotation(&undo.deployment, undo.previous.as_deref())
            .await
    }
}

// ── Redis cache flush ───────────────────────────────────────────────────────

/// A key captured with `DUMP` so it can be restored with `RESTORE`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedKey {
    pub key: String,
    /// `DUMP` payload, hex-encoded so the undo token stays JSON.
    pub dump_hex: String,
    /// Remaining TTL in milliseconds; `0` means no expiry.
    pub pttl_ms: u64,
}

#[async_trait]
pub trait CacheStore: Send + Sync {
    /// Keys matching `pattern`, stopping after `limit + 1` so callers can tell
    /// a set that is too large to snapshot.
    async fn keys(&self, pattern: &str, limit: usize) -> Result<Vec<String>, String>;
    async fn dump(&self, keys: &[String]) -> Result<Vec<CachedKey>, String>;
    /// Deletes the keys, returning how many existed.
    async fn delete(&self, keys: &[String]) -> Result<u64, String>;
    async fn restore(&self, entries: &[CachedKey]) -> Result<(), String>;
}

/// Flushes the API response cache (`cache:v1:*` by default) so stale
/// snapshots stop being served. Only keys under the pattern are touched, never
/// `FLUSHDB`, and the keys are snapshotted first so a failed flush can be
/// undone.
pub struct FlushRedisCache {
    cache: Arc<dyn CacheStore>,
    pub pattern: String,
    /// Refuse to flush more keys than this, since they could not all be
    /// snapshotted for rollback.
    pub max_keys: usize,
    pub timeout: Duration,
}

impl FlushRedisCache {
    pub fn new(cache: Arc<dyn CacheStore>) -> Self {
        Self {
            cache,
            pattern: "cache:v1:*".into(),
            max_keys: 10_000,
            timeout: Duration::from_secs(10),
        }
    }
}

#[async_trait]
impl RunbookStep for FlushRedisCache {
    fn action(&self) -> &'static str {
        "flush_redis_cache"
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    async fn prepare(&self, _alert: &Alert) -> Result<Option<UndoToken>, String> {
        let keys = self.cache.keys(&self.pattern, self.max_keys).await?;
        if keys.len() > self.max_keys {
            return Err(format!(
                "more than {} keys match {}; not flushing without a full snapshot",
                self.max_keys, self.pattern
            ));
        }
        let snapshot = self.cache.dump(&keys).await?;
        serde_json::to_value(snapshot)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    async fn execute(&self, _alert: &Alert) -> Result<StepSuccess, String> {
        let keys = self.cache.keys(&self.pattern, self.max_keys).await?;
        if keys.is_empty() {
            return Ok(StepSuccess::new(
                Outcome::Skipped,
                format!("no keys match {}", self.pattern),
            ));
        }
        let deleted = self.cache.delete(&keys).await?;
        Ok(StepSuccess::new(
            Outcome::Remediated,
            format!("flushed {deleted} keys matching {}", self.pattern),
        ))
    }

    async fn rollback(&self, undo: UndoToken) -> Result<(), String> {
        let entries: Vec<CachedKey> = serde_json::from_value(undo).map_err(|e| e.to_string())?;
        self.cache.restore(&entries).await
    }
}
