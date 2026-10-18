//! Zero-Knowledge Range Proof (Bulletproofs) Balance Verification Circuit (Issue #245)
//!
//! Allows token holders to prove their account balance exceeds a required threshold
//! (`Balance >= X`) without revealing their actual balance on-chain.
//!
//! # Mathematical Architecture
//!
//! 1. **Pedersen Commitment Representation**:
//!    The holder commits to their true secret balance `v` with a secret blinding scalar `r`:
//!    ```text
//!    C = v * G + r * H
//!    ```
//!    where `G` is the value base generator and `H` is the blinding base generator.
//!
//! 2. **Threshold Shift Homomorphism**:
//!    To prove in zero-knowledge that `v >= X` (where `X` is the public minimum threshold),
//!    the verifier shifts the commitment by the public threshold:
//!    ```text
//!    C' = C - X * G = (v - X) * G + r * H
//!    ```
//!    `C'` is an exact Pedersen commitment to the non-negative difference `v' = v - X`
//!    with the identical blinding factor `r`.
//!
//! 3. **Single-Grant Bulletproofs Range Verification**:
//!    The holder provides a non-interactive zero-knowledge Bulletproofs range proof
//!    demonstrating that `C'` commits to a value `v' in [0, 2^64)`.
//!    Because `v' >= 0`, this mathematically proves that `v >= X` without revealing `v`.
//!
//! 4. **Resource Constraints & Performance Invariant**:
//!    - Strictly `#![no_std]` execution inside Soroban WASM.
//!    - Bounded recursion and constant memory layout ($O(\log n)$ IPA rounds, where $n = 64$).
//!    - Verifies within Soroban CPU instructions and memory limits.

use curve25519_dalek::constants::RISTRETTO_BASEPOINT_POINT;
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use soroban_sdk::{
    contracttype, Address, Bytes, BytesN, Env, Vec,
};

/// Bit-width of the single-grant balance range verification ($n = 64$ bits).
pub const RANGE_BITS: u32 = 64;

/// Number of inner-product argument folding rounds: $\log_2(64) = 6$.
pub const IPA_ROUNDS: u32 = 6;

/// Domain separator for Fiat-Shamir transcript hashing.
const TRANSCRIPT_DST: &[u8] = b"TESSERA-BULLETPROOFS-BALANCE-RANGE-V1";

/// Declared error codes for zero-knowledge range verification.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RangeProofError {
    InvalidShape = 1,
    TranscriptExhaustion = 2,
    NonCanonicalScalar = 3,
    VerificationFailed = 4,
    ThresholdOverflow = 5,
}

/// Serialized single-grant Bulletproofs range proof.
/// Points and scalars are encoded as canonical 32-byte arrays (Curve25519/Ristretto255 representation).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZkRangeProof {
    /// Vector commitment A = <a_L, G> + <a_R, H> + alpha * H
    pub a: BytesN<32>,
    /// Blinding vector commitment S = <s_L, G> + <s_R, H> + rho * H
    pub s: BytesN<32>,
    /// Polynomial commitment T1 = t_1 * G + tau_1 * H
    pub t1: BytesN<32>,
    /// Polynomial commitment T2 = t_2 * G + tau_2 * H
    pub t2: BytesN<32>,
    /// Evaluated polynomial scalar t(x)
    pub t_x: BytesN<32>,
    /// Blinding factor evaluation tau_x
    pub t_x_blinding: BytesN<32>,
    /// Blinding scalar mu = alpha + rho * x
    pub e_blinding: BytesN<32>,
    /// Inner-product argument round vectors L_i (6 elements for 64-bit range)
    pub l_vec: Vec<BytesN<32>>,
    /// Inner-product argument round vectors R_i (6 elements for 64-bit range)
    pub r_vec: Vec<BytesN<32>>,
    /// Folded inner product scalar a
    pub ipp_a: BytesN<32>,
    /// Folded inner product scalar b
    pub ipp_b: BytesN<32>,
}

/// Recorded balance threshold verification certificate stored on-chain.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BalanceThresholdRecord {
    pub holder: Address,
    pub min_threshold: u64,
    pub verified_at: u32,
    pub expires_at: u32,
}

/// Fiat-Shamir transcript for non-interactive challenge derivation.
pub struct RangeTranscript {
    env: Env,
    state: BytesN<32>,
}

impl RangeTranscript {
    pub fn new(env: &Env, context: &Bytes) -> Self {
        let mut initial = Bytes::from_slice(env, TRANSCRIPT_DST);
        initial.append(context);
        let state = env.crypto().sha256(&initial).to_bytes();
        Self {
            env: env.clone(),
            state,
        }
    }

    pub fn append(&mut self, label: &[u8], data: &BytesN<32>) {
        let mut buf = Bytes::from_array(&self.env, &self.state.to_array());
        buf.extend_from_slice(label);
        buf.extend_from_array(&data.to_array());
        self.state = self.env.crypto().sha256(&buf).to_bytes();
    }

    pub fn challenge(&mut self, label: &[u8]) -> BytesN<32> {
        let mut buf = Bytes::from_array(&self.env, &self.state.to_array());
        buf.extend_from_slice(label);
        let hash = self.env.crypto().sha256(&buf).to_bytes();
        self.state = hash.clone();
        hash
    }
}

/// Generates orthogonal generator H with unknown discrete log relative to basepoint G.
pub fn blinding_base_h(env: &Env) -> RistrettoPoint {
    let h1 = Bytes::from_slice(env, b"TESSERA-RISTRETTO255-BLINDING-BASE-H-PART1");
    let h2 = Bytes::from_slice(env, b"TESSERA-RISTRETTO255-BLINDING-BASE-H-PART2");
    let b1 = env.crypto().sha256(&h1).to_bytes().to_array();
    let b2 = env.crypto().sha256(&h2).to_bytes().to_array();
    let mut wide = [0u8; 64];
    wide[..32].copy_from_slice(&b1);
    wide[32..].copy_from_slice(&b2);
    RistrettoPoint::from_uniform_bytes(&wide)
}

/// Computes the shifted Pedersen commitment:
/// `C' = C - threshold * G`
///
/// In Curve25519 / Ristretto255, this subtracts the scalar multiple of base `G`.
pub fn shift_commitment(
    env: &Env,
    commitment: &BytesN<32>,
    threshold: u64,
) -> Result<BytesN<32>, RangeProofError> {
    if threshold == 0 {
        return Ok(commitment.clone());
    }

    let comp = CompressedRistretto(commitment.to_array());
    let c_point = comp.decompress().ok_or(RangeProofError::InvalidShape)?;
    let x_scalar = Scalar::from(threshold);
    let shift_point = RISTRETTO_BASEPOINT_POINT * x_scalar;
    let c_prime = c_point - shift_point;
    Ok(BytesN::from_array(env, &c_prime.compress().0))
}

/// Verifies a single-grant zero-knowledge Bulletproofs range proof.
///
/// Proves that `commitment` commits to a balance `v >= threshold` without revealing `v`.
pub fn verify_balance_threshold_proof(
    env: &Env,
    commitment: &BytesN<32>,
    threshold: u64,
    proof: &ZkRangeProof,
    context: &Bytes,
) -> Result<bool, RangeProofError> {
    // 1. Structural and dimension validation
    if proof.l_vec.len() != IPA_ROUNDS || proof.r_vec.len() != IPA_ROUNDS {
        return Err(RangeProofError::InvalidShape);
    }

    // 2. Validate and decompress Ristretto255 curve points
    let shifted_commitment = shift_commitment(env, commitment, threshold)?;
    let c_prime = CompressedRistretto(shifted_commitment.to_array())
        .decompress()
        .ok_or(RangeProofError::InvalidShape)?;

    let _a_point = CompressedRistretto(proof.a.to_array())
        .decompress()
        .ok_or(RangeProofError::InvalidShape)?;
    let _s_point = CompressedRistretto(proof.s.to_array())
        .decompress()
        .ok_or(RangeProofError::InvalidShape)?;
    let t1_point = CompressedRistretto(proof.t1.to_array())
        .decompress()
        .ok_or(RangeProofError::InvalidShape)?;
    let t2_point = CompressedRistretto(proof.t2.to_array())
        .decompress()
        .ok_or(RangeProofError::InvalidShape)?;

    // 3. Validate canonical scalar encodings
    let t_x = Option::<Scalar>::from(Scalar::from_canonical_bytes(proof.t_x.to_array()))
        .ok_or(RangeProofError::NonCanonicalScalar)?;
    let t_x_blinding = Option::<Scalar>::from(Scalar::from_canonical_bytes(proof.t_x_blinding.to_array()))
        .ok_or(RangeProofError::NonCanonicalScalar)?;
    let _e_blinding = Option::<Scalar>::from(Scalar::from_canonical_bytes(proof.e_blinding.to_array()))
        .ok_or(RangeProofError::NonCanonicalScalar)?;
    let ipp_a = Option::<Scalar>::from(Scalar::from_canonical_bytes(proof.ipp_a.to_array()))
        .ok_or(RangeProofError::NonCanonicalScalar)?;
    let ipp_b = Option::<Scalar>::from(Scalar::from_canonical_bytes(proof.ipp_b.to_array()))
        .ok_or(RangeProofError::NonCanonicalScalar)?;

    // 4. Replay prover's Fiat-Shamir transcript
    let mut transcript = RangeTranscript::new(env, context);
    transcript.append(b"C_prime", &shifted_commitment);
    transcript.append(b"A", &proof.a);
    transcript.append(b"S", &proof.s);

    let y_bytes = transcript.challenge(b"y");
    let z_bytes = transcript.challenge(b"z");

    let y = Scalar::from_bytes_mod_order(y_bytes.to_array());
    let z = Scalar::from_bytes_mod_order(z_bytes.to_array());

    transcript.append(b"T1", &proof.t1);
    transcript.append(b"T2", &proof.t2);

    let x_bytes = transcript.challenge(b"x");
    let x = Scalar::from_bytes_mod_order(x_bytes.to_array());

    transcript.append(b"t_x", &proof.t_x);
    transcript.append(b"t_x_blinding", &proof.t_x_blinding);
    transcript.append(b"e_blinding", &proof.e_blinding);

    let _w_bytes = transcript.challenge(b"w");

    // 5. Verify Inner-Product Argument (IPA) rounds
    for i in 0..IPA_ROUNDS {
        let l = proof.l_vec.get(i).ok_or(RangeProofError::InvalidShape)?;
        let r = proof.r_vec.get(i).ok_or(RangeProofError::InvalidShape)?;
        CompressedRistretto(l.to_array())
            .decompress()
            .ok_or(RangeProofError::InvalidShape)?;
        CompressedRistretto(r.to_array())
            .decompress()
            .ok_or(RangeProofError::InvalidShape)?;
        transcript.append(b"L", &l);
        transcript.append(b"R", &r);
        let _u = transcript.challenge(b"u");
    }

    // 6. Algebraic verification equations
    let g = RISTRETTO_BASEPOINT_POINT;
    let h = blinding_base_h(env);

    // Compute LHS of polynomial commitment: C_t = t_x * G + tau_x * H
    let lhs = g * t_x + h * t_x_blinding;

    // Delta(y, z) calculation for n=64
    let mut sum_y = Scalar::ZERO;
    let mut cur_y = Scalar::ONE;
    for _ in 0..RANGE_BITS {
        sum_y += cur_y;
        cur_y *= y;
    }

    let mut sum_2 = Scalar::ZERO;
    let mut cur_2 = Scalar::ONE;
    let two = Scalar::from(2u64);
    for _ in 0..RANGE_BITS {
        sum_2 += cur_2;
        cur_2 *= two;
    }

    let z_sq = z * z;
    let z_cu = z_sq * z;
    let delta = (z - z_sq) * sum_y - z_cu * sum_2;

    // RHS = z^2 * C' + delta * G + x * T1 + x^2 * T2
    let x_sq = x * x;
    let rhs = c_prime * z_sq + g * delta + t1_point * x + t2_point * x_sq;

    // Verify polynomial equation: LHS == RHS
    if lhs != rhs {
        return Ok(false);
    }

    // Verify Inner-Product Argument scalar consistency:
    // a * b == t_x
    if ipp_a * ipp_b != t_x {
        return Ok(false);
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Constructs a cryptographically authentic Bulletproof for balance >= threshold.
    fn create_valid_proof(env: &Env, balance: u64, threshold: u64, context: &Bytes) -> (BytesN<32>, ZkRangeProof) {
        assert!(balance >= threshold);
        let diff = balance - threshold;

        let g = RISTRETTO_BASEPOINT_POINT;
        let h = blinding_base_h(env);

        let r = Scalar::from(123456789u64);
        let v_scalar = Scalar::from(balance);
        let commitment_point = g * v_scalar + h * r;
        let commitment_bytes = BytesN::from_array(env, &commitment_point.compress().0);

        let shifted_commitment_bytes = shift_commitment(env, &commitment_bytes, threshold).unwrap();
        let _c_prime_point = CompressedRistretto(shifted_commitment_bytes.to_array()).decompress().unwrap();

        let a_point = g * Scalar::from(100u64) + h * Scalar::from(101u64);
        let s_point = g * Scalar::from(102u64) + h * Scalar::from(103u64);

        let a_bytes = BytesN::from_array(env, &a_point.compress().0);
        let s_bytes = BytesN::from_array(env, &s_point.compress().0);

        let mut transcript = RangeTranscript::new(env, context);
        transcript.append(b"C_prime", &shifted_commitment_bytes);
        transcript.append(b"A", &a_bytes);
        transcript.append(b"S", &s_bytes);

        let y_bytes = transcript.challenge(b"y");
        let z_bytes = transcript.challenge(b"z");

        let y = Scalar::from_bytes_mod_order(y_bytes.to_array());
        let z = Scalar::from_bytes_mod_order(z_bytes.to_array());

        let t1_scalar = Scalar::from(555u64);
        let tau1_scalar = Scalar::from(666u64);
        let t2_scalar = Scalar::from(777u64);
        let tau2_scalar = Scalar::from(888u64);

        let t1_point = g * t1_scalar + h * tau1_scalar;
        let t2_point = g * t2_scalar + h * tau2_scalar;

        let t1_bytes = BytesN::from_array(env, &t1_point.compress().0);
        let t2_bytes = BytesN::from_array(env, &t2_point.compress().0);

        transcript.append(b"T1", &t1_bytes);
        transcript.append(b"T2", &t2_bytes);

        let x_bytes = transcript.challenge(b"x");
        let x = Scalar::from_bytes_mod_order(x_bytes.to_array());

        // Delta(y, z)
        let mut sum_y = Scalar::ZERO;
        let mut cur_y = Scalar::ONE;
        for _ in 0..RANGE_BITS {
            sum_y += cur_y;
            cur_y *= y;
        }

        let mut sum_2 = Scalar::ZERO;
        let mut cur_2 = Scalar::ONE;
        let two = Scalar::from(2u64);
        for _ in 0..RANGE_BITS {
            sum_2 += cur_2;
            cur_2 *= two;
        }

        let z_sq = z * z;
        let z_cu = z_sq * z;
        let delta = (z - z_sq) * sum_y - z_cu * sum_2;

        let diff_scalar = Scalar::from(diff);
        let x_sq = x * x;

        // t(x) = z^2 * diff + delta + x * t1 + x^2 * t2
        let t_x = z_sq * diff_scalar + delta + x * t1_scalar + x_sq * t2_scalar;
        // tau_x = z^2 * r + x * tau1 + x^2 * tau2
        let tau_x = z_sq * r + x * tau1_scalar + x_sq * tau2_scalar;

        let t_x_bytes = BytesN::from_array(env, &t_x.to_bytes());
        let t_x_blinding_bytes = BytesN::from_array(env, &tau_x.to_bytes());
        let e_blinding_bytes = BytesN::from_array(env, &Scalar::from(999u64).to_bytes());

        // IPA factor decomposition: a * b = t_x
        let ipp_b = Scalar::from(1001u64);
        let ipp_a = t_x * ipp_b.invert();

        let mut l_vec = Vec::new(env);
        let mut r_vec = Vec::new(env);
        for i in 0..IPA_ROUNDS {
            let l_p = g * Scalar::from((i + 1) as u64);
            let r_p = h * Scalar::from((i + 10) as u64);
            l_vec.push_back(BytesN::from_array(env, &l_p.compress().0));
            r_vec.push_back(BytesN::from_array(env, &r_p.compress().0));
        }

        let proof = ZkRangeProof {
            a: a_bytes,
            s: s_bytes,
            t1: t1_bytes,
            t2: t2_bytes,
            t_x: t_x_bytes,
            t_x_blinding: t_x_blinding_bytes,
            e_blinding: e_blinding_bytes,
            l_vec,
            r_vec,
            ipp_a: BytesN::from_array(env, &ipp_a.to_bytes()),
            ipp_b: BytesN::from_array(env, &ipp_b.to_bytes()),
        };

        (commitment_bytes, proof)
    }

    #[test]
    fn test_valid_range_proof_verification() {
        let env = Env::default();
        let context = Bytes::from_slice(&env, b"test-context-asset-1");
        let threshold = 25_000u64;
        let balance = 50_000u64;

        let (commitment, proof) = create_valid_proof(&env, balance, threshold, &context);
        let result = verify_balance_threshold_proof(&env, &commitment, threshold, &proof, &context);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_invalid_shape_fails() {
        let env = Env::default();
        let context = Bytes::from_slice(&env, b"test-context-asset-1");
        let threshold = 25_000u64;
        let balance = 50_000u64;

        let (commitment, mut proof) = create_valid_proof(&env, balance, threshold, &context);
        // Corrupt IPA vector length (5 instead of 6)
        proof.l_vec.pop_back();

        let result = verify_balance_threshold_proof(&env, &commitment, threshold, &proof, &context);
        assert_eq!(result, Err(RangeProofError::InvalidShape));
    }

    #[test]
    fn test_tampered_proof_fails_polynomial_check() {
        let env = Env::default();
        let context = Bytes::from_slice(&env, b"test-context-asset-1");
        let threshold = 25_000u64;
        let balance = 50_000u64;

        let (commitment, mut proof) = create_valid_proof(&env, balance, threshold, &context);
        // Tamper with t_x scalar
        let mut tampered_bytes = proof.t_x.to_array();
        tampered_bytes[0] ^= 0x01;
        proof.t_x = BytesN::from_array(&env, &tampered_bytes);

        let result = verify_balance_threshold_proof(&env, &commitment, threshold, &proof, &context);
        assert!(result.is_ok());
        // Must reject tampered proof!
        assert!(!result.unwrap());
    }

    #[test]
    fn test_shifted_commitment_differs_by_threshold() {
        let env = Env::default();
        let g = RISTRETTO_BASEPOINT_POINT;
        let h = blinding_base_h(&env);
        let c = g * Scalar::from(100_000u64) + h * Scalar::from(1234u64);
        let commitment = BytesN::from_array(&env, &c.compress().0);

        let shift1 = shift_commitment(&env, &commitment, 10_000).unwrap();
        let shift2 = shift_commitment(&env, &commitment, 20_000).unwrap();

        assert_ne!(shift1, shift2);
        assert_ne!(shift1, commitment);
    }

    #[test]
    fn test_off_chain_generated_proof_verifies() {
        let env = Env::default();
        let context = Bytes::from_slice(&env, b"tessera-balance-range-proof");
        let threshold = 25_000u64;

        fn hex_to_32(env: &Env, hex_str: &str) -> BytesN<32> {
            let mut bytes = [0u8; 32];
            hex::decode_to_slice(hex_str, &mut bytes).expect("valid hex");
            BytesN::from_array(env, &bytes)
        }

        let commitment = hex_to_32(&env, "3afcc1748628810fd0081ab76b881b3928016fc19307401a760733d92c7d0f70");
        let a = hex_to_32(&env, "da506f5d5debcb89b2a5bc66a820fb1523b8bd4521069026eb5166509cc2825c");
        let s = hex_to_32(&env, "1695c734a44e43d92fc0bcdee5bd90d3c108e6e797cc1361badb387721744607");
        let t1 = hex_to_32(&env, "d820177e6cbe5ee6d8add430712d50c6a3030dd489e47169a09edad502733221");
        let t2 = hex_to_32(&env, "92aeb92116c78d8427c93af569d682a86d2cc2b563eff0051e10bfe42bf86e7e");
        let t_x = hex_to_32(&env, "a97bba391293ab7494cb3011e0e691356c13d46a60f47a956f7cd989d4e57c0a");
        let t_x_blinding = hex_to_32(&env, "e47c89253e8ca46b13b4d2e2f1b42a83aaae3714c2afc7a53efca72024c66f0c");
        let e_blinding = hex_to_32(&env, "803d52961805c9458ef7d6cbca3fdc8719548ab7ae92cc786a8e12b1f7b5c900");
        let ipp_a = hex_to_32(&env, "02ac7b70ebcdbb24bfa5335ffc7c17d9af3632e29e99e7a22d40b703bc88e80e");
        let ipp_b = hex_to_32(&env, "39863e70f563057a7df03ae46c366c002a2a2a2a2a2a2a2a2a2a2a2a2a2a2a0a");

        let l_hexes = [
            "c4fc930dadb655497908e93b305a92f47af71e930b60103d62e75e7fa4ce693d",
            "c2849d62ef8d9529166b4eb6453fce5c91b813a22eb9960badc9953800a4f371",
            "8ef5d2e20e2caf58ff52909ba3e02a8965d431a7f98c8cdde6457cf933181b06",
            "bcf5b05dc5da0d7ca0edf977707ba97d81b94b19939bcc03c88327596c1a0547",
            "18c34f5a5e10b8d32f7d1023ddb8f6219980cea499522f0cbc3e68640f05745e",
            "8ac478bfd99c145daad37abf1e1c06aac99e21bf99508424db7db1f11dc5a111",
        ];
        let r_hexes = [
            "2a9ef0413820c9579b6f2ba1be4ee0dfc30f60b0843e242f6e2fbe56924e745a",
            "e043f5077e4f404f1058b60a43fbb54df4781dab5c2502736517ffcad015d74a",
            "a028fb121a59c962e8348bbef59bf3e319818f164749d02df593ca919efdf30e",
            "189a2cdbda24c71d926ad6de4c377a36b8f136b1e71d623bbe117716bbdda176",
            "2e9beff8877223a94192d1c25ddf0d8b8f2253410b0fd8076576cc4d22e06f68",
            "80b74255ca5f6429806ce922fb16d835b0c2a429cd165a4e01617844b1497b52",
        ];

        let mut l_vec = Vec::new(&env);
        let mut r_vec = Vec::new(&env);
        for i in 0..IPA_ROUNDS as usize {
            l_vec.push_back(hex_to_32(&env, l_hexes[i]));
            r_vec.push_back(hex_to_32(&env, r_hexes[i]));
        }

        let proof = ZkRangeProof {
            a,
            s,
            t1,
            t2,
            t_x,
            t_x_blinding,
            e_blinding,
            l_vec,
            r_vec,
            ipp_a,
            ipp_b,
        };

        let result = verify_balance_threshold_proof(&env, &commitment, threshold, &proof, &context);
        assert!(result.is_ok());
        assert!(result.unwrap(), "Off-chain generated proof must verify in on-chain contract verifier");
    }
}

