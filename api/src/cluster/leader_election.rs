//! Lease-based leader election for multi-node indexer deployments (issue #159).
//!
//! Every API container serves HTTP, but only the elected leader polls Soroban
//! RPC. The other nodes stand by already queued in the election, so one of
//! them takes over as soon as the leader's lease ends.
//!
//! # Protocol (etcd v3 election API)
//!
//! 1. Grant a lease with a [`LEASE_TTL_SECS`] TTL and renew it every
//!    [`KEEPALIVE_INTERVAL`].
//! 2. `Campaign` on the election name with that lease. etcd orders candidates
//!    by the creation revision of their lease-bound key, and the call returns
//!    once this node's key is the oldest, i.e. once it is leader.
//! 3. When a leader dies its renewals stop. etcd expires the lease within its
//!    TTL plus one 500 ms expiry sweep, deletes the leader key, and the next
//!    candidate's `Campaign` returns: takeover within 2.5 s with the default
//!    etcd minimum TTL. A leader that shuts down cleanly revokes its lease,
//!    which deletes its key at once, so takeover is immediate.
//! 4. A leader that cannot renew (for example, partitioned from etcd) steps
//!    down one keep-alive interval before etcd could expire the lease, so two
//!    nodes never poll at the same time.
//!
//! etcd raises a TTL below its minimum (1.5 × the election timeout, rounded
//! up to whole seconds: 2 s with default settings) to that minimum. The step-
//! down deadline uses the TTL etcd actually granted, so step 4 holds either way.
//!
//! Consul was not used because its session TTL has a 10 s floor, which cannot
//! meet a 3 s failover.
//!
//! Without `RWA_ETCD_ENDPOINTS` the node runs standalone and is always leader.

use std::time::Duration;

use etcd_client::{Client, ConnectOptions};
use serde::Serialize;
use tokio::sync::watch;
use tokio::time::{interval, sleep, timeout, Instant, MissedTickBehavior};

/// Requested lease TTL. etcd's default minimum, so failover stays under 3 s.
pub const LEASE_TTL_SECS: i64 = 2;
const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(500);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Pause before rejoining after a session error (etcd unreachable etc.).
const RETRY_DELAY: Duration = Duration::from_secs(1);
const DEFAULT_ELECTION: &str = "tessera/indexer-leader";

/// This node's role, reported on `GET /health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeRole {
    Leader,
    Follower,
}

#[derive(Debug, Clone)]
pub struct ElectionConfig {
    endpoints: Vec<String>,
    election: String,
    node_id: String,
}

impl ElectionConfig {
    /// Read `RWA_ETCD_ENDPOINTS` (comma-separated), `RWA_ELECTION_NAME` and
    /// `RWA_NODE_ID` (default: `HOSTNAME`, the container ID under Docker and
    /// ECS). `None` when no endpoints are configured.
    pub fn from_env() -> Option<Self> {
        let endpoints: Vec<String> = std::env::var("RWA_ETCD_ENDPOINTS")
            .ok()?
            .split(',')
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())
            .map(str::to_owned)
            .collect();
        if endpoints.is_empty() {
            return None;
        }
        Some(ElectionConfig {
            endpoints,
            election: std::env::var("RWA_ELECTION_NAME")
                .unwrap_or_else(|_| DEFAULT_ELECTION.to_owned()),
            node_id: std::env::var("RWA_NODE_ID")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| format!("node-{:016x}", rand::random::<u64>())),
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum SessionError {
    #[error(transparent)]
    Etcd(#[from] etcd_client::Error),
    #[error("lease {0:x} was not renewed within its TTL")]
    LeaseLost(i64),
}

/// Join the election and return this node's live role. A standalone node
/// (`config` is `None`) is permanently leader.
pub fn spawn(
    config: Option<ElectionConfig>,
    shutdown: watch::Receiver<bool>,
) -> watch::Receiver<NodeRole> {
    let Some(config) = config else {
        return watch::channel(NodeRole::Leader).1;
    };
    let (role, role_rx) = watch::channel(NodeRole::Follower);
    tokio::spawn(run(config, role, shutdown));
    role_rx
}

async fn run(
    config: ElectionConfig,
    role: watch::Sender<NodeRole>,
    mut shutdown: watch::Receiver<bool>,
) {
    tracing::info!(node_id = %config.node_id, election = %config.election, "joining indexer leader election");
    while !*shutdown.borrow() {
        if let Err(e) = session(&config, &role, &mut shutdown).await {
            tracing::warn!(error = %e, node_id = %config.node_id, "leader election session ended; rejoining");
            tokio::select! {
                _ = sleep(RETRY_DELAY) => {}
                _ = shutdown.changed() => {}
            }
        }
    }
}

/// One lease-bound session: campaign, then hold leadership while the lease
/// is renewed. Returns `Ok` only on shutdown.
async fn session(
    config: &ElectionConfig,
    role: &watch::Sender<NodeRole>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), SessionError> {
    let options = ConnectOptions::new()
        .with_connect_timeout(CONNECT_TIMEOUT)
        .with_keep_alive(KEEPALIVE_INTERVAL, CONNECT_TIMEOUT);
    let mut client = Client::connect(&config.endpoints, Some(options)).await?;
    let lease = client.lease_grant(LEASE_TTL_SECS, None).await?;
    let ttl = Duration::from_secs(lease.ttl().max(0) as u64);

    let result = hold(&mut client, config, role, shutdown, lease.id(), ttl).await;

    // Stop polling before releasing the lease, so the successor never
    // overlaps with this node. Revoking deletes the election key at once;
    // if etcd is unreachable the lease simply expires.
    role.send_replace(NodeRole::Follower);
    let _ = timeout(CONNECT_TIMEOUT, client.lease_revoke(lease.id())).await;
    result
}

async fn hold(
    client: &mut Client,
    config: &ElectionConfig,
    role: &watch::Sender<NodeRole>,
    shutdown: &mut watch::Receiver<bool>,
    lease_id: i64,
    ttl: Duration,
) -> Result<(), SessionError> {
    let (mut keeper, mut renewals) = client.lease_keep_alive(lease_id).await?;
    let step_down_after = ttl.saturating_sub(KEEPALIVE_INTERVAL);
    let mut renewed_at = Instant::now();
    let mut ticker = interval(KEEPALIVE_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut elector = client.clone();
    let campaign = elector.campaign(config.election.as_str(), config.node_id.as_str(), lease_id);
    tokio::pin!(campaign);
    let mut elected = false;

    loop {
        tokio::select! {
            won = &mut campaign, if !elected => {
                won?;
                elected = true;
                role.send_replace(NodeRole::Leader);
                tracing::info!(node_id = %config.node_id, "elected indexer leader");
            }
            _ = ticker.tick() => {
                if renewed_at.elapsed() >= step_down_after {
                    return Err(SessionError::LeaseLost(lease_id));
                }
                keeper.keep_alive().await?;
            }
            renewal = renewals.message() => match renewal? {
                Some(response) if response.ttl() > 0 => renewed_at = Instant::now(),
                _ => return Err(SessionError::LeaseLost(lease_id)),
            },
            _ = shutdown.changed() => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn standalone_node_is_always_leader() {
        let (_shutdown_tx, shutdown) = watch::channel(false);
        assert_eq!(*spawn(None, shutdown).borrow(), NodeRole::Leader);
    }

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(serde_json::json!(NodeRole::Leader), "leader");
        assert_eq!(serde_json::json!(NodeRole::Follower), "follower");
    }

    async fn wait_for_role(role: &mut watch::Receiver<NodeRole>, want: NodeRole) {
        timeout(Duration::from_secs(10), role.wait_for(|r| *r == want))
            .await
            .expect("role change timed out")
            .expect("election task ended");
    }

    /// `RWA_TEST_ETCD_ENDPOINTS=http://127.0.0.1:2379 cargo test leader_election -- --ignored`
    #[tokio::test]
    #[ignore = "requires etcd via RWA_TEST_ETCD_ENDPOINTS"]
    async fn standby_takes_over_within_three_seconds_of_leader_failure() {
        let endpoints: Vec<String> = std::env::var("RWA_TEST_ETCD_ENDPOINTS")
            .expect("RWA_TEST_ETCD_ENDPOINTS")
            .split(',')
            .map(str::to_owned)
            .collect();
        let election = format!("tessera-test/{:016x}", rand::random::<u64>());
        let node = |id: &str| ElectionConfig {
            endpoints: endpoints.clone(),
            election: election.clone(),
            node_id: id.to_owned(),
        };
        let (a_tx, mut a) = watch::channel(NodeRole::Follower);
        let (_a_stop, a_shutdown) = watch::channel(false);
        let a_task = tokio::spawn(run(node("a"), a_tx, a_shutdown));
        wait_for_role(&mut a, NodeRole::Leader).await;

        let (b_stop, b_shutdown) = watch::channel(false);
        let mut b = spawn(Some(node("b")), b_shutdown);
        sleep(Duration::from_secs(1)).await;
        assert_eq!(*b.borrow(), NodeRole::Follower, "only one leader at a time");

        // Crash the leader: nothing is revoked, so its lease has to expire.
        a_task.abort();
        let crashed_at = Instant::now();
        wait_for_role(&mut b, NodeRole::Leader).await;
        let takeover = crashed_at.elapsed();
        assert!(
            takeover < Duration::from_secs(3),
            "crash takeover took {takeover:?}"
        );

        // A clean shutdown revokes the lease, so the standby takes over at once.
        let (c_stop, c_shutdown) = watch::channel(false);
        let mut c = spawn(Some(node("c")), c_shutdown);
        sleep(Duration::from_millis(500)).await;
        assert_eq!(*c.borrow(), NodeRole::Follower);
        b_stop.send(true).unwrap();
        let stopped_at = Instant::now();
        wait_for_role(&mut b, NodeRole::Follower).await;
        wait_for_role(&mut c, NodeRole::Leader).await;
        let handover = stopped_at.elapsed();
        assert!(
            handover < Duration::from_secs(1),
            "clean handover took {handover:?}"
        );
        c_stop.send(true).unwrap();
    }
}
