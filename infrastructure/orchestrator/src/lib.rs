//! Tessera infrastructure orchestrator.
//!
//! Hosts the cross-region database failover controller. The controller logic
//! in [`failover`] is written against three small traits (health probing,
//! promotion, DNS) so that it can be exercised deterministically in tests and
//! backed by real PostgreSQL ([`postgres`]) and DNS providers ([`dns`]) in
//! production.
//!
//! It also hosts the automated P1 alert triage bot ([`triage`], issue #474),
//! which runs runbook steps with timeouts and rollback, reports to Slack and
//! records every action in the audit trail.

pub mod config;
pub mod dns;
pub mod failover;
pub mod postgres;
pub mod triage;
pub mod types;

pub use failover::{
    DnsUpdater, FailoverController, FailoverError, FailoverPolicy, FailoverReport, HealthProbe,
    Promoter, TickOutcome, Topology,
};
pub use types::*;
