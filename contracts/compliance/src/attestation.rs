//! Signed KYC assertions and sparse-Merkle non-revocation proofs.
//!
//! The issuer signs SHA-256 of the following bytes, in order:
//! `TESSERA_KYC_ATTESTATION_V1 || investor XDR || issuer XDR || id ||
//! jurisdiction XDR || verified_at(be) || expires_at(be)`. The Merkle tree
//! uses the attestation ID as its 256-bit path (least-significant bit first,
//! starting at the final byte), zero as the non-revoked leaf, and
//! `sha256("TESSERA_KYC_REVOKED_V1" || id)` as the revoked leaf. Each proof
//! contains exactly 256 siblings, leaf to root. Providers invalidate any
//! number of assertions by publishing one replacement root.

use soroban_sdk::{contracttype, xdr::ToXdr, Address, Bytes, BytesN, Env, String, Vec};

use crate::DataKey;

const SIGNING_DOMAIN: &[u8] = b"TESSERA_KYC_ATTESTATION_V1";
const REVOCATION_DOMAIN: &[u8] = b"TESSERA_KYC_REVOKED_V1";
const TREE_DEPTH: u32 = 256;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentityAttestation {
    pub investor: Address,
    pub issuer: Address,
    pub id: BytesN<32>,
    pub jurisdiction: String,
    pub verified_at: u32,
    /// Ledger sequence at which this assertion stops being valid. Zero means
    /// no time-based expiry (revocation remains effective).
    pub expires_at: u32,
    pub signature: BytesN<64>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevocationProof {
    /// Sibling nodes from the leaf level up to the root; must contain 256
    /// hashes for a fixed-depth sparse Merkle proof.
    pub siblings: Vec<BytesN<32>>,
}

pub(crate) fn issuer_mode_enabled(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::AttestationMode)
        .unwrap_or(false)
}

pub(crate) fn verify_with_proof(
    env: &Env,
    attestation: &IdentityAttestation,
    investor: &Address,
    proof: &RevocationProof,
) -> bool {
    if &attestation.investor != investor || proof.siblings.len() != TREE_DEPTH {
        return false;
    }
    let public_key: Option<BytesN<32>> = env
        .storage()
        .instance()
        .get(&DataKey::IssuerKey(attestation.issuer.clone()));
    let root: Option<BytesN<32>> = env
        .storage()
        .instance()
        .get(&DataKey::RevocationRoot(attestation.issuer.clone()));
    let (public_key, root) = match (public_key, root) {
        (Some(key), Some(root)) => (key, root),
        _ => return false,
    };

    if attestation.verified_at > env.ledger().sequence()
        || (attestation.expires_at != 0 && env.ledger().sequence() >= attestation.expires_at)
    {
        return false;
    }

    let message = signing_message(env, attestation);
    let message_hash: BytesN<32> = env.crypto().sha256(&message).into();
    // An invalid Ed25519 signature traps in the Soroban host, which safely
    // rejects the enclosing transfer check.
    env.crypto().ed25519_verify(
        &public_key,
        &Bytes::from(message_hash),
        &attestation.signature,
    );

    let mut node = BytesN::<32>::from_array(env, &[0; 32]);
    for depth in 0..TREE_DEPTH {
        let sibling = proof.siblings.get(depth).unwrap();
        let byte_index = 31 - (depth / 8) as usize;
        let bit_in_byte = depth % 8;
        let is_right = (attestation.id.to_array()[byte_index] >> bit_in_byte) & 1 == 1;
        node = if is_right {
            hash_pair(env, &sibling, &node)
        } else {
            hash_pair(env, &node, &sibling)
        };
    }
    node == root
}

pub(crate) fn verify(env: &Env, investor: &Address) -> bool {
    let stored: Option<(IdentityAttestation, RevocationProof)> = env
        .storage()
        .persistent()
        .get(&DataKey::Attestation(investor.clone()));
    let (attestation, proof) = match stored {
        Some(value) => value,
        None => return false,
    };
    let key = DataKey::Attestation(investor.clone());
    env.storage()
        .persistent()
        .extend_ttl(&key, 100_000, 120_000);
    if !verify_with_proof(env, &attestation, investor, &proof) {
        return false;
    }
    !crate::ComplianceContract::is_jurisdiction_blocked(env.clone(), attestation.jurisdiction)
}

fn signing_message(env: &Env, attestation: &IdentityAttestation) -> Bytes {
    let mut message = Bytes::from_slice(env, SIGNING_DOMAIN);
    message.append(&attestation.investor.clone().to_xdr(env));
    message.append(&attestation.issuer.clone().to_xdr(env));
    message.append(&Bytes::from(attestation.id.clone()));
    message.append(&attestation.jurisdiction.clone().to_xdr(env));
    message.append(&Bytes::from_array(
        env,
        &attestation.verified_at.to_be_bytes(),
    ));
    message.append(&Bytes::from_array(
        env,
        &attestation.expires_at.to_be_bytes(),
    ));
    message
}

fn hash_pair(env: &Env, left: &BytesN<32>, right: &BytesN<32>) -> BytesN<32> {
    let mut input = Bytes::new(env);
    input.append(&Bytes::from(left.clone()));
    input.append(&Bytes::from(right.clone()));
    env.crypto().sha256(&input).into()
}

#[allow(dead_code)]
fn revoked_leaf(env: &Env, id: &BytesN<32>) -> BytesN<32> {
    let mut input = Bytes::from_slice(env, REVOCATION_DOMAIN);
    input.append(&Bytes::from(id.clone()));
    env.crypto().sha256(&input).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn sample_attestation(env: &Env, investor: Address) -> IdentityAttestation {
        IdentityAttestation {
            investor,
            issuer: Address::generate(env),
            id: BytesN::from_array(env, &[7; 32]),
            jurisdiction: String::from_str(env, "US"),
            verified_at: 1,
            expires_at: 0,
            signature: BytesN::from_array(env, &[0; 64]),
        }
    }

    #[test]
    fn rejects_a_proof_with_the_wrong_depth() {
        let env = Env::default();
        let investor = Address::generate(&env);
        let attestation = sample_attestation(&env, investor.clone());
        let mut siblings = Vec::new(&env);
        siblings.push_back(BytesN::from_array(&env, &[0; 32]));
        let proof = RevocationProof { siblings };

        assert!(!verify_with_proof(&env, &attestation, &investor, &proof));
    }

    #[test]
    fn rejects_an_attestation_bound_to_another_investor() {
        let env = Env::default();
        let investor = Address::generate(&env);
        let other = Address::generate(&env);
        let attestation = sample_attestation(&env, investor);
        let mut siblings = Vec::new(&env);
        siblings.push_back(BytesN::from_array(&env, &[0; 32]));
        let proof = RevocationProof { siblings };

        assert!(!verify_with_proof(&env, &attestation, &other, &proof));
    }
}
