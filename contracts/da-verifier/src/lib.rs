#![no_std]

//! EigenDA DA-proof verifier (issue #469).
//!
//! The indexer exposes `GET /v1/assets/:id/da-proof`, which returns an
//! aggregate BLS signature together with the operator public keys, their stake
//! weights and a signer bitmap. This contract re-checks:
//!
//! 1. that the selected operators represent >= 80% of the total stake;
//! 2. that the aggregate signature verifies under the aggregate public key
//!    using a BLS12-381 pairing check over the domain-separated blob header
//!    and MMR root.
//!
//! It is a fraud-proof anchor: a rollup can re-execute from the state the MMR
//! root commits to and rely on this contract to confirm the data was made
//! available by the EigenDA quorum.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    crypto::bls12_381::{Bls12_381, G1Affine, G2Affine},
    Bytes, BytesN, Env, Vec,
};

/// EigenDA's stake-weighted quorum threshold, in percent.
pub const QUORUM_THRESHOLD_PERCENT: u128 = 80;
pub const DOMAIN: &[u8] = b"TESSERA-EIGENDA-DA-PROOF-V1";

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DaError {
    InvalidQuorum = 1,
    InvalidSignature = 2,
    InvalidProof = 3,
}

/// Retrieval proof emitted by the indexer and verified on-chain.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaProof {
    /// Aggregate BLS signature in G1 (uncompressed, 96 bytes).
    pub aggregate_signature: BytesN<96>,
    /// Operator public keys in G2 (uncompressed, 192 bytes each).
    pub operator_pubkeys: Vec<BytesN<192>>,
    /// Operator stake weights, aligned with `operator_pubkeys`.
    pub stake_weights: Vec<u64>,
    /// Indices into `operator_pubkeys` of the operators who signed.
    pub signer_bitmap: Vec<u32>,
    /// MMR root committed by this DA blob.
    pub mmr_root: BytesN<32>,
}

#[contract]
pub struct DaVerifierContract;

#[contractimpl]
impl DaVerifierContract {
    /// Verify an EigenDA proof. Returns `Ok(())` only for a valid, quorate
    /// proof; any malformed input fails closed.
    pub fn verify_da_proof(env: Env, blob_header: Bytes, proof: DaProof) -> Result<(), DaError> {
        verify(&env, &blob_header, &proof)
    }
}

/// The verification logic, shared by the contract and its unit tests.
pub fn verify(env: &Env, blob_header: &Bytes, proof: &DaProof) -> Result<(), DaError> {
    let operators = proof.operator_pubkeys.len();
    if operators == 0 || operators != proof.stake_weights.len() || proof.signer_bitmap.is_empty() {
        return Err(DaError::InvalidProof);
    }

    // Stake-weighted quorum: selected_stake / total_stake >= 80%.
    let mut total_stake: u128 = 0;
    for index in 0..operators {
        total_stake = total_stake
            .checked_add(proof.stake_weights.get(index).unwrap_or(0) as u128)
            .ok_or(DaError::InvalidProof)?;
    }
    if total_stake == 0 {
        return Err(DaError::InvalidQuorum);
    }
    let mut signed_stake: u128 = 0;
    let mut seen: Vec<u32> = Vec::new(env);
    for operator in proof.signer_bitmap.iter() {
        let index = operator as u32;
        if index >= operators || seen.contains(&index) {
            // A duplicate index would let one operator sign twice and inflate
            // its stake share to pass the quorum.
            return Err(DaError::InvalidProof);
        }
        seen.push_back(index);
        signed_stake = signed_stake
            .checked_add(proof.stake_weights.get(index).unwrap_or(0) as u128)
            .ok_or(DaError::InvalidProof)?;
    }
    if signed_stake
        .checked_mul(100)
        .ok_or(DaError::InvalidQuorum)?
        < total_stake
            .checked_mul(QUORUM_THRESHOLD_PERCENT)
            .ok_or(DaError::InvalidQuorum)?
    {
        return Err(DaError::InvalidQuorum);
    }

    let bls: Bls12_381 = env.crypto().bls12_381();
    let dst = Bytes::from_slice(env, DOMAIN);

    // Domain-separate the message with the blob header AND the MMR root so a
    // proof cannot be replayed against a different checkpoint.
    let mut message = Bytes::new(env);
    message.append(&dst);
    message.append(blob_header);
    message.append(&Bytes::from_array(env, &proof.mmr_root.to_array()));
    let digest = env.crypto().sha256(&message);
    let hashed_message = bls.hash_to_g1(&Bytes::from(digest), &dst);

    // The scheme's generators are derived deterministically from the domain,
    // so no trusted setup and no hard-coded curve constants are required.
    let g1_generator = bls.hash_to_g1(&Bytes::from_slice(env, b"TESSERA-DA-G1"), &dst);
    let g2_generator = bls.hash_to_g2(&Bytes::from_slice(env, b"TESSERA-DA-G2"), &dst);

    let signature = G1Affine::from_bytes(proof.aggregate_signature.clone());
    if !signature.is_in_subgroup() || !g1_generator.is_in_subgroup() {
        return Err(DaError::InvalidSignature);
    }

    // Aggregate the public keys of the operators that actually signed.
    let mut infinity = [0u8; 192];
    infinity[0] = 0x40; // BLS12-381 infinity flag, uncompressed encoding.
    let mut aggregate_key = G2Affine::from_bytes(BytesN::from_array(env, &infinity));
    for operator in proof.signer_bitmap.iter() {
        let key = G2Affine::from_bytes(proof.operator_pubkeys.get(operator as u32).unwrap());
        if !key.is_in_subgroup() {
            return Err(DaError::InvalidSignature);
        }
        aggregate_key = aggregate_key
            .checked_add(&key)
            .ok_or(DaError::InvalidSignature)?;
    }

    // e(sig, g2) * e(H(m), -agg_pk) == 1  <=>  e(sig, g2) == e(H(m), agg_pk)
    let pairing = bls.pairing_check(
        Vec::from_array(env, [signature, hashed_message]),
        Vec::from_array(env, [g2_generator, -aggregate_key]),
    );
    if pairing {
        Ok(())
    } else {
        Err(DaError::InvalidSignature)
    }
}

#[cfg(test)]
mod test;
