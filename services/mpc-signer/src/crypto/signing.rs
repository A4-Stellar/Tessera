use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use sha2::{Digest, Sha512};
use rand_core::OsRng;

use crate::crypto::lagrange::compute_lagrange_coefficient;
use crate::types::{KeyShare, MpcError, NodeId, Round1Commitment, Round2Response, ThresholdSignature};

/// Computes the RFC 8032 Ed25519 challenge scalar:
/// c = SHA-512(R_bytes || Y_bytes || message) mod l
pub fn compute_challenge(r_bytes: &[u8; 32], y_bytes: &[u8; 32], message: &[u8]) -> Scalar {
    let mut hasher = Sha512::new();
    hasher.update(r_bytes);
    hasher.update(y_bytes);
    hasher.update(message);
    let hash = hasher.finalize();

    let mut wide = [0u8; 64];
    wide.copy_from_slice(&hash);
    Scalar::from_bytes_mod_order_wide(&wide)
}

/// Ephemeral state kept by a node between Round 1 and Round 2
pub struct NodeRoundState {
    pub node_id: NodeId,
    pub ephemeral_secret: Scalar,
    pub ephemeral_commitment: EdwardsPoint,
}

impl NodeRoundState {
    /// Round 1: Generate ephemeral secret k_i and commitment R_i = k_i * G
    pub fn round1(node_id: NodeId) -> (Self, Round1Commitment) {
        let mut rng = OsRng;
        let ephemeral_secret = Scalar::random(&mut rng);
        let ephemeral_commitment = &ephemeral_secret * &ED25519_BASEPOINT_POINT;

        let commitment_bytes = ephemeral_commitment.compress().to_bytes();
        let state = Self {
            node_id,
            ephemeral_secret,
            ephemeral_commitment,
        };

        (state, Round1Commitment { node_id, commitment_bytes })
    }

    /// Round 2: Given all commitments, compute partial signature:
    /// s_i = k_i + c * lambda_i * x_i mod l
    pub fn round2(
        self,
        share: &KeyShare,
        participating_nodes: &[NodeId],
        commitments: &[Round1Commitment],
        message: &[u8],
    ) -> Result<Round2Response, MpcError> {
        if participating_nodes.len() < 2 {
            return Err(MpcError::InsufficientSigners {
                threshold: 2,
                actual: participating_nodes.len(),
            });
        }

        // Aggregate R = \sum R_j
        let mut group_r = EdwardsPoint::default();
        for comm in commitments {
            let point = CompressedEdwardsY(comm.commitment_bytes)
                .decompress()
                .ok_or_else(|| MpcError::RoundFailure("Failed to decompress commitment".to_string()))?;
            group_r += point;
        }

        let r_bytes = group_r.compress().to_bytes();
        let y_bytes = share.master_public_key.compress().to_bytes();

        // Challenge c = H(R || Y || m) mod l
        let challenge = compute_challenge(&r_bytes, &y_bytes, message);

        // Lagrange coefficient lambda_i for this participant
        let lambda_i = compute_lagrange_coefficient(self.node_id, participating_nodes);

        // s_i = k_i + c * lambda_i * x_i mod l
        let s_i = self.ephemeral_secret + challenge * lambda_i * share.secret_share;

        Ok(Round2Response {
            node_id: self.node_id,
            response_bytes: s_i.to_bytes(),
        })
    }
}

/// Aggregate partial responses into a final Ed25519 threshold signature (R, s)
pub fn aggregate_signature(
    commitments: &[Round1Commitment],
    responses: &[Round2Response],
    master_public_key: &EdwardsPoint,
    message: &[u8],
) -> Result<ThresholdSignature, MpcError> {
    if responses.len() < 2 {
        return Err(MpcError::InsufficientSigners {
            threshold: 2,
            actual: responses.len(),
        });
    }

    // Sum R = \sum R_i
    let mut group_r = EdwardsPoint::default();
    for comm in commitments {
        let point = CompressedEdwardsY(comm.commitment_bytes)
            .decompress()
            .ok_or_else(|| MpcError::RoundFailure("Invalid commitment in aggregation".to_string()))?;
        group_r += point;
    }

    let r_bytes = group_r.compress().to_bytes();
    let y_bytes = master_public_key.compress().to_bytes();

    // Sum s = \sum s_i mod l
    let mut group_s = Scalar::ZERO;
    for resp in responses {
        let opt_s: Option<Scalar> = Option::from(Scalar::from_canonical_bytes(resp.response_bytes));
        let s_i = opt_s.ok_or_else(|| MpcError::InvalidPartialSignature(resp.node_id))?;
        group_s += s_i;
    }

    let signature = ThresholdSignature {
        r_bytes,
        s_bytes: group_s.to_bytes(),
        master_public_key_bytes: y_bytes,
    };

    // Verify equation s * G == R + c * Y
    verify_threshold_signature(&signature, message)?;

    Ok(signature)
}

/// Verifies that (R, s) satisfies standard Ed25519: s * G == R + c * Y
pub fn verify_threshold_signature(sig: &ThresholdSignature, message: &[u8]) -> Result<(), MpcError> {
    let r_point = CompressedEdwardsY(sig.r_bytes)
        .decompress()
        .ok_or_else(|| MpcError::RoundFailure("Decompressing R failed".to_string()))?;

    let y_point = CompressedEdwardsY(sig.master_public_key_bytes)
        .decompress()
        .ok_or_else(|| MpcError::RoundFailure("Decompressing Y failed".to_string()))?;

    let opt_s: Option<Scalar> = Option::from(Scalar::from_canonical_bytes(sig.s_bytes));
    let s_scalar = opt_s.ok_or_else(|| MpcError::RoundFailure("Invalid canonical scalar s".to_string()))?;

    let challenge = compute_challenge(&sig.r_bytes, &sig.master_public_key_bytes, message);

    // LHS = s * G
    let lhs = &s_scalar * &ED25519_BASEPOINT_POINT;
    // RHS = R + c * Y
    let rhs = r_point + (challenge * y_point);

    if lhs == rhs {
        Ok(())
    } else {
        Err(MpcError::RoundFailure("Signature equation verification failed: s*G != R + c*Y".to_string()))
    }
}
