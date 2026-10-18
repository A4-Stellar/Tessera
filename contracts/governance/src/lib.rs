//! Tessera corporate governance contract (issue #92): board members approve
//! high-privilege operations (contract upgrades, minting, fee changes) as
//! m-of-n resolutions that execute automatically at threshold.
#![no_std]

mod resolution;
pub mod ve_vault;

pub use resolution::{
    BoardConfig, Error, ExecutionPayload, GovernanceContract, GovernanceContractClient, Resolution,
};
pub use ve_vault::{
    EarlyWithdrawn, LockCreated, LockUpdated, VeError, VeLock, VeVaultContract,
    VeVaultContractClient, Withdrawn, BPS_SCALE, DEFAULT_MAX_PENALTY_BPS, MAX_LOCK_DURATION,
    SECONDS_PER_DAY,
};

#[cfg(test)]
mod test;
