use soroban_sdk::{contracttype, Env, Bytes, BytesN};

/// Represents the elements of a Groth16 proof.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Groth16Proof {
    /// G1 point A
    pub a: BytesN<32>,
    /// G2 point B
    pub b: BytesN<64>,
    /// G1 point C
    pub c: BytesN<32>,
}

/// Represents the Verification Key for a specific circuit.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationKey {
    pub alpha_g1: BytesN<32>,
    pub beta_g2: BytesN<64>,
    pub gamma_g2: BytesN<64>,
    pub delta_g2: BytesN<64>,
    pub ic: soroban_sdk::Vec<BytesN<32>>,
}

pub struct ZkVerifier;

impl ZkVerifier {
    /// Verifies a Groth16 proof against a set of public inputs using a verification key.
    ///
    /// In a fully realized implementation on Soroban, this would use a native host function
    /// for elliptic curve pairings (e.g., BN254 or BLS12-381) to ensure verification
    /// remains within gas and CPU limits:
    ///
    /// 1. Ensure the number of public inputs matches the expected length in the verification key.
    /// 2. Compute the linear combination of public inputs:
    ///    `L = vk.ic[0] + Σ_{i=1}^{n} public_inputs[i-1] * vk.ic[i]`
    /// 3. Perform the pairing check:
    ///    `e(A, B) == e(alpha_G1, beta_G2) * e(L, gamma_G2) * e(C, delta_G2)`
    ///
    /// That pairing check is not implemented here -- it needs either a host
    /// function or an on-chain BN254 library this contract does not have.
    /// **Until it is, this function must fail closed.** A verifier that
    /// returns `true` for every input it cannot actually check is worse than
    /// having no verifier at all: it would let any caller "prove" any claim
    /// by submitting a well-formed but otherwise meaningless proof. So this
    /// checks everything it genuinely can (proof/key shape, public input
    /// count) and then rejects, rather than accepting on faith.
    pub fn verify(
        _env: &Env,
        vk: &VerificationKey,
        _proof: &Groth16Proof,
        public_inputs: &soroban_sdk::Vec<BytesN<32>>,
    ) -> bool {
        // Ensure the number of public inputs matches the expected length in the verification key.
        if public_inputs.len() + 1 != vk.ic.len() {
            return false;
        }

        // The linear combination and pairing check above are not implemented
        // (see doc comment), so every structurally well-formed proof is
        // still rejected rather than accepted.
        false
    }
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

    #[test]
    fn rejects_a_structurally_well_formed_but_unverified_proof() {
        // The regression this guards: `verify` must never return `true` for
        // input it has not actually checked by pairing. A well-formed shape
        // is not a valid proof.
        let env = Env::default();
        let vk = key_with_ic_len(&env, 1);
        let proof = well_formed_proof(&env);
        let public_inputs = SdkVec::new(&env);

        assert!(!ZkVerifier::verify(&env, &vk, &proof, &public_inputs));
    }

    #[test]
    fn rejects_a_mismatched_public_input_count() {
        let env = Env::default();
        let vk = key_with_ic_len(&env, 2);
        let proof = well_formed_proof(&env);
        let public_inputs = SdkVec::new(&env); // needs len 1, has 0

        assert!(!ZkVerifier::verify(&env, &vk, &proof, &public_inputs));
    }
}
