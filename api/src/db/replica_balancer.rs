//! Latency- and lag-aware routing of read queries across read replicas.
//!
//! The balancer is pure routing logic: a health probe task records each
//! replica's measured latency and replication lag with [`ReplicaBalancer::record`],
//! and [`ReplicaBalancer::route`] picks where the next read should go.

// Not yet wired to a connection pool; kept compiling and tested.
#![allow(dead_code)]

use std::time::Duration;

/// Replicas lagging this far behind (or more) are not eligible for reads.
pub const MAX_REPLICATION_LAG: Duration = Duration::from_secs(2);
/// Replicas slower than this are treated as unhealthy.
pub const MAX_LATENCY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Us,
    Eu,
    Asia,
}

/// Last probe result for one replica.
#[derive(Debug, Clone, Copy)]
pub struct ReplicaHealth {
    pub region: Region,
    pub latency: Duration,
    pub lag: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Replica(Region),
    Primary,
}

#[derive(Debug, Default)]
pub struct ReplicaBalancer {
    replicas: Vec<ReplicaHealth>,
}

impl ReplicaBalancer {
    /// Store the latest probe for a replica, replacing any previous one.
    pub fn record(&mut self, health: ReplicaHealth) {
        match self.replicas.iter_mut().find(|r| r.region == health.region) {
            Some(slot) => *slot = health,
            None => self.replicas.push(health),
        }
    }

    /// Lowest-latency replica whose lag is under [`MAX_REPLICATION_LAG`] and
    /// latency under [`MAX_LATENCY`]; the primary when none qualifies (all
    /// lagging or slow, or none probed yet).
    pub fn route(&self) -> Route {
        self.replicas
            .iter()
            .filter(|r| r.lag < MAX_REPLICATION_LAG && r.latency < MAX_LATENCY)
            .min_by_key(|r| r.latency)
            .map_or(Route::Primary, |r| Route::Replica(r.region))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(region: Region, latency_ms: u64, lag_ms: u64) -> ReplicaHealth {
        ReplicaHealth {
            region,
            latency: Duration::from_millis(latency_ms),
            lag: Duration::from_millis(lag_ms),
        }
    }

    #[test]
    fn routes_to_lowest_latency_replica() {
        let mut b = ReplicaBalancer::default();
        b.record(health(Region::Us, 40, 100));
        b.record(health(Region::Eu, 15, 100));
        b.record(health(Region::Asia, 90, 100));
        assert_eq!(b.route(), Route::Replica(Region::Eu));
    }

    #[test]
    fn skips_replicas_lagging_two_seconds_or_more() {
        let mut b = ReplicaBalancer::default();
        b.record(health(Region::Eu, 15, 2_000));
        b.record(health(Region::Us, 40, 1_999));
        assert_eq!(b.route(), Route::Replica(Region::Us));
    }

    #[test]
    fn falls_back_to_primary_when_all_lag_or_unprobed() {
        let mut b = ReplicaBalancer::default();
        assert_eq!(b.route(), Route::Primary);
        b.record(health(Region::Us, 10, 5_000));
        b.record(health(Region::Eu, 10, 3_000));
        b.record(health(Region::Asia, 900, 10));
        assert_eq!(b.route(), Route::Primary);
    }

    #[test]
    fn newer_probe_replaces_older() {
        let mut b = ReplicaBalancer::default();
        b.record(health(Region::Us, 10, 5_000));
        b.record(health(Region::Us, 10, 100));
        assert_eq!(b.route(), Route::Replica(Region::Us));
    }
}
