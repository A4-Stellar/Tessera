//! Aggregated zero-knowledge KYC proofs (issue #317).
//!
//! Verifying one KYC proof per investor costs a pairing check each. Instead,
//! an off-chain aggregator (`contracts/zk-aggregator`, the
//! `tessera-zk-aggregator` CLI) collects up to [`MAX_SUBJECTS`] individual
//! credentials from a queue and proves, in **one** constant-size Groth16
//! proof over BN254, that:
//!
//! > for every slot `i < count`, the prover knows an expiry `e_i` and a
//! > Merkle path showing `Poseidon(subject_i, e_i)` is a leaf of the issuer's
//! > credential tree with root `R`, and `e_i > valid_until`.
//!
//! ## The single public input
//!
//! The circuit has exactly one public input, the batch commitment
//!
//! ```text
//! C = SHA-256(R ‖ valid_until ‖ count ‖ s_0 ‖ … ‖ s_{slots-1})   (byte 0 cleared)
//! ```
//!
//! with `R` and every `s_i` as 32-byte big-endian scalars, `valid_until` and
//! `count` as 4-byte big-endian integers, and unused slots as 32 zero bytes.
//! `s_i` is [`subject_field`] of the `i`-th address: SHA-256 of the address's
//! XDR with byte 0 cleared. The contract computes `C` itself from the call
//! arguments and its stored issuer root — the caller never supplies a field
//! element — and the circuit recomputes the same SHA-256 internally, so the
//! proof only verifies for exactly this statement.
//!
//! One public input keeps the cost flat in the batch size: one `g1_mul`, one
//! 4-pair BN254 multi-pairing, and some hashing. A 32-subject submission
//! measures 32.95M CPU instructions, 33% of Soroban's per-transaction limit
//! (`zk_aggregator_test::verification_stays_under_40_percent_of_the_transaction_budget`).
//!
//! ## What the contract checks before the pairing
//!
//! - the verification key is set (`1 ≤ slots ≤ MAX_SUBJECTS`, two IC points);
//! - `issuer` is an authorized ZK issuer and `R` is its current root (so an
//!   issuer revokes credentials by publishing a new root);
//! - `1 ≤ count = subjects.len() ≤ slots`, every subject distinct;
//! - `valid_until` is strictly after the current ledger, i.e. every
//!   credential in the batch is still unexpired now.
//!
//! The circuit has not been audited and the CLI's setup is single-party;
//! see `contracts/zk-aggregator/README.md` before relying on it in
//! production.

use soroban_sdk::{
    contracttype,
    crypto::bn254::{Bn254G1Affine, Bn254G2Affine, Fr},
    panic_with_error,
    xdr::ToXdr,
    Address, Bytes, BytesN, Env, Vec,
};

use crate::{DataKey, Error};

/// Largest batch one aggregated proof may cover.
pub const MAX_SUBJECTS: u32 = 32;
/// How long an accreditation entry stays live in persistent storage.
const ACCREDITATION_TTL: u32 = 120_000;

/// Groth16 proof over BN254, in the Ethereum layout Soroban uses
/// (G1: 64 bytes, G2: 128 bytes).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AggregatedProof {
    pub a: BytesN<64>,
    pub b: BytesN<128>,
    pub c: BytesN<64>,
}

/// Groth16 verification key of the aggregation circuit.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AggregationVerifyingKey {
    /// Circuit slot count; the commitment is padded to this many subjects.
    pub slots: u32,
    pub alpha_g1: BytesN<64>,
    pub beta_g2: BytesN<128>,
    pub gamma_g2: BytesN<128>,
    pub delta_g2: BytesN<128>,
    /// `ic[0]` and `ic[1]`: the circuit has exactly one public input.
    pub ic: Vec<BytesN<64>>,
}

/// Recorded per subject after a successful aggregated submission.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZkAccreditation {
    pub issuer: Address,
    /// Last ledger (exclusive) for which the batch proof vouched.
    pub valid_until: u32,
}

/// The field element an address occupies in the circuit's public inputs.
pub fn subject_field(env: &Env, subject: &Address) -> BytesN<32> {
    let digest: BytesN<32> = env.crypto().sha256(&subject.clone().to_xdr(env)).into();
    let mut bytes = digest.to_array();
    bytes[0] = 0;
    BytesN::from_array(env, &bytes)
}

pub(crate) fn set_vk(env: &Env, vk: &AggregationVerifyingKey) {
    if vk.ic.len() != 2 || vk.slots == 0 || vk.slots > MAX_SUBJECTS {
        panic_with_error!(env, Error::ZkInvalidVerifyingKey);
    }
    env.storage().instance().set(&DataKey::ZkAggVk, vk);
}

pub(crate) fn issuer_root(env: &Env, issuer: &Address) -> Option<BytesN<32>> {
    env.storage()
        .instance()
        .get(&DataKey::ZkIssuerRoot(issuer.clone()))
}

/// Run every check and the pairing; panics with a specific error on any
/// structural failure, returns `false` only if the pairing itself fails.
pub(crate) fn verify(
    env: &Env,
    issuer: &Address,
    subjects: &Vec<Address>,
    valid_until: u32,
    proof: &AggregatedProof,
) -> bool {
    let vk: AggregationVerifyingKey = env
        .storage()
        .instance()
        .get(&DataKey::ZkAggVk)
        .unwrap_or_else(|| panic_with_error!(env, Error::ZkVerifyingKeyNotSet));

    let root = issuer_root(env, issuer)
        .unwrap_or_else(|| panic_with_error!(env, Error::ZkIssuerNotAuthorized));

    let count = subjects.len();
    if count == 0 || count > vk.slots {
        panic_with_error!(env, Error::ZkInvalidBatch);
    }
    if valid_until <= env.ledger().sequence() {
        panic_with_error!(env, Error::ZkProofExpired);
    }

    // C = SHA-256(R ‖ valid_until ‖ count ‖ s_0..s_{slots-1}), byte 0 cleared.
    let mut preimage = Bytes::from_array(env, &root.to_array());
    preimage.extend_from_array(&valid_until.to_be_bytes());
    preimage.extend_from_array(&count.to_be_bytes());
    let mut seen: Vec<BytesN<32>> = Vec::new(env);
    for subject in subjects.iter() {
        let s = subject_field(env, &subject);
        if seen.contains(&s) {
            panic_with_error!(env, Error::ZkInvalidBatch);
        }
        preimage.extend_from_array(&s.to_array());
        seen.push_back(s);
    }
    for _ in count..vk.slots {
        preimage.extend_from_array(&[0u8; 32]);
    }
    let mut c: [u8; 32] = env.crypto().sha256(&preimage).to_array();
    c[0] = 0;

    groth16_verify(
        env,
        &vk,
        proof,
        &Fr::from_bytes(BytesN::from_array(env, &c)),
    )
}

/// `e(A, B) == e(α, β) · e(L, γ) · e(C, δ)` with `L = ic[0] + x · ic[1]`,
/// checked as one multi-pairing `e(-A, B) · e(α, β) · e(L, γ) · e(C, δ) == 1`.
fn groth16_verify(
    env: &Env,
    vk: &AggregationVerifyingKey,
    proof: &AggregatedProof,
    input: &Fr,
) -> bool {
    let bn = env.crypto().bn254();
    let ic1 = Bn254G1Affine::from_bytes(vk.ic.get_unchecked(1));
    let l = bn.g1_add(
        &Bn254G1Affine::from_bytes(vk.ic.get_unchecked(0)),
        &bn.g1_mul(&ic1, input),
    );

    let neg_a = -Bn254G1Affine::from_bytes(proof.a.clone());
    let g1: Vec<Bn254G1Affine> = Vec::from_array(
        env,
        [
            neg_a,
            Bn254G1Affine::from_bytes(vk.alpha_g1.clone()),
            l,
            Bn254G1Affine::from_bytes(proof.c.clone()),
        ],
    );
    let g2: Vec<Bn254G2Affine> = Vec::from_array(
        env,
        [
            Bn254G2Affine::from_bytes(proof.b.clone()),
            Bn254G2Affine::from_bytes(vk.beta_g2.clone()),
            Bn254G2Affine::from_bytes(vk.gamma_g2.clone()),
            Bn254G2Affine::from_bytes(vk.delta_g2.clone()),
        ],
    );
    bn.pairing_check(g1, g2)
}

pub(crate) fn record(env: &Env, issuer: &Address, subjects: &Vec<Address>, valid_until: u32) {
    for subject in subjects.iter() {
        let key = DataKey::ZkAccreditation(subject);
        env.storage().persistent().set(
            &key,
            &ZkAccreditation {
                issuer: issuer.clone(),
                valid_until,
            },
        );
        env.storage()
            .persistent()
            .extend_ttl(&key, ACCREDITATION_TTL, ACCREDITATION_TTL);
    }
}

pub(crate) fn accreditation(env: &Env, subject: &Address) -> Option<ZkAccreditation> {
    env.storage()
        .persistent()
        .get(&DataKey::ZkAccreditation(subject.clone()))
}
