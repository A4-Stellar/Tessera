use curve25519_dalek::edwards::EdwardsPoint;
use curve25519_dalek::scalar::Scalar;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identifier for MPC server node (1..=n)
pub type NodeId = u8;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum MpcError {
    #[error("Invalid threshold: need at least {threshold} signers, got {actual}")]
    InsufficientSigners { threshold: usize, actual: usize },
    #[error("Node {0} not found or inactive")]
    NodeNotFound(NodeId),
    #[error("Invalid partial signature from node {0}")]
    InvalidPartialSignature(NodeId),
    #[error("Invalid key share")]
    InvalidKeyShare,
    #[error("Round failure: {0}")]
    RoundFailure(String),
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Serialization error: {0}")]
    SerializationError(String),
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
}

/// Configurable t-of-n Threshold configuration (default: 3-of-5).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThresholdConfig {
    pub threshold: usize,
    pub total_nodes: usize,
}

impl ThresholdConfig {
    pub fn new(threshold: usize, total_nodes: usize) -> Result<Self, MpcError> {
        if threshold < 2 {
            return Err(MpcError::InvalidConfig(
                "Threshold must be at least 2".to_string(),
            ));
        }
        if threshold > total_nodes {
            return Err(MpcError::InvalidConfig(
                "Threshold cannot exceed total node count".to_string(),
            ));
        }
        Ok(Self {
            threshold,
            total_nodes,
        })
    }
}

impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            threshold: 3,
            total_nodes: 5,
        }
    }
}

/// Private key share held by an independent node
#[derive(Clone, Debug)]
pub struct KeyShare {
    pub node_id: NodeId,
    pub secret_share: Scalar,
    pub public_key: EdwardsPoint,
    pub master_public_key: EdwardsPoint,
}

/// FROST Round 1: Nonce commitment broadcast (hiding and binding commitments)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Round1Commitment {
    pub node_id: NodeId,
    pub hiding_commitment: [u8; 32],
    pub binding_commitment: [u8; 32],
}

impl Round1Commitment {
    pub fn new(node_id: NodeId, hiding: [u8; 32], binding: [u8; 32]) -> Self {
        Self {
            node_id,
            hiding_commitment: hiding,
            binding_commitment: binding,
        }
    }

    /// Primary commitment bytes representation for backward compatibility.
    pub fn commitment_bytes(&self) -> [u8; 32] {
        self.hiding_commitment
    }
}

/// FROST Round 2: Partial signature response share z_i
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Round2Response {
    pub node_id: NodeId,
    pub response_bytes: [u8; 32],
}

/// Multi-Party Transaction Signing Request
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignRequest {
    pub session_id: String,
    pub transaction_payload: Vec<u8>,
    pub participating_nodes: Vec<NodeId>,
}

/// Completed Ed25519 Threshold Signature (R, s)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThresholdSignature {
    pub r_bytes: [u8; 32],
    pub s_bytes: [u8; 32],
    pub master_public_key_bytes: [u8; 32],
}

impl ThresholdSignature {
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut sig = [0u8; 64];
        sig[..32].copy_from_slice(&self.r_bytes);
        sig[32..].copy_from_slice(&self.s_bytes);
        sig
    }
}
