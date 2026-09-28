//! Tessera infrastructure orchestrator.
//!
//! Hosts the cross-region database failover controller. The controller logic
//! in [`failover`] is written against three small traits (health probing,
//! promotion, DNS) so that it can be exercised deterministically in tests and
//! backed by real PostgreSQL ([`postgres`]) and DNS providers ([`dns`]) in
//! production.

pub mod config;
pub mod dns;
pub mod failover;
pub mod postgres;
pub mod types;

pub use failover::{
    DnsUpdater, FailoverController, FailoverError, FailoverPolicy, FailoverReport, HealthProbe,
    Promoter, TickOutcome, Topology,
};
pub use types::*;
