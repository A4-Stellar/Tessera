use std::sync::Arc;
use curve25519_dalek::edwards::EdwardsPoint;

use crate::crypto::signing::aggregate_signature;
use crate::network::MpcNetwork;
use crate::types::{KeyShare, MpcError, SignRequest, ThresholdConfig, ThresholdSignature};

/// FROST MPC Signer Coordinator orchestrating multi-party signing rounds.
pub struct MpcCoordinator {
    config: ThresholdConfig,
    network: Arc<MpcNetwork>,
    master_public_key: EdwardsPoint,
    key_shares: Vec<KeyShare>,
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
            key_shares: Vec::new(),
        }
    }

    pub fn with_key_shares(
        config: ThresholdConfig,
        network: Arc<MpcNetwork>,
        master_public_key: EdwardsPoint,
        key_shares: Vec<KeyShare>,
    ) -> Self {
        Self {
            config,
            network,
            master_public_key,
            key_shares,
        }
    }

    pub fn register_key_share(&mut self, share: KeyShare) {
        self.key_shares.retain(|s| s.node_id != share.node_id);
        self.key_shares.push(share);
    }

    /// Execute a t-of-n distributed FROST signing session across independent server nodes.
    pub async fn sign_transaction(
        &self,
        request: SignRequest,
    ) -> Result<ThresholdSignature, MpcError> {
        // Enforce threshold invariant: at least t signers required
        if request.participating_nodes.len() < self.config.threshold {
            return Err(MpcError::InsufficientSigners {
                threshold: self.config.threshold,
                actual: request.participating_nodes.len(),
            });
        }

        // Protocol Round 1: Nonce commitment exchange with deterministic re-transmission
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

        // Signature Aggregation & Byzantine Fault Detection
        let signature = aggregate_signature(
            &commitments,
            &responses,
            &self.key_shares,
            &self.master_public_key,
            &request.transaction_payload,
        )?;

        Ok(signature)
    }

    pub fn master_public_key_bytes(&self) -> [u8; 32] {
        self.master_public_key.compress().to_bytes()
    }
}
