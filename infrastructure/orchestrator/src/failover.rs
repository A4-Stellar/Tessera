//! Automated cross-region PostgreSQL failover controller.
//!
//! The controller heartbeats the primary writer and every streaming standby
//! (for example a primary in AWS us-east-1 and read replicas in eu-west-1).
//! When the primary misses `failure_threshold` consecutive heartbeats it runs
//! a failover that must finish promotion within `promotion_deadline`
//! (30 seconds by default), measured from the moment of detection:
//!
//! 1. **Witness check** - the standbys are re-probed. If any of them is still
//!    streaming from the primary, the controller is the one that is
//!    partitioned and failover is refused, which prevents split brain.
//! 2. **Candidate selection** - synchronous standbys first, then the standby
//!    holding the most WAL.
//! 3. **Zero-data-loss gate** - the candidate must hold every WAL byte any
//!    node is known to have (the "high-water mark": the last primary LSN seen
//!    and every standby's received LSN) and must finish replaying it before it
//!    is promoted. With `require_synchronous_candidate` (the default) only a
//!    standby the primary was waiting on for commits is eligible, so every
//!    acknowledged commit is guaranteed to be on it.
//! 4. **Fence** the old primary (best effort) so it cannot keep taking writes
//!    if it comes back.
//! 5. **Promote** the candidate and confirm it left recovery.
//! 6. **Re-route** traffic by pointing the database DNS record at the new
//!    primary. DNS failures do not undo the promotion; the update is retried
//!    on every following heartbeat until it succeeds.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::join_all;
use serde::Serialize;
use thiserror::Error;
use tokio::time::{sleep, timeout, Instant, MissedTickBehavior};
use tracing::{debug, error, info, warn};

use crate::types::{DnsError, Lsn, Node, PrimaryStatus, ProbeError, PromoteError, ReplicaStatus};

/// Reads health and replication state from a database node.
#[async_trait]
pub trait HealthProbe: Send + Sync {
    async fn probe_primary(&self, node: &Node) -> Result<PrimaryStatus, ProbeError>;
    async fn probe_replica(&self, node: &Node) -> Result<ReplicaStatus, ProbeError>;
}

/// Changes the role of a database node.
#[async_trait]
pub trait Promoter: Send + Sync {
    /// Promote a standby to a writable primary, waiting at most `wait`.
    async fn promote(&self, node: &Node, wait: Duration) -> Result<(), PromoteError>;
    /// Stop a (possibly recovered) old primary from accepting writes.
    async fn fence(&self, node: &Node) -> Result<(), PromoteError>;
}

/// Points the public database hostname at a node.
#[async_trait]
pub trait DnsUpdater: Send + Sync {
    async fn point_to(&self, target: &Node) -> Result<(), DnsError>;
}

/// Timing and safety knobs for the controller.
#[derive(Clone, Debug)]
pub struct FailoverPolicy {
    /// Interval between heartbeats.
    pub heartbeat_interval: Duration,
    /// Per-probe timeout. A probe that takes longer counts as a failure.
    pub probe_timeout: Duration,
    /// Consecutive failed primary heartbeats that declare the primary dead.
    pub failure_threshold: u32,
    /// Budget from detection to a confirmed, writable new primary.
    pub promotion_deadline: Duration,
    /// Maximum WAL (bytes) the promoted standby may be missing relative to the
    /// high-water mark. `0` means zero data loss.
    pub max_data_loss_bytes: u64,
    /// Only promote standbys the primary listed as `sync` or `quorum`.
    pub require_synchronous_candidate: bool,
    /// Refuse to fail over while any reachable standby still streams from the
    /// primary, and require a majority of standbys to be reachable.
    pub require_witness_quorum: bool,
    /// A standby that has not heard from the primary for this long no longer
    /// vouches for it.
    pub upstream_silence_threshold: Duration,
    /// Poll interval while waiting for the candidate to finish replaying WAL.
    pub catchup_poll_interval: Duration,
    /// Time allowed for fencing the old primary before moving on.
    pub fence_timeout: Duration,
    /// Replication lag above which a warning is logged during normal operation.
    pub lag_alert_bytes: u64,
    /// DNS update attempts per failover (and per retry heartbeat).
    pub dns_attempts: u32,
    /// Timeout for one DNS API call.
    pub dns_timeout: Duration,
}

impl Default for FailoverPolicy {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(2),
            probe_timeout: Duration::from_millis(1500),
            failure_threshold: 3,
            promotion_deadline: Duration::from_secs(30),
            max_data_loss_bytes: 0,
            require_synchronous_candidate: true,
            require_witness_quorum: true,
            upstream_silence_threshold: Duration::from_secs(5),
            catchup_poll_interval: Duration::from_millis(250),
            fence_timeout: Duration::from_secs(2),
            lag_alert_bytes: 16 * 1024 * 1024,
            dns_attempts: 3,
            dns_timeout: Duration::from_secs(5),
        }
    }
}

impl FailoverPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.failure_threshold == 0 {
            return Err("failure_threshold must be at least 1".into());
        }
        if self.heartbeat_interval.is_zero() || self.catchup_poll_interval.is_zero() {
            return Err("heartbeat and catch-up intervals must be non-zero".into());
        }
        if self.probe_timeout >= self.promotion_deadline {
            return Err("probe_timeout must be shorter than promotion_deadline".into());
        }
        if self.dns_attempts == 0 {
            return Err("dns_attempts must be at least 1".into());
        }
        Ok(())
    }
}

/// Current role assignment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Topology {
    pub primary: Node,
    pub replicas: Vec<Node>,
    /// Former primaries. They must be rebuilt (e.g. `pg_rewind`) and re-added
    /// as standbys by an operator before they serve traffic again.
    pub fenced: Vec<Node>,
}

/// Replication lag of one standby relative to the primary.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReplicationLag {
    pub node: String,
    pub region: String,
    pub bytes_behind: Option<u64>,
    pub replay_delay: Option<Duration>,
}

/// Summary of a completed failover.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FailoverReport {
    pub old_primary: Node,
    pub new_primary: Node,
    pub high_water_lsn: Lsn,
    pub promoted_at_lsn: Lsn,
    /// Detection to confirmed writable primary.
    pub time_to_promote: Duration,
    /// Detection to DNS switched, when the DNS update succeeded.
    pub time_to_reroute: Option<Duration>,
    pub dns_updated: bool,
}

/// Result of one heartbeat.
#[derive(Debug)]
pub enum TickOutcome {
    Healthy {
        lag: Vec<ReplicationLag>,
    },
    Degraded {
        consecutive_failures: u32,
        reason: String,
    },
    FailoverAborted(FailoverError),
    FailedOver(FailoverReport),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FailoverError {
    #[error(
        "primary still reachable from standbys {witnesses:?}; controller is likely partitioned"
    )]
    PrimaryStillReachable { witnesses: Vec<String> },
    #[error("witness quorum not met: {reachable} of {configured} standbys reachable")]
    NoWitnessQuorum { reachable: usize, configured: usize },
    #[error("no reachable standby is eligible for promotion")]
    NoEligibleStandby,
    #[error("no synchronous standby is available; promoting an async standby could lose commits")]
    NoSynchronousStandby,
    #[error("best candidate {candidate} holds WAL up to {candidate_lsn}, high-water mark is {high_water}")]
    DataLossRisk {
        candidate: String,
        candidate_lsn: Lsn,
        high_water: Lsn,
    },
    #[error("{candidate} did not finish replaying WAL to {target} before the deadline")]
    CatchUpTimeout { candidate: String, target: Lsn },
    #[error("promotion deadline exceeded during {stage}")]
    DeadlineExceeded { stage: &'static str },
    #[error(transparent)]
    Promote(#[from] PromoteError),
}

pub struct FailoverController {
    topology: Topology,
    probe: Arc<dyn HealthProbe>,
    promoter: Arc<dyn Promoter>,
    dns: Arc<dyn DnsUpdater>,
    policy: FailoverPolicy,
    consecutive_failures: u32,
    /// Last successful heartbeat of the current primary.
    last_primary: Option<PrimaryStatus>,
    /// Node whose DNS switch still has to be retried after a failover.
    dns_pending: Option<Node>,
}

impl FailoverController {
    pub fn new(
        topology: Topology,
        probe: Arc<dyn HealthProbe>,
        promoter: Arc<dyn Promoter>,
        dns: Arc<dyn DnsUpdater>,
        policy: FailoverPolicy,
    ) -> Self {
        Self {
            topology,
            probe,
            promoter,
            dns,
            policy,
            consecutive_failures: 0,
            last_primary: None,
            dns_pending: None,
        }
    }

    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// Heartbeat until `shutdown` resolves, then return the final topology.
    pub async fn run<F: Future<Output = ()>>(mut self, shutdown: F) -> Topology {
        let mut ticker = tokio::time::interval(self.policy.heartbeat_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = ticker.tick() => {
                    match self.tick().await {
                        TickOutcome::FailedOver(report) => info!(?report, "failover completed"),
                        TickOutcome::FailoverAborted(err) => error!(%err, "failover aborted"),
                        _ => {}
                    }
                }
            }
        }
        self.topology
    }

    /// Run one heartbeat, failing over if the primary is declared dead.
    pub async fn tick(&mut self) -> TickOutcome {
        if let Some(target) = self.dns_pending.clone() {
            if self.update_dns(&target).await {
                info!(node = %target.name, "deferred DNS switch completed");
                self.dns_pending = None;
            }
        }

        let primary = self.topology.primary.clone();
        let (primary_result, replicas) =
            tokio::join!(self.probe_primary(&primary), self.probe_replicas());

        let reason = match primary_result {
            Ok(status) if !status.in_recovery => {
                if self.consecutive_failures > 0 {
                    info!(node = %primary.name, "primary heartbeat recovered");
                }
                self.consecutive_failures = 0;
                let lag = self.replication_lag(&status, &replicas);
                self.last_primary = Some(status);
                return TickOutcome::Healthy { lag };
            }
            Ok(_) => "primary reports it is in recovery (read-only)".to_string(),
            Err(err) => err.to_string(),
        };

        self.consecutive_failures += 1;
        warn!(
            node = %primary.name,
            region = %primary.region,
            failures = self.consecutive_failures,
            threshold = self.policy.failure_threshold,
            %reason,
            "primary heartbeat failed"
        );
        if self.consecutive_failures < self.policy.failure_threshold {
            return TickOutcome::Degraded {
                consecutive_failures: self.consecutive_failures,
                reason,
            };
        }

        let detected_at = Instant::now();
        error!(node = %primary.name, "primary declared dead, starting failover");
        match self.execute_failover(detected_at).await {
            Ok(report) => TickOutcome::FailedOver(report),
            Err(err) => TickOutcome::FailoverAborted(err),
        }
    }

    async fn execute_failover(
        &mut self,
        detected_at: Instant,
    ) -> Result<FailoverReport, FailoverError> {
        let deadline = detected_at + self.policy.promotion_deadline;
        let policy = self.policy.clone();

        // 1. Witness check on fresh observations.
        let reachable: Vec<(Node, ReplicaStatus)> = self
            .probe_replicas()
            .await
            .into_iter()
            .filter_map(|(node, result)| result.ok().map(|status| (node, status)))
            .filter(|(_, status)| status.in_recovery)
            .collect();

        if policy.require_witness_quorum {
            let witnesses: Vec<String> = reachable
                .iter()
                .filter(|(_, s)| s.sees_live_upstream(policy.upstream_silence_threshold))
                .map(|(n, _)| n.name.clone())
                .collect();
            if !witnesses.is_empty() {
                return Err(FailoverError::PrimaryStillReachable { witnesses });
            }
            let configured = self.topology.replicas.len();
            if reachable.len() * 2 <= configured {
                return Err(FailoverError::NoWitnessQuorum {
                    reachable: reachable.len(),
                    configured,
                });
            }
        }
        if reachable.is_empty() {
            return Err(FailoverError::NoEligibleStandby);
        }

        // 2. High-water mark: the most WAL any node is known to hold.
        let high_water = reachable
            .iter()
            .filter_map(|(_, s)| s.durable_lsn())
            .chain(self.last_primary.as_ref().map(|p| p.current_lsn))
            .max()
            .unwrap_or_default();

        // 3. Rank candidates: synchronous first, then most WAL, then config order.
        let sync_names: HashSet<&str> = self
            .last_primary
            .iter()
            .flat_map(|p| p.peers.iter())
            .filter(|peer| peer.sync_state.is_synchronous())
            .map(|peer| peer.application_name.as_str())
            .collect();
        let mut candidates: Vec<&(Node, ReplicaStatus)> = reachable
            .iter()
            .filter(|(n, _)| {
                !policy.require_synchronous_candidate || sync_names.contains(n.name.as_str())
            })
            .collect();
        if candidates.is_empty() {
            return Err(FailoverError::NoSynchronousStandby);
        }
        candidates.sort_by_key(|(n, s)| {
            (
                std::cmp::Reverse(sync_names.contains(n.name.as_str())),
                std::cmp::Reverse(s.durable_lsn()),
            )
        });

        let min_lsn = Lsn(high_water.0.saturating_sub(policy.max_data_loss_bytes));
        let (candidate, candidate_status) = candidates
            .iter()
            .find(|(_, s)| s.durable_lsn().unwrap_or_default() >= min_lsn)
            .map(|(n, s)| (n.clone(), s.clone()))
            .ok_or_else(|| {
                let (best, status) = candidates[0];
                FailoverError::DataLossRisk {
                    candidate: best.name.clone(),
                    candidate_lsn: status.durable_lsn().unwrap_or_default(),
                    high_water,
                }
            })?;
        info!(
            candidate = %candidate.name,
            region = %candidate.region,
            %high_water,
            "selected promotion candidate"
        );

        // 4. Wait until the candidate has applied everything it must hold.
        let replay_target = candidate_status
            .receive_lsn
            .unwrap_or_default()
            .max(min_lsn);
        let mut replayed = candidate_status.replay_lsn.unwrap_or_default();
        while replayed < replay_target {
            if Instant::now() + policy.catchup_poll_interval >= deadline {
                return Err(FailoverError::CatchUpTimeout {
                    candidate: candidate.name.clone(),
                    target: replay_target,
                });
            }
            debug!(candidate = %candidate.name, %replayed, %replay_target, "waiting for WAL replay");
            sleep(policy.catchup_poll_interval).await;
            if let Ok(status) = self.probe_replica(&candidate).await {
                replayed = status.replay_lsn.unwrap_or_default();
            }
        }

        // 5. Fence the old primary. Best effort: it is usually unreachable.
        let old_primary = self.topology.primary.clone();
        match timeout(policy.fence_timeout, self.promoter.fence(&old_primary)).await {
            Ok(Ok(())) => info!(node = %old_primary.name, "old primary fenced"),
            Ok(Err(err)) => warn!(%err, "could not fence old primary"),
            Err(_) => warn!(node = %old_primary.name, "fencing old primary timed out"),
        }

        // 6. Promote and confirm the candidate is writable.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(FailoverError::DeadlineExceeded { stage: "promotion" });
        }
        timeout(remaining, self.promoter.promote(&candidate, remaining))
            .await
            .map_err(|_| FailoverError::DeadlineExceeded { stage: "promotion" })??;

        let promoted_status = loop {
            if let Ok(status) = self.probe_primary(&candidate).await {
                if !status.in_recovery {
                    break status;
                }
            }
            if Instant::now() + policy.catchup_poll_interval >= deadline {
                return Err(FailoverError::DeadlineExceeded {
                    stage: "promotion confirmation",
                });
            }
            sleep(policy.catchup_poll_interval).await;
        };
        let time_to_promote = detected_at.elapsed();
        info!(
            node = %candidate.name,
            region = %candidate.region,
            elapsed_ms = time_to_promote.as_millis() as u64,
            "standby promoted to primary"
        );

        // Commit point: the candidate is the writer now, whatever DNS does.
        self.topology.replicas.retain(|n| n.name != candidate.name);
        self.topology.fenced.push(old_primary.clone());
        self.topology.primary = candidate.clone();
        self.consecutive_failures = 0;
        let promoted_at_lsn = promoted_status.current_lsn;
        self.last_primary = Some(promoted_status);

        // 7. Re-route traffic.
        let dns_updated = self.update_dns(&candidate).await;
        let time_to_reroute = dns_updated.then(|| detected_at.elapsed());
        if !dns_updated {
            error!(node = %candidate.name, "DNS switch failed, will retry on next heartbeat");
            self.dns_pending = Some(candidate.clone());
        }

        Ok(FailoverReport {
            old_primary,
            new_primary: candidate,
            high_water_lsn: high_water,
            promoted_at_lsn,
            time_to_promote,
            time_to_reroute,
            dns_updated,
        })
    }

    async fn update_dns(&self, target: &Node) -> bool {
        let mut backoff = Duration::from_millis(250);
        for attempt in 1..=self.policy.dns_attempts {
            match timeout(self.policy.dns_timeout, self.dns.point_to(target)).await {
                Ok(Ok(())) => {
                    info!(node = %target.name, endpoint = %target.endpoint, "DNS now points at new primary");
                    return true;
                }
                Ok(Err(err)) => warn!(attempt, %err, "DNS update failed"),
                Err(_) => warn!(attempt, "DNS update timed out"),
            }
            if attempt < self.policy.dns_attempts {
                sleep(backoff).await;
                backoff *= 2;
            }
        }
        false
    }

    async fn probe_primary(&self, node: &Node) -> Result<PrimaryStatus, ProbeError> {
        timeout(self.policy.probe_timeout, self.probe.probe_primary(node))
            .await
            .unwrap_or_else(|_| {
                Err(ProbeError::Timeout {
                    node: node.name.clone(),
                })
            })
    }

    async fn probe_replica(&self, node: &Node) -> Result<ReplicaStatus, ProbeError> {
        timeout(self.policy.probe_timeout, self.probe.probe_replica(node))
            .await
            .unwrap_or_else(|_| {
                Err(ProbeError::Timeout {
                    node: node.name.clone(),
                })
            })
    }

    async fn probe_replicas(&self) -> Vec<(Node, Result<ReplicaStatus, ProbeError>)> {
        let replicas = self.topology.replicas.clone();
        let results = join_all(replicas.iter().map(|n| self.probe_replica(n))).await;
        replicas.into_iter().zip(results).collect()
    }

    fn replication_lag(
        &self,
        primary: &PrimaryStatus,
        replicas: &[(Node, Result<ReplicaStatus, ProbeError>)],
    ) -> Vec<ReplicationLag> {
        let peer_replay: HashMap<&str, Option<Lsn>> = primary
            .peers
            .iter()
            .map(|p| (p.application_name.as_str(), p.replay_lsn))
            .collect();

        replicas
            .iter()
            .map(|(node, result)| {
                let status = result.as_ref().ok();
                // Prefer the standby's own view; fall back to the primary's.
                let replay = status
                    .and_then(|s| s.replay_lsn)
                    .or_else(|| peer_replay.get(node.name.as_str()).copied().flatten());
                let bytes_behind = replay.map(|lsn| lsn.bytes_behind(primary.current_lsn));
                let lag = ReplicationLag {
                    node: node.name.clone(),
                    region: node.region.clone(),
                    bytes_behind,
                    replay_delay: status.and_then(|s| s.replay_delay),
                };
                match (result, bytes_behind) {
                    (Err(err), _) => warn!(node = %node.name, %err, "standby heartbeat failed"),
                    (Ok(_), Some(bytes)) if bytes > self.policy.lag_alert_bytes => warn!(
                        node = %node.name,
                        region = %node.region,
                        bytes_behind = bytes,
                        "replication lag above alert threshold"
                    ),
                    _ => debug!(node = %node.name, ?lag.bytes_behind, "replication lag"),
                }
                lag
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ReplicationPeer, SyncState};
    use std::sync::Mutex;

    #[derive(Clone)]
    enum NodeState {
        Primary {
            lsn: Lsn,
            peers: Vec<ReplicationPeer>,
        },
        Replica(ReplicaStatus),
        Down,
    }

    /// In-memory cluster shared by the mock probe, promoter and DNS.
    #[derive(Default)]
    struct Cluster {
        nodes: Mutex<HashMap<String, NodeState>>,
        /// WAL each replica replays per probe while catching up.
        replay_step: Mutex<u64>,
        promote_delay: Mutex<Duration>,
        promoted: Mutex<Vec<String>>,
        fenced: Mutex<Vec<String>>,
        dns_targets: Mutex<Vec<String>>,
        dns_failures_left: Mutex<u32>,
    }

    impl Cluster {
        fn set(&self, name: &str, state: NodeState) {
            self.nodes.lock().unwrap().insert(name.to_string(), state);
        }
    }

    #[async_trait]
    impl HealthProbe for Cluster {
        async fn probe_primary(&self, node: &Node) -> Result<PrimaryStatus, ProbeError> {
            match self.nodes.lock().unwrap().get(&node.name).cloned() {
                Some(NodeState::Primary { lsn, peers }) => Ok(PrimaryStatus {
                    in_recovery: false,
                    current_lsn: lsn,
                    peers,
                }),
                Some(NodeState::Replica(s)) => Ok(PrimaryStatus {
                    in_recovery: true,
                    current_lsn: s.replay_lsn.unwrap_or_default(),
                    peers: vec![],
                }),
                _ => Err(ProbeError::Unreachable {
                    node: node.name.clone(),
                    reason: "connection refused".into(),
                }),
            }
        }

        async fn probe_replica(&self, node: &Node) -> Result<ReplicaStatus, ProbeError> {
            let step = *self.replay_step.lock().unwrap();
            let mut nodes = self.nodes.lock().unwrap();
            match nodes.get_mut(&node.name) {
                Some(NodeState::Replica(s)) => {
                    let status = s.clone();
                    if let (Some(recv), Some(replay)) = (s.receive_lsn, s.replay_lsn) {
                        s.replay_lsn = Some(Lsn((replay.0 + step).min(recv.0)));
                    }
                    Ok(status)
                }
                Some(NodeState::Primary { lsn, .. }) => Ok(ReplicaStatus {
                    in_recovery: false,
                    receive_lsn: None,
                    replay_lsn: Some(*lsn),
                    replay_delay: None,
                    wal_receiver_streaming: false,
                    upstream_silence: None,
                }),
                _ => Err(ProbeError::Unreachable {
                    node: node.name.clone(),
                    reason: "connection refused".into(),
                }),
            }
        }
    }

    #[async_trait]
    impl Promoter for Cluster {
        async fn promote(&self, node: &Node, _wait: Duration) -> Result<(), PromoteError> {
            let delay = *self.promote_delay.lock().unwrap();
            sleep(delay).await;
            let mut nodes = self.nodes.lock().unwrap();
            let lsn = match nodes.get(&node.name) {
                Some(NodeState::Replica(s)) => s.replay_lsn.unwrap_or_default(),
                _ => {
                    return Err(PromoteError::Failed {
                        node: node.name.clone(),
                        reason: "not a standby".into(),
                    })
                }
            };
            nodes.insert(node.name.clone(), NodeState::Primary { lsn, peers: vec![] });
            self.promoted.lock().unwrap().push(node.name.clone());
            Ok(())
        }

        async fn fence(&self, node: &Node) -> Result<(), PromoteError> {
            self.fenced.lock().unwrap().push(node.name.clone());
            Ok(())
        }
    }

    #[async_trait]
    impl DnsUpdater for Cluster {
        async fn point_to(&self, target: &Node) -> Result<(), DnsError> {
            let mut left = self.dns_failures_left.lock().unwrap();
            if *left > 0 {
                *left -= 1;
                return Err(DnsError::Request("503 from provider".into()));
            }
            self.dns_targets
                .lock()
                .unwrap()
                .push(target.endpoint.clone());
            Ok(())
        }
    }

    fn node(name: &str, region: &str) -> Node {
        Node {
            name: name.into(),
            region: region.into(),
            endpoint: format!("{name}.db.internal"),
        }
    }

    fn standby(receive: u64, replay: u64, streaming: bool) -> ReplicaStatus {
        ReplicaStatus {
            in_recovery: true,
            receive_lsn: Some(Lsn(receive)),
            replay_lsn: Some(Lsn(replay)),
            replay_delay: Some(Duration::from_millis(40)),
            wal_receiver_streaming: streaming,
            upstream_silence: Some(if streaming {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(30)
            }),
        }
    }

    fn peer(name: &str, state: SyncState, replay: u64) -> ReplicationPeer {
        ReplicationPeer {
            application_name: name.into(),
            sync_state: state,
            replay_lsn: Some(Lsn(replay)),
        }
    }

    /// us-east-1 primary with a sync standby in eu-west-1 and an async one in
    /// us-west-2, all caught up at LSN 1000.
    fn healthy_cluster() -> (Arc<Cluster>, FailoverController) {
        let cluster = Arc::new(Cluster::default());
        *cluster.replay_step.lock().unwrap() = 100;
        cluster.set(
            "use1",
            NodeState::Primary {
                lsn: Lsn(1000),
                peers: vec![
                    peer("euw1", SyncState::Sync, 1000),
                    peer("usw2", SyncState::Async, 1000),
                ],
            },
        );
        cluster.set("euw1", NodeState::Replica(standby(1000, 1000, true)));
        cluster.set("usw2", NodeState::Replica(standby(1000, 1000, true)));
        let topology = Topology {
            primary: node("use1", "us-east-1"),
            replicas: vec![node("usw2", "us-west-2"), node("euw1", "eu-west-1")],
            fenced: vec![],
        };
        let controller = FailoverController::new(
            topology,
            cluster.clone(),
            cluster.clone(),
            cluster.clone(),
            FailoverPolicy::default(),
        );
        (cluster, controller)
    }

    /// Simulate the primary region going dark: primary unreachable, standbys
    /// stop streaming.
    fn kill_primary(cluster: &Cluster, euw1: ReplicaStatus, usw2: ReplicaStatus) {
        cluster.set("use1", NodeState::Down);
        cluster.set("euw1", NodeState::Replica(euw1));
        cluster.set("usw2", NodeState::Replica(usw2));
    }

    async fn tick_until_outcome(controller: &mut FailoverController) -> TickOutcome {
        loop {
            match controller.tick().await {
                TickOutcome::Degraded { .. } => continue,
                other => return other,
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn healthy_primary_reports_lag_and_never_fails_over() {
        let (cluster, mut controller) = healthy_cluster();
        *cluster.replay_step.lock().unwrap() = 0;
        cluster.set("usw2", NodeState::Replica(standby(1000, 600, true)));
        for _ in 0..10 {
            match controller.tick().await {
                TickOutcome::Healthy { lag } => {
                    let usw2 = lag.iter().find(|l| l.node == "usw2").unwrap();
                    assert_eq!(usw2.bytes_behind, Some(400));
                }
                other => panic!("unexpected outcome {other:?}"),
            }
        }
        assert!(cluster.promoted.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn transient_failures_below_threshold_do_not_fail_over() {
        let (cluster, mut controller) = healthy_cluster();
        cluster.set("use1", NodeState::Down);
        assert!(matches!(
            controller.tick().await,
            TickOutcome::Degraded {
                consecutive_failures: 1,
                ..
            }
        ));
        assert!(matches!(
            controller.tick().await,
            TickOutcome::Degraded {
                consecutive_failures: 2,
                ..
            }
        ));
        cluster.set(
            "use1",
            NodeState::Primary {
                lsn: Lsn(1000),
                peers: vec![],
            },
        );
        assert!(matches!(
            controller.tick().await,
            TickOutcome::Healthy { .. }
        ));
        cluster.set("use1", NodeState::Down);
        assert!(matches!(
            controller.tick().await,
            TickOutcome::Degraded {
                consecutive_failures: 1,
                ..
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn promotes_sync_standby_and_reroutes_dns_within_deadline() {
        let (cluster, mut controller) = healthy_cluster();
        controller.tick().await;
        // eu-west-1 received WAL up to 1400 but only replayed 1000.
        kill_primary(
            &cluster,
            standby(1400, 1000, false),
            standby(1200, 1200, false),
        );

        let report = match tick_until_outcome(&mut controller).await {
            TickOutcome::FailedOver(report) => report,
            other => panic!("expected failover, got {other:?}"),
        };
        assert_eq!(report.old_primary.name, "use1");
        assert_eq!(report.new_primary.name, "euw1");
        assert_eq!(report.high_water_lsn, Lsn(1400));
        assert_eq!(
            report.promoted_at_lsn,
            Lsn(1400),
            "must replay all received WAL first"
        );
        assert!(report.time_to_promote <= Duration::from_secs(30));
        assert!(report.dns_updated);
        assert_eq!(
            *cluster.dns_targets.lock().unwrap(),
            vec!["euw1.db.internal"]
        );
        assert_eq!(*cluster.fenced.lock().unwrap(), vec!["use1"]);

        let topology = controller.topology();
        assert_eq!(topology.primary.name, "euw1");
        assert_eq!(topology.replicas, vec![node("usw2", "us-west-2")]);
        assert_eq!(topology.fenced, vec![node("use1", "us-east-1")]);
        assert!(matches!(
            controller.tick().await,
            TickOutcome::Healthy { .. }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn partitioned_controller_does_not_fail_over() {
        let (cluster, mut controller) = healthy_cluster();
        controller.tick().await;
        // Controller cannot reach the primary, but the standbys still stream.
        cluster.set("use1", NodeState::Down);

        match tick_until_outcome(&mut controller).await {
            TickOutcome::FailoverAborted(FailoverError::PrimaryStillReachable { witnesses }) => {
                assert_eq!(witnesses.len(), 2);
            }
            other => panic!("expected abort, got {other:?}"),
        }
        assert!(cluster.promoted.lock().unwrap().is_empty());
        assert!(cluster.dns_targets.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn refuses_async_only_candidates_by_default() {
        let (cluster, mut controller) = healthy_cluster();
        controller.tick().await;
        kill_primary(
            &cluster,
            standby(1000, 1000, false),
            standby(1000, 1000, false),
        );
        cluster.set("euw1", NodeState::Down);
        // One of two standbys reachable is not a majority; relax the quorum to
        // isolate the synchronous-candidate rule.
        controller.policy.require_witness_quorum = false;

        assert!(matches!(
            tick_until_outcome(&mut controller).await,
            TickOutcome::FailoverAborted(FailoverError::NoSynchronousStandby)
        ));
        assert!(cluster.promoted.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn refuses_candidate_missing_wal_seen_elsewhere() {
        let (cluster, mut controller) = healthy_cluster();
        controller.tick().await;
        controller.policy.require_synchronous_candidate = false;
        // Both standbys are behind the last primary LSN observed (1000).
        kill_primary(&cluster, standby(900, 900, false), standby(950, 950, false));

        match tick_until_outcome(&mut controller).await {
            TickOutcome::FailoverAborted(FailoverError::DataLossRisk {
                candidate,
                high_water,
                ..
            }) => {
                // The synchronous standby ranks first even when it is behind.
                assert_eq!(candidate, "euw1");
                assert_eq!(high_water, Lsn(1000));
            }
            other => panic!("expected data-loss abort, got {other:?}"),
        }
        assert!(cluster.promoted.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn async_failover_picks_standby_with_most_wal_when_allowed() {
        let (cluster, mut controller) = healthy_cluster();
        controller.tick().await;
        controller.policy.require_synchronous_candidate = false;
        controller.policy.require_witness_quorum = false;
        kill_primary(&cluster, standby(0, 0, false), standby(1000, 1000, false));
        cluster.set("euw1", NodeState::Down);

        match tick_until_outcome(&mut controller).await {
            TickOutcome::FailedOver(report) => assert_eq!(report.new_primary.name, "usw2"),
            other => panic!("expected failover, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn aborts_when_replay_cannot_finish_before_deadline() {
        let (cluster, mut controller) = healthy_cluster();
        controller.tick().await;
        *cluster.replay_step.lock().unwrap() = 0; // replay is stuck
        kill_primary(
            &cluster,
            standby(1400, 1000, false),
            standby(1000, 1000, false),
        );

        let started = Instant::now();
        match tick_until_outcome(&mut controller).await {
            TickOutcome::FailoverAborted(FailoverError::CatchUpTimeout { candidate, target }) => {
                assert_eq!(candidate, "euw1");
                assert_eq!(target, Lsn(1400));
            }
            other => panic!("expected catch-up timeout, got {other:?}"),
        }
        assert!(started.elapsed() <= Duration::from_secs(31));
        assert!(cluster.promoted.lock().unwrap().is_empty());
        assert_eq!(controller.topology().primary.name, "use1");
    }

    #[tokio::test(start_paused = true)]
    async fn slow_promotion_breaches_deadline() {
        let (cluster, mut controller) = healthy_cluster();
        controller.tick().await;
        *cluster.promote_delay.lock().unwrap() = Duration::from_secs(45);
        kill_primary(
            &cluster,
            standby(1000, 1000, false),
            standby(1000, 1000, false),
        );

        assert!(matches!(
            tick_until_outcome(&mut controller).await,
            TickOutcome::FailoverAborted(FailoverError::DeadlineExceeded { stage: "promotion" })
        ));
        assert!(cluster.dns_targets.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn dns_failure_keeps_promotion_and_retries_next_heartbeat() {
        let (cluster, mut controller) = healthy_cluster();
        controller.tick().await;
        *cluster.dns_failures_left.lock().unwrap() = 4; // more than one round of attempts
        kill_primary(
            &cluster,
            standby(1000, 1000, false),
            standby(1000, 1000, false),
        );

        match tick_until_outcome(&mut controller).await {
            TickOutcome::FailedOver(report) => {
                assert!(!report.dns_updated);
                assert_eq!(report.time_to_reroute, None);
            }
            other => panic!("expected failover, got {other:?}"),
        }
        assert_eq!(controller.topology().primary.name, "euw1");
        assert!(cluster.dns_targets.lock().unwrap().is_empty());

        assert!(matches!(
            controller.tick().await,
            TickOutcome::Healthy { .. }
        ));
        assert_eq!(
            *cluster.dns_targets.lock().unwrap(),
            vec!["euw1.db.internal"]
        );
        assert!(controller.dns_pending.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn run_loop_detects_and_fails_over_then_stops_on_shutdown() {
        let (cluster, controller) = healthy_cluster();
        kill_primary(
            &cluster,
            standby(1000, 1000, false),
            standby(1000, 1000, false),
        );
        // With no prior healthy heartbeat, nothing is known to be synchronous.
        let mut controller = controller;
        controller.policy.require_synchronous_candidate = false;

        let topology = controller.run(sleep(Duration::from_secs(20))).await;
        assert_ne!(topology.primary.name, "use1");
        assert_eq!(cluster.promoted.lock().unwrap().len(), 1);
    }

    #[test]
    fn default_policy_is_valid_and_zero_loss() {
        let policy = FailoverPolicy::default();
        assert!(policy.validate().is_ok());
        assert_eq!(policy.max_data_loss_bytes, 0);
        assert_eq!(policy.promotion_deadline, Duration::from_secs(30));
        let bad = FailoverPolicy {
            failure_threshold: 0,
            ..FailoverPolicy::default()
        };
        assert!(bad.validate().is_err());
    }
}
