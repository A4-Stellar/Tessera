//! Off-Chain Decentralized Storage module (Issue #69).

pub mod ipfs;
pub mod rocksdb;

pub use ipfs::{EncryptedDocumentMetadata, IpfsClient, IpfsConfig, IpfsError};
