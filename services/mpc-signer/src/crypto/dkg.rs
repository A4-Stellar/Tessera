use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::EdwardsPoint;
use curve25519_dalek::scalar::Scalar;
use rand_core::OsRng;
use crate::types::{KeyShare, NodeId};

/// Distributes 2-of-3 threshold key shares
pub struct DistributedKeyGenerator;

impl DistributedKeyGenerator {
    /// Generates a master secret x, master public key Y = x * G, and Shamir shares for nodes 1, 2, 3
    pub fn generate_2_of_3() -> (EdwardsPoint, Vec<KeyShare>) {
        let mut rng = OsRng;
        // Master secret a0 = x
        let a0 = Scalar::random(&mut rng);
        // Linear slope a1 for threshold 2
        let a1 = Scalar::random(&mut rng);

        let master_public_key = &a0 * &ED25519_BASEPOINT_POINT;

        let mut shares = Vec::new();
        for id in 1..=3 {
            let x_id = Scalar::from(id as u64);
            let secret_share = a0 + a1 * x_id;
            let public_key = &secret_share * &ED25519_BASEPOINT_POINT;

            shares.push(KeyShare {
                node_id: id as NodeId,
                secret_share,
                public_key,
                master_public_key,
            });
        }

        (master_public_key, shares)
    }
}
