use curve25519_dalek::edwards::EdwardsPoint;
use curve25519_dalek::scalar::Scalar;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identifier for MPC server node (1, 2, or 3)
pub type NodeId = u8;

#[derive(Error, Debug)]
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
}

/// 2-of-3 Threshold configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThresholdConfig {
    pub threshold: usize,
    pub total_nodes: usize,
}

impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            threshold: 2,
            total_nodes: 3,
        }
    }
}

/// Private key share held by an independent node
#[derive(Clone)]
pub struct KeyShare {
    pub node_id: NodeId,
    pub secret_share: Scalar,
    pub public_key: EdwardsPoint,
    pub master_public_key: EdwardsPoint,
}

/// Round 1: Nonce commitment broadcast
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Round1Commitment {
    pub node_id: NodeId,
    pub commitment_bytes: [u8; 32],
}

/// Round 2: Partial signature response
#[derive(Clone, Debug, Serialize, Deserialize)]
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

/// Completed Ed25519 Threshold Signature
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
