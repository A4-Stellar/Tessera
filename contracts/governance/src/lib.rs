//! Tessera corporate governance contract (issue #92): board members approve
//! high-privilege operations (contract upgrades, minting, fee changes) as
//! m-of-n resolutions that execute automatically at threshold.
#![no_std]

mod resolution;

pub use resolution::{
    BoardConfig, Error, ExecutionPayload, GovernanceContract, GovernanceContractClient, Resolution,
};

#[cfg(test)]
mod test;
