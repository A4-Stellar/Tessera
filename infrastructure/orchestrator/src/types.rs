use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// PostgreSQL write-ahead log position (`pg_lsn`), stored as the raw 64-bit
/// byte offset so that positions can be compared and subtracted directly.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Lsn(pub u64);

impl Lsn {
    /// Number of WAL bytes `self` is behind `ahead` (zero when not behind).
    pub fn bytes_behind(self, ahead: Lsn) -> u64 {
        ahead.0.saturating_sub(self.0)
    }
}

impl fmt::Display for Lsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:X}/{:X}", self.0 >> 32, self.0 & 0xFFFF_FFFF)
    }
}

impl FromStr for Lsn {
    type Err = LsnParseError;

    /// Parses the textual `pg_lsn` form, e.g. `16/B374D848`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (hi, lo) = s
            .trim()
            .split_once('/')
            .ok_or_else(|| LsnParseError(s.to_string()))?;
        let hi = u32::from_str_radix(hi, 16).map_err(|_| LsnParseError(s.to_string()))?;
        let lo = u32::from_str_radix(lo, 16).map_err(|_| LsnParseError(s.to_string()))?;
        Ok(Lsn((u64::from(hi) << 32) | u64::from(lo)))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid pg_lsn value: {0:?}")]
pub struct LsnParseError(pub String);

/// A PostgreSQL instance managed by the controller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Unique name. For replicas it must match the `application_name` the
    /// standby uses in `primary_conninfo`, so the primary's
    /// `pg_stat_replication` rows can be attributed to it.
    pub name: String,
    /// Cloud region, e.g. `us-east-1` or `eu-west-1`.
    pub region: String,
    /// Hostname the public database DNS record points at when this node is
    /// the primary writer.
    pub endpoint: String,
}

/// Replication mode of a standby as reported by the primary.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncState {
    Async,
    Potential,
    Sync,
    Quorum,
}

impl SyncState {
    /// Whether commits on the primary wait for this standby.
    pub fn is_synchronous(self) -> bool {
        matches!(self, SyncState::Sync | SyncState::Quorum)
    }
}

impl FromStr for SyncState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "async" => Ok(SyncState::Async),
            "potential" => Ok(SyncState::Potential),
            "sync" => Ok(SyncState::Sync),
            "quorum" => Ok(SyncState::Quorum),
            other => Err(format!("unknown sync_state {other:?}")),
        }
    }
}

/// One row of the primary's `pg_stat_replication`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReplicationPeer {
    pub application_name: String,
    pub sync_state: SyncState,
    pub replay_lsn: Option<Lsn>,
}

/// Heartbeat result for the current primary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PrimaryStatus {
    /// `pg_is_in_recovery()`. A primary that reports `true` is not writable.
    pub in_recovery: bool,
    /// `pg_current_wal_lsn()`: everything up to here may have been committed.
    pub current_lsn: Lsn,
    pub peers: Vec<ReplicationPeer>,
}

/// Heartbeat result for a streaming standby.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReplicaStatus {
    pub in_recovery: bool,
    /// `pg_last_wal_receive_lsn()`: WAL durably received from the primary.
    pub receive_lsn: Option<Lsn>,
    /// `pg_last_wal_replay_lsn()`: WAL applied and visible on this standby.
    pub replay_lsn: Option<Lsn>,
    /// Time since the last replayed transaction committed on the primary.
    pub replay_delay: Option<Duration>,
    /// `pg_stat_wal_receiver.status = 'streaming'`.
    pub wal_receiver_streaming: bool,
    /// Time since the WAL receiver last heard from the primary.
    pub upstream_silence: Option<Duration>,
}

impl ReplicaStatus {
    /// Highest WAL position this standby holds, applied or not.
    pub fn durable_lsn(&self) -> Option<Lsn> {
        self.receive_lsn.max(self.replay_lsn)
    }

    /// Whether this standby still sees a live upstream primary. Used as a
    /// witness vote: a controller that cannot reach the primary while its
    /// standbys still can is partitioned, and must not fail over.
    pub fn sees_live_upstream(&self, silence_threshold: Duration) -> bool {
        self.wal_receiver_streaming
            && self
                .upstream_silence
                .is_some_and(|silence| silence < silence_threshold)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProbeError {
    #[error("probe of {node} timed out")]
    Timeout { node: String },
    #[error("probe of {node} failed: {reason}")]
    Unreachable { node: String, reason: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PromoteError {
    #[error("promotion of {node} failed: {reason}")]
    Failed { node: String, reason: String },
    #[error("fencing of {node} failed: {reason}")]
    FenceFailed { node: String, reason: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DnsError {
    #[error("DNS provider request failed: {0}")]
    Request(String),
    #[error("DNS provider rejected the change: {0}")]
    Rejected(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsn_round_trips_through_text() {
        let lsn: Lsn = "16/B374D848".parse().unwrap();
        assert_eq!(lsn.0, (0x16 << 32) | 0xB374_D848);
        assert_eq!(lsn.to_string(), "16/B374D848");
        assert_eq!("0/0".parse::<Lsn>().unwrap(), Lsn(0));
    }

    #[test]
    fn lsn_rejects_malformed_input() {
        assert!("16B374D848".parse::<Lsn>().is_err());
        assert!("zz/1".parse::<Lsn>().is_err());
        assert!("1/100000000".parse::<Lsn>().is_err());
    }

    #[test]
    fn lsn_distance_saturates() {
        assert_eq!(Lsn(10).bytes_behind(Lsn(25)), 15);
        assert_eq!(Lsn(25).bytes_behind(Lsn(10)), 0);
    }

    #[test]
    fn witness_vote_requires_recent_streaming() {
        let mut status = ReplicaStatus {
            in_recovery: true,
            receive_lsn: Some(Lsn(1)),
            replay_lsn: Some(Lsn(1)),
            replay_delay: None,
            wal_receiver_streaming: true,
            upstream_silence: Some(Duration::from_secs(1)),
        };
        assert!(status.sees_live_upstream(Duration::from_secs(5)));
        status.upstream_silence = Some(Duration::from_secs(9));
        assert!(!status.sees_live_upstream(Duration::from_secs(5)));
        status.upstream_silence = Some(Duration::from_secs(1));
        status.wal_receiver_streaming = false;
        assert!(!status.sees_live_upstream(Duration::from_secs(5)));
    }
}
