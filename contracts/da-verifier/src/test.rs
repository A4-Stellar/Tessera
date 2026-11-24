use core::ops::Mul;

use soroban_sdk::{
    crypto::bls12_381::{Bls12_381, Fr, G1Affine, G2Affine},
    Bytes, BytesN, Env, Vec,
};

use super::{verify, DaError, DaProof, DaVerifierContract, DaVerifierContractClient, DOMAIN};

fn generators(env: &Env) -> (G1Affine, G2Affine) {
    let bls: Bls12_381 = env.crypto().bls12_381();
    let dst = Bytes::from_slice(env, DOMAIN);
    (
        bls.hash_to_g1(&Bytes::from_slice(env, b"TESSERA-DA-G1"), &dst),
        bls.hash_to_g2(&Bytes::from_slice(env, b"TESSERA-DA-G2"), &dst),
    )
}

fn hash_message(env: &Env, header: &Bytes, mmr_root: &BytesN<32>) -> G1Affine {
    let bls: Bls12_381 = env.crypto().bls12_381();
    let dst = Bytes::from_slice(env, DOMAIN);
    let mut message = Bytes::new(env);
    message.append(&dst);
    message.append(header);
    message.append(&Bytes::from_array(env, &mmr_root.to_array()));
    bls.hash_to_g1(&Bytes::from(env.crypto().sha256(&message)), &dst)
}

fn proof_for(env: &Env, header: &Bytes, mmr_root: &BytesN<32>, sk: &Fr) -> DaProof {
    let (_, g2) = generators(env);
    let signature = hash_message(env, header, mmr_root).mul(sk.clone());
    let public_key = g2.mul(sk.clone());
    DaProof {
        aggregate_signature: signature.to_bytes(),
        operator_pubkeys: Vec::from_array(env, [public_key.to_bytes()]),
        stake_weights: Vec::from_array(env, [100u64]),
        signer_bitmap: Vec::from_array(env, [0u32]),
        mmr_root: mmr_root.clone(),
    }
}

#[test]
fn verifies_a_valid_aggregate_proof() {
    let env = Env::default();
    let header = Bytes::from_slice(&env, b"batch-header-hash");
    let mmr_root = BytesN::from_array(&env, &[9u8; 32]);
    let sk = Fr::from_bytes(BytesN::from_array(&env, &[1u8; 32]));
    let proof = proof_for(&env, &header, &mmr_root, &sk);

    assert_eq!(verify(&env, &header, &proof), Ok(()));

    // Also exercise the exported contract function through its client.
    let id = env.register(DaVerifierContract, ());
    let client = DaVerifierContractClient::new(&env, &id);
    // The generated client unwraps contract errors into a panic, so a normal
    // return here means the exported function accepted the proof.
    client.verify_da_proof(&header, &proof);
}

#[test]
fn rejects_a_tampered_blob_header() {
    let env = Env::default();
    let header = Bytes::from_slice(&env, b"batch-header-hash");
    let mmr_root = BytesN::from_array(&env, &[9u8; 32]);
    let sk = Fr::from_bytes(BytesN::from_array(&env, &[1u8; 32]));
    let proof = proof_for(&env, &header, &mmr_root, &sk);

    let tampered = Bytes::from_slice(&env, b"batch-header-HASH");
    assert_eq!(
        verify(&env, &tampered, &proof),
        Err(DaError::InvalidSignature)
    );
}

#[test]
fn rejects_a_tampered_mmr_root() {
    let env = Env::default();
    let header = Bytes::from_slice(&env, b"batch-header-hash");
    let mmr_root = BytesN::from_array(&env, &[9u8; 32]);
    let sk = Fr::from_bytes(BytesN::from_array(&env, &[1u8; 32]));
    let proof = proof_for(&env, &header, &mmr_root, &sk);

    let other_root = BytesN::from_array(&env, &[8u8; 32]);
    assert_eq!(
        verify(&env, &header, &proof_with_root(&proof, other_root)),
        Err(DaError::InvalidSignature)
    );
}

fn proof_with_root(proof: &DaProof, mmr_root: BytesN<32>) -> DaProof {
    DaProof {
        aggregate_signature: proof.aggregate_signature.clone(),
        operator_pubkeys: proof.operator_pubkeys.clone(),
        stake_weights: proof.stake_weights.clone(),
        signer_bitmap: proof.signer_bitmap.clone(),
        mmr_root,
    }
}

#[test]
fn rejects_quorum_below_80_percent() {
    let env = Env::default();
    let header = Bytes::from_slice(&env, b"batch-header-hash");
    let mmr_root = BytesN::from_array(&env, &[9u8; 32]);
    let sk = Fr::from_bytes(BytesN::from_array(&env, &[1u8; 32]));
    let mut proof = proof_for(&env, &header, &mmr_root, &sk);

    // Add a non-signing operator with enough stake to drop participation to
    // 100 / 134 (~74.6%), below the required 80%.
    let (_, g2) = generators(&env);
    let other_pk = g2.mul(Fr::from_bytes(BytesN::from_array(&env, &[2u8; 32])));
    proof.operator_pubkeys.push_back(other_pk.to_bytes());
    proof.stake_weights.push_back(34);

    assert_eq!(verify(&env, &header, &proof), Err(DaError::InvalidQuorum));
}

#[test]
fn rejects_malformed_proofs_closed() {
    let env = Env::default();
    let header = Bytes::from_slice(&env, b"batch-header-hash");
    let mmr_root = BytesN::from_array(&env, &[9u8; 32]);
    let sk = Fr::from_bytes(BytesN::from_array(&env, &[1u8; 32]));
    let mut proof = proof_for(&env, &header, &mmr_root, &sk);

    proof.signer_bitmap = Vec::from_array(&env, [5u32]); // out of range
    assert_eq!(verify(&env, &header, &proof), Err(DaError::InvalidProof));

    let mut proof = proof_for(&env, &header, &mmr_root, &sk);
    proof.signer_bitmap = Vec::from_array(&env, [0u32, 0u32]); // same operator twice
    assert_eq!(verify(&env, &header, &proof), Err(DaError::InvalidProof));

    let mut proof = proof_for(&env, &header, &mmr_root, &sk);
    proof.stake_weights = Vec::from_array(&env, [0u64]);
    assert_eq!(verify(&env, &header, &proof), Err(DaError::InvalidQuorum));
}
