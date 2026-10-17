//! PostgreSQL access: primary / read-replica routing (issue #95).

pub mod replica_balancer;
mod router;
pub mod sharding;

pub use router::DbRouter;
