//! Separate deployment artifact for the emergency vault. The implementation
//! remains beside the escrow logic; its constructor must not become the
//! constructor of the existing buyer/seller escrow WASM.
#![no_std]

#[path = "../../escrow/src/emergency_liquidation.rs"]
mod vault;

pub use vault::*;
