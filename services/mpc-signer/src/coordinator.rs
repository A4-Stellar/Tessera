use std::sync::Arc;
use curve25519_dalek::edwards::EdwardsPoint;

use crate::crypto::signing::aggregate_signature;
use crate::network::MpcNetwork;
use crate::types::{MpcError, SignRequest, ThresholdConfig, ThresholdSignature};

/// 2-of-3 MPC Signer Coordinator orchestrating multi-party rounds
pub struct MpcCoordinator {
    config: ThresholdConfig,
    network: Arc<MpcNetwork>,
    master_public_key: EdwardsPoint,
}

impl MpcCoordinator {
    pub fn new(
        config: ThresholdConfig,
        network: Arc<MpcNetwork>,
        master_public_key: EdwardsPoint,
    ) -> Self {
        Self {
            config,
            network,
            master_public_key,
        }
    }

    /// Execute a 2-of-3 distributed signing session across independent server nodes
    pub async fn sign_transaction(
        &self,
        request: SignRequest,
    ) -> Result<ThresholdSignature, MpcError> {
        // Enforce threshold invariant: at least 2 signers required
        if request.participating_nodes.len() < self.config.threshold {
            return Err(MpcError::InsufficientSigners {
                threshold: self.config.threshold,
                actual: request.participating_nodes.len(),
            });
        }

        // Protocol Round 1: Nonce commitment exchange
        let commitments = self
            .network
            .broadcast_round1(&request.session_id, &request.participating_nodes)
            .await?;

        // Protocol Round 2: Partial signature share computation
        let responses = self
            .network
            .broadcast_round2(
                &request.session_id,
                &request.participating_nodes,
                &commitments,
                &request.transaction_payload,
            )
            .await?;

        // Signature Aggregation & Invariant Verification
        let signature = aggregate_signature(
            &commitments,
            &responses,
            &self.master_public_key,
            &request.transaction_payload,
        )?;

        Ok(signature)
    }

    pub fn master_public_key_bytes(&self) -> [u8; 32] {
        self.master_public_key.compress().to_bytes()
    }
}
