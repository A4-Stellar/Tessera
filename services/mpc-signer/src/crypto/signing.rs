use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use rand_core::OsRng;
use sha2::{Digest, Sha512};

use crate::crypto::lagrange::compute_lagrange_coefficient;
use crate::types::{
    KeyShare, MpcError, NodeId, Round1Commitment, Round2Response, ThresholdSignature,
};

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

/// Computes the FROST binding factor rho_i:
/// rho_i = SHA-512("FROST-ED25519-RHO" || node_id || message || all_commitments) mod l
pub fn compute_binding_factor(
    node_id: NodeId,
    message: &[u8],
    commitments: &[Round1Commitment],
) -> Scalar {
    let mut hasher = Sha512::new();
    hasher.update(b"FROST-ED25519-RHO-V1");
    hasher.update(&[node_id]);
    hasher.update(message);

    for c in commitments {
        hasher.update(&[c.node_id]);
        hasher.update(&c.hiding_commitment);
        hasher.update(&c.binding_commitment);
    }

    let hash = hasher.finalize();
    let mut wide = [0u8; 64];
    wide.copy_from_slice(&hash);
    Scalar::from_bytes_mod_order_wide(&wide)
}

/// Computes the aggregate group commitment point R and per-signer binding factors:
/// R = \sum_{j \in S} (D_j + rho_j * E_j)
pub fn compute_group_commitment(
    message: &[u8],
    commitments: &[Round1Commitment],
) -> Result<(EdwardsPoint, Vec<Scalar>), MpcError> {
    let mut rhos = Vec::with_capacity(commitments.len());
    let mut group_r = EdwardsPoint::default();

    for comm in commitments {
        let d_point = CompressedEdwardsY(comm.hiding_commitment)
            .decompress()
            .ok_or_else(|| {
                MpcError::RoundFailure(format!(
                    "Failed to decompress hiding commitment from node {}",
                    comm.node_id
                ))
            })?;
        let e_point = CompressedEdwardsY(comm.binding_commitment)
            .decompress()
            .ok_or_else(|| {
                MpcError::RoundFailure(format!(
                    "Failed to decompress binding commitment from node {}",
                    comm.node_id
                ))
            })?;

        let rho = compute_binding_factor(comm.node_id, message, commitments);
        rhos.push(rho);

        group_r += d_point + (&e_point * &rho);
    }

    Ok((group_r, rhos))
}

/// Ephemeral state kept by a node between FROST Round 1 and Round 2
pub struct NodeRoundState {
    pub node_id: NodeId,
    pub hiding_secret: Scalar,
    pub binding_secret: Scalar,
    pub hiding_commitment: EdwardsPoint,
    pub binding_commitment: EdwardsPoint,
}

impl NodeRoundState {
    /// FROST Round 1: Generate ephemeral secrets (d_i, e_i) and commitments (D_i, E_i)
    pub fn round1(node_id: NodeId) -> (Self, Round1Commitment) {
        let mut rng = OsRng;
        let hiding_secret = Scalar::random(&mut rng);
        let binding_secret = Scalar::random(&mut rng);

        let hiding_commitment = &hiding_secret * &ED25519_BASEPOINT_POINT;
        let binding_commitment = &binding_secret * &ED25519_BASEPOINT_POINT;

        let commitment = Round1Commitment {
            node_id,
            hiding_commitment: hiding_commitment.compress().to_bytes(),
            binding_commitment: binding_commitment.compress().to_bytes(),
        };

        let state = Self {
            node_id,
            hiding_secret,
            binding_secret,
            hiding_commitment,
            binding_commitment,
        };

        (state, commitment)
    }

    /// FROST Round 2: Compute partial signature share:
    /// z_i = d_i + (rho_i * e_i) + (lambda_i * c * x_i) mod l
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

        // 1. Group commitment R and binding factor rho_i
        let (group_r, _) = compute_group_commitment(message, commitments)?;
        let r_bytes = group_r.compress().to_bytes();
        let y_bytes = share.master_public_key.compress().to_bytes();

        let rho_i = compute_binding_factor(self.node_id, message, commitments);

        // 2. Ed25519 Challenge c = H(R || Y || m) mod l
        let challenge = compute_challenge(&r_bytes, &y_bytes, message);

        // 3. Lagrange coefficient lambda_i for this participant
        let lambda_i = compute_lagrange_coefficient(self.node_id, participating_nodes);

        // 4. z_i = d_i + (rho_i * e_i) + (lambda_i * c * x_i) mod l
        let z_i = self.hiding_secret
            + (rho_i * self.binding_secret)
            + (challenge * lambda_i * share.secret_share);

        Ok(Round2Response {
            node_id: self.node_id,
            response_bytes: z_i.to_bytes(),
        })
    }
}

/// Verifies an individual node's partial signature share against its public key share.
///
/// Equation: z_i * G == (D_i + rho_i * E_i) + (lambda_i * c) * Y_i
/// This detects Byzantine nodes sending incorrect shares prior to aggregation.
pub fn verify_signature_share(
    share: &Round2Response,
    comm: &Round1Commitment,
    public_key_share: &EdwardsPoint,
    lambda: &Scalar,
    challenge: &Scalar,
    rho: &Scalar,
) -> Result<bool, MpcError> {
    let d_point = CompressedEdwardsY(comm.hiding_commitment)
        .decompress()
        .ok_or_else(|| {
            MpcError::RoundFailure(format!(
                "Failed to decompress hiding commitment for node {}",
                comm.node_id
            ))
        })?;
    let e_point = CompressedEdwardsY(comm.binding_commitment)
        .decompress()
        .ok_or_else(|| {
            MpcError::RoundFailure(format!(
                "Failed to decompress binding commitment for node {}",
                comm.node_id
            ))
        })?;

    let z_i = Option::<Scalar>::from(Scalar::from_canonical_bytes(share.response_bytes))
        .ok_or_else(|| MpcError::InvalidPartialSignature(share.node_id))?;

    let lhs = &z_i * &ED25519_BASEPOINT_POINT;
    let rhs = (d_point + (&e_point * rho)) + (public_key_share * (lambda * challenge));

    Ok(lhs == rhs)
}

/// Aggregate partial responses into a final Ed25519 threshold signature (R, s).
///
/// Also verifies each participant share individually to catch Byzantine signers.
pub fn aggregate_signature(
    commitments: &[Round1Commitment],
    responses: &[Round2Response],
    key_shares: &[KeyShare],
    master_public_key: &EdwardsPoint,
    message: &[u8],
) -> Result<ThresholdSignature, MpcError> {
    if responses.len() < 2 {
        return Err(MpcError::InsufficientSigners {
            threshold: 2,
            actual: responses.len(),
        });
    }

    // 1. Compute group commitment R and binding factors
    let (group_r, rhos) = compute_group_commitment(message, commitments)?;
    let r_bytes = group_r.compress().to_bytes();
    let y_bytes = master_public_key.compress().to_bytes();

    // 2. Challenge c = H(R || Y || m) mod l
    let challenge = compute_challenge(&r_bytes, &y_bytes, message);

    // Extract list of participating node IDs
    let participating_nodes: Vec<NodeId> = responses.iter().map(|r| r.node_id).collect();

    // 3. Verify each share to detect any Byzantine nodes
    let mut aggregate_s = Scalar::ZERO;

    for (idx, resp) in responses.iter().enumerate() {
        let comm = commitments
            .iter()
            .find(|c| c.node_id == resp.node_id)
            .ok_or(MpcError::NodeNotFound(resp.node_id))?;

        let key_share = key_shares
            .iter()
            .find(|k| k.node_id == resp.node_id)
            .ok_or(MpcError::NodeNotFound(resp.node_id))?;

        let lambda_i = compute_lagrange_coefficient(resp.node_id, &participating_nodes);
        let rho_i = &rhos[idx];

        // Byzantine verification
        let is_valid = verify_signature_share(
            resp,
            comm,
            &key_share.public_key,
            &lambda_i,
            &challenge,
            rho_i,
        )?;

        if !is_valid {
            return Err(MpcError::InvalidPartialSignature(resp.node_id));
        }

        let z_i = Option::<Scalar>::from(Scalar::from_canonical_bytes(resp.response_bytes))
            .ok_or_else(|| MpcError::InvalidPartialSignature(resp.node_id))?;

        aggregate_s += z_i;
    }

    let signature = ThresholdSignature {
        r_bytes,
        s_bytes: aggregate_s.to_bytes(),
        master_public_key_bytes: y_bytes,
    };

    // 4. Verify global signature equation: s * G == R + c * Y
    verify_threshold_signature(&signature, message)?;

    Ok(signature)
}

/// Verifies that a completed ThresholdSignature satisfies the Ed25519 equation:
/// s * G == R + c * Y
pub fn verify_threshold_signature(
    signature: &ThresholdSignature,
    message: &[u8],
) -> Result<(), MpcError> {
    let r_point = CompressedEdwardsY(signature.r_bytes)
        .decompress()
        .ok_or_else(|| MpcError::RoundFailure("Invalid R point".to_string()))?;

    let y_point = CompressedEdwardsY(signature.master_public_key_bytes)
        .decompress()
        .ok_or_else(|| MpcError::RoundFailure("Invalid master public key".to_string()))?;

    let s = Option::<Scalar>::from(Scalar::from_canonical_bytes(signature.s_bytes))
        .ok_or_else(|| MpcError::RoundFailure("Invalid s scalar in signature".to_string()))?;

    let challenge = compute_challenge(
        &signature.r_bytes,
        &signature.master_public_key_bytes,
        message,
    );

    let lhs = &s * &ED25519_BASEPOINT_POINT;
    let rhs = r_point + (&y_point * &challenge);

    if lhs == rhs {
        Ok(())
    } else {
        Err(MpcError::RoundFailure(
            "Threshold signature equation check failed".to_string(),
        ))
    }
}
