//! PostgreSQL access: primary / read-replica routing (issue #95).

mod router;
pub mod sharding;

pub use router::DbRouter;
