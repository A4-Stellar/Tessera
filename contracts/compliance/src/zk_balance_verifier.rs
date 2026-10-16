//! Zero-knowledge balance range verifier (issue #295).
//!
//! Lets an account prove its asset balance falls within a required range
//! (`min <= balance <= max`) without revealing the exact balance, by binding
//! the range bounds into the Groth16 public inputs checked by
//! [`crate::zk_verifier::ZkVerifier`].
//!
//! # What this actually proves today
//!
//! [`ZkVerifier::verify`] does not yet perform the elliptic-curve pairing
//! check a real Groth16 verifier needs -- Soroban has no BN254/BLS12-381
//! pairing host function, and no on-chain pairing library lives in this
//! repository. It currently rejects every proof (see its own doc comment
//! for why that is the correct behaviour for an unimplemented check, rather
//! than accepting everything).
//!
//! This module is therefore the real, honest shape of the feature -- the
//! public interface, the range-bound binding, and the "zero leak of raw
//! balance" property (the balance itself never appears in any argument or
//! return value here, only a commitment to it) -- wired to a verifier that
//! correctly fails closed until the pairing check lands. Treat
//! `verify_balance_in_range` returning `false` for a well-formed proof as
//! expected today, not as a bug in this module.
//!
//! See `scripts/zk/` for the off-chain proof-generation side of this
//! interface, in Rust and TypeScript -- both are scaffolding for the same
//! reason: there is nothing to prove against until a real circuit and
//! prover exist.

use crate::zk_verifier::{Groth16Proof, VerificationKey, ZkVerifier};
use soroban_sdk::{contracttype, BytesN, Env, Vec as SdkVec};

/// A claim that some committed balance lies in `[min, max]`, plus the proof
/// of it. `balance_commitment` is a Pedersen-style commitment to the balance
/// (never the balance itself), bound into the circuit's public inputs
/// alongside `min`/`max` so a proof for one range or one commitment cannot
/// be replayed against another.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BalanceRangeClaim {
    pub balance_commitment: BytesN<32>,
    pub min: i128,
    pub max: i128,
}

/// Verifies that `claim.proof` attests `min <= balance <= max` for the
/// account behind `claim.balance_commitment`, without the verifier ever
/// learning the balance.
///
/// Returns `false` if `min > max` (a range no balance can satisfy), if the
/// public inputs don't match what `vk` expects, or -- today, always -- if
/// the pairing check inside [`ZkVerifier::verify`] is not yet implemented.
pub fn verify_balance_in_range(
    env: &Env,
    vk: &VerificationKey,
    proof: &Groth16Proof,
    claim: &BalanceRangeClaim,
) -> bool {
    if claim.min > claim.max {
        return false;
    }

    let public_inputs = encode_public_inputs(env, claim);
    ZkVerifier::verify(env, vk, proof, &public_inputs)
}

/// Encodes `(balance_commitment, min, max)` as the fixed-width field
/// elements a real circuit would take as public inputs. `min`/`max` are
/// encoded big-endian into the low 16 bytes of a 32-byte field, matching how
/// `soroban_sdk::BytesN<32>` treats shorter integer payloads elsewhere in
/// this crate (see `attestation.rs`).
fn encode_public_inputs(env: &Env, claim: &BalanceRangeClaim) -> SdkVec<BytesN<32>> {
    let mut inputs = SdkVec::new(env);
    inputs.push_back(claim.balance_commitment.clone());
    inputs.push_back(encode_i128(env, claim.min));
    inputs.push_back(encode_i128(env, claim.max));
    inputs
}

fn encode_i128(env: &Env, value: i128) -> BytesN<32> {
    let mut bytes = [0u8; 32];
    bytes[16..].copy_from_slice(&value.to_be_bytes());
    BytesN::from_array(env, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Vec as SdkVec;

    fn key_with_ic_len(env: &Env, len: u32) -> VerificationKey {
        let mut ic = SdkVec::new(env);
        for _ in 0..len {
            ic.push_back(BytesN::from_array(env, &[0u8; 32]));
        }
        VerificationKey {
            alpha_g1: BytesN::from_array(env, &[0u8; 32]),
            beta_g2: BytesN::from_array(env, &[0u8; 64]),
            gamma_g2: BytesN::from_array(env, &[0u8; 64]),
            delta_g2: BytesN::from_array(env, &[0u8; 64]),
            ic,
        }
    }

    fn well_formed_proof(env: &Env) -> Groth16Proof {
        Groth16Proof {
            a: BytesN::from_array(env, &[0u8; 32]),
            b: BytesN::from_array(env, &[0u8; 64]),
            c: BytesN::from_array(env, &[0u8; 32]),
        }
    }

    fn claim(env: &Env, min: i128, max: i128) -> BalanceRangeClaim {
        BalanceRangeClaim {
            balance_commitment: BytesN::from_array(env, &[7u8; 32]),
            min,
            max,
        }
    }

    #[test]
    fn rejects_an_inverted_range_before_touching_the_verifier() {
        let env = Env::default();
        let vk = key_with_ic_len(&env, 4);
        let proof = well_formed_proof(&env);

        assert!(!verify_balance_in_range(
            &env,
            &vk,
            &proof,
            &claim(&env, 100, 50),
        ));
    }

    #[test]
    fn rejects_a_well_formed_claim_while_the_pairing_check_is_unimplemented() {
        // Three public inputs (commitment, min, max) means `vk.ic` must
        // have length 4 for the count check inside `ZkVerifier::verify` to
        // pass -- and it must still reject, because no pairing is performed.
        let env = Env::default();
        let vk = key_with_ic_len(&env, 4);
        let proof = well_formed_proof(&env);

        assert!(!verify_balance_in_range(
            &env,
            &vk,
            &proof,
            &claim(&env, 0, 1_000),
        ));
    }

    #[test]
    fn rejects_when_the_verification_key_does_not_match_the_input_shape() {
        let env = Env::default();
        let vk = key_with_ic_len(&env, 1); // wrong length for 3 public inputs
        let proof = well_formed_proof(&env);

        assert!(!verify_balance_in_range(
            &env,
            &vk,
            &proof,
            &claim(&env, 0, 1_000),
        ));
    }

    #[test]
    fn encodes_min_and_max_as_distinct_field_elements() {
        let env = Env::default();
        let a = encode_i128(&env, 5);
        let b = encode_i128(&env, 6);
        assert_ne!(a, b);
    }

    #[test]
    fn encodes_negative_values_distinctly_from_their_positive_counterparts() {
        let env = Env::default();
        let positive = encode_i128(&env, 5);
        let negative = encode_i128(&env, -5);
        assert_ne!(positive, negative);
    }
}
