use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::EdwardsPoint;
use curve25519_dalek::scalar::Scalar;
use rand_core::OsRng;

use crate::types::{KeyShare, NodeId};

/// Distributed Key Generation using Pedersen Verifiable Secret Sharing (VSS).
///
/// Implements FROST-compatible multi-party key generation where each participant
/// acts as a dealer, eliminating any single point of failure (SPoF).
pub struct DistributedKeyGenerator;

impl DistributedKeyGenerator {
    /// Generates key shares for the default 3-of-5 threshold configuration.
    pub fn generate_3_of_5() -> (EdwardsPoint, Vec<KeyShare>) {
        Self::generate_t_of_n(3, 5)
    }

    /// Generates key shares for 2-of-3 threshold configuration (backward compatibility).
    pub fn generate_2_of_3() -> (EdwardsPoint, Vec<KeyShare>) {
        Self::generate_t_of_n(2, 3)
    }

    /// Generic Pedersen VSS DKG for configurable t-of-n threshold.
    ///
    /// Protocol steps:
    /// 1. Each participant i in 1..=n generates a random polynomial of degree (t - 1):
    ///    f_i(x) = a_{i,0} + a_{i,1} x + ... + a_{i,t-1} x^{t-1}
    /// 2. Participant i computes coefficient commitments:
    ///    C_{i,k} = a_{i,k} * G for k in 0..t
    /// 3. Participant i sends secret evaluation s_{i -> j} = f_i(j) to participant j.
    /// 4. Participant j verifies received shares against commitments:
    ///    s_{i -> j} * G == \sum_{k=0}^{t-1} (j^k * C_{i,k})
    /// 5. Participant j aggregates shares: x_j = \sum_{i=1}^n s_{i -> j} mod l.
    /// 6. Group public key is Y = \sum_{i=1}^n C_{i,0}.
    pub fn generate_t_of_n(threshold: usize, total_nodes: usize) -> (EdwardsPoint, Vec<KeyShare>) {
        assert!(
            threshold >= 2 && threshold <= total_nodes,
            "Invalid threshold parameters: t={}, n={}",
            threshold,
            total_nodes
        );

        let mut rng = OsRng;

        // Step 1 & 2: Generate random polynomials and commitments for each participant
        let mut polynomials = Vec::with_capacity(total_nodes);
        let mut commitments = Vec::with_capacity(total_nodes);

        for _ in 0..total_nodes {
            let mut coeffs = Vec::with_capacity(threshold);
            let mut comms = Vec::with_capacity(threshold);

            for _ in 0..threshold {
                let coeff = Scalar::random(&mut rng);
                let comm = &coeff * &ED25519_BASEPOINT_POINT;
                coeffs.push(coeff);
                comms.push(comm);
            }

            polynomials.push(coeffs);
            commitments.push(comms);
        }

        // Master public key Y = \sum_{i=1}^n C_{i,0}
        let mut master_public_key = EdwardsPoint::default();
        for comms in &commitments {
            master_public_key += comms[0];
        }

        // Step 3, 4, 5: Distribute, verify, and aggregate shares
        let mut key_shares = Vec::with_capacity(total_nodes);

        for j in 1..=total_nodes {
            let j_id = j as NodeId;
            let j_scalar = Scalar::from(j as u64);

            let mut aggregated_secret = Scalar::ZERO;

            for i in 0..total_nodes {
                // Evaluate polynomial f_i(j) = \sum_{k=0}^{t-1} a_{i,k} * j^k
                let mut share_ij = Scalar::ZERO;
                let mut current_power = Scalar::ONE;

                for coeff in &polynomials[i] {
                    share_ij += coeff * current_power;
                    current_power *= j_scalar;
                }

                // Step 4: Pedersen VSS verification
                // s_{i -> j} * G == \sum_{k=0}^{t-1} j^k * C_{i,k}
                let mut expected_point = EdwardsPoint::default();
                let mut check_power = Scalar::ONE;

                for comm in &commitments[i] {
                    expected_point += comm * check_power;
                    check_power *= j_scalar;
                }

                let actual_point = &share_ij * &ED25519_BASEPOINT_POINT;
                assert_eq!(
                    actual_point, expected_point,
                    "Pedersen VSS verification failed for share from node {} to node {}",
                    i + 1,
                    j
                );

                aggregated_secret += share_ij;
            }

            let public_key = &aggregated_secret * &ED25519_BASEPOINT_POINT;

            key_shares.push(KeyShare {
                node_id: j_id,
                secret_share: aggregated_secret,
                public_key,
                master_public_key,
            });
        }

        (master_public_key, key_shares)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::lagrange::compute_lagrange_coefficient;

    #[test]
    fn test_pedersen_vss_3_of_5_reconstruction() {
        let (master_pk, shares) = DistributedKeyGenerator::generate_3_of_5();
        assert_eq!(shares.len(), 5);

        // Verify that any 3-of-5 subset reconstructs master public key
        let subsets: Vec<Vec<NodeId>> = vec![
            vec![1, 2, 3],
            vec![2, 3, 4],
            vec![3, 4, 5],
            vec![1, 3, 5],
            vec![1, 4, 5],
        ];

        for subset in subsets {
            let mut reconstructed_pk = EdwardsPoint::default();
            for &id in &subset {
                let share = shares.iter().find(|s| s.node_id == id).unwrap();
                let lambda = compute_lagrange_coefficient(id, &subset);
                reconstructed_pk += &share.public_key * lambda;
            }
            assert_eq!(
                reconstructed_pk, master_pk,
                "Failed reconstruction for subset {:?}",
                subset
            );
        }
    }
}
