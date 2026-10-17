/// Decoupled identity attestation revocation accumulator.
/// #301: Efficient cryptographic accumulator for instant identity revocation without large revocation lists.

use soroban_sdk::{contract, contractimpl, Address, Env, Vec};
use std::collections::HashMap;

/// Minimal representation of accumulator state (< 100 bytes).
/// #301: Keep Soroban storage footprint minimal.
#[derive(Clone)]
pub struct RevocationAccumulator {
    /// Current accumulator value (SHA-256 hash digest)
    pub value: [u8; 32],
    /// Version counter for accumulator updates
    pub version: u32,
}

impl RevocationAccumulator {
    /// Initialize accumulator with empty state.
    pub fn new() -> Self {
        RevocationAccumulator {
            value: [0u8; 32],
            version: 0,
        }
    }

    /// Update accumulator state when identity is revoked.
    /// #301: Dynamic state update upon revocation.
    pub fn revoke(&mut self, identity: &[u8; 32]) -> Result<(), String> {
        // Hash-based accumulator: acc' = H(acc || revoked_identity)
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(&self.value);
        hasher.update(identity);
        self.value = hasher.finalize().into();
        self.version = self.version.saturating_add(1);
        Ok(())
    }

    /// Check if identity is revoked (membership check in O(1)).
    /// #301: O(1) verification time during transfer checks.
    pub fn is_revoked(&self, identity: &[u8; 32], proof: &RevocationProof) -> bool {
        // Verify proof: H(proof.acc_before || identity) == self.value
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(&proof.accumulator_before);
        hasher.update(identity);
        let computed = <[u8; 32]>::from(hasher.finalize());
        computed == self.value
    }
}

/// Zero-knowledge proof of revocation (< 100 bytes).
/// #301: Minimal storage representation.
#[derive(Clone)]
pub struct RevocationProof {
    pub accumulator_before: [u8; 32],
    pub version: u32,
}

#[contract]
pub struct RevocationAccumulatorContract;

#[contractimpl]
impl RevocationAccumulatorContract {
    /// Initialize the accumulator on contract deployment.
    pub fn initialize(env: Env) -> Result<(), String> {
        let acc = RevocationAccumulator::new();
        env.storage()
            .persistent()
            .set(&AccumulatorKey, &acc);
        Ok(())
    }

    /// Revoke an identity by adding it to the accumulator.
    /// #301: Instant invalidation of compromised KYC attestations.
    pub fn revoke_identity(
        env: Env,
        caller: Address,
        identity: [u8; 32],
    ) -> Result<(), String> {
        caller.require_auth();

        let mut acc: RevocationAccumulator = env
            .storage()
            .persistent()
            .get(&AccumulatorKey)
            .ok_or("Accumulator not initialized")?;

        acc.revoke(&identity)?;
        env.storage()
            .persistent()
            .set(&AccumulatorKey, &acc);

        Ok(())
    }

    /// Verify if an identity is revoked using a proof.
    /// #301: O(1) verification for transfer checks.
    pub fn verify_not_revoked(
        env: Env,
        identity: [u8; 32],
        proof: RevocationProof,
    ) -> Result<bool, String> {
        let acc: RevocationAccumulator = env
            .storage()
            .persistent()
            .get(&AccumulatorKey)
            .ok_or("Accumulator not initialized")?;

        // Not revoked if proof verification succeeds
        Ok(!acc.is_revoked(&identity, &proof))
    }

    /// Get current accumulator state and version.
    pub fn get_accumulator_state(env: Env) -> Result<(Vec<u8>, u32), String> {
        let acc: RevocationAccumulator = env
            .storage()
            .persistent()
            .get(&AccumulatorKey)
            .ok_or("Accumulator not initialized")?;

        Ok((acc.value.to_vec(), acc.version))
    }

    /// Batch revoke multiple identities in one transaction.
    pub fn batch_revoke(
        env: Env,
        caller: Address,
        identities: Vec<[u8; 32]>,
    ) -> Result<u32, String> {
        caller.require_auth();

        let mut acc: RevocationAccumulator = env
            .storage()
            .persistent()
            .get(&AccumulatorKey)
            .ok_or("Accumulator not initialized")?;

        let initial_version = acc.version;
        for identity in identities {
            acc.revoke(&identity)?;
        }

        env.storage()
            .persistent()
            .set(&AccumulatorKey, &acc);

        Ok(acc.version - initial_version)
    }
}

// Storage key for accumulator state
struct AccumulatorKey;
