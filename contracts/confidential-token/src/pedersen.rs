//! Pedersen commitments `C = v*G + r*H` over the BLS12-381 G1 group.
//!
//! `G` is the standard G1 generator and every other generator (`H` and the
//! Bulletproofs vector generators) is derived with the host's RFC 9380
//! `hash_to_g1`, so nobody knows a discrete-log relation between them. That
//! is what makes the commitments binding: opening `C` to two different values
//! would reveal `log_G(H)`.

use soroban_sdk::crypto::bls12_381::{Fr, G1Affine};
use soroban_sdk::{Bytes, BytesN, Env, Vec, U256};

/// Uncompressed G1 point encoding used for every stored commitment.
pub type Point = BytesN<96>;

/// Domain separation tag for all hash-to-curve derivations in this contract.
const DST: &[u8] = b"TESSERA-CONFIDENTIAL-TOKEN-V1_BLS12381G1_XMD:SHA-256_SSWU_RO_";

/// The standard BLS12-381 G1 generator (uncompressed, big-endian).
const G1_GENERATOR: [u8; 96] = [
    0x17, 0xf1, 0xd3, 0xa7, 0x31, 0x97, 0xd7, 0x94, 0x26, 0x95, 0x63, 0x8c, //
    0x4f, 0xa9, 0xac, 0x0f, 0xc3, 0x68, 0x8c, 0x4f, 0x97, 0x74, 0xb9, 0x05, //
    0xa1, 0x4e, 0x3a, 0x3f, 0x17, 0x1b, 0xac, 0x58, 0x6c, 0x55, 0xe8, 0x3f, //
    0xf9, 0x7a, 0x1a, 0xef, 0xfb, 0x3a, 0xf0, 0x0a, 0xdb, 0x22, 0xc6, 0xbb, //
    0x08, 0xb3, 0xf4, 0x81, 0xe3, 0xaa, 0xa0, 0xf1, 0xa0, 0x9e, 0x30, 0xed, //
    0x74, 0x1d, 0x8a, 0xe4, 0xfc, 0xf5, 0xe0, 0x95, 0xd5, 0xd0, 0x0a, 0xf6, //
    0x00, 0xdb, 0x18, 0xcb, 0x2c, 0x04, 0xb3, 0xed, 0xd0, 0x3c, 0xc7, 0x44, //
    0xa2, 0x88, 0x8a, 0xe4, 0x0c, 0xaa, 0x23, 0x29, 0x46, 0xc5, 0xe7, 0xe1,
];

/// Encoding of the point at infinity (only the infinity flag set). This is
/// the commitment to `0` with blinding `0`, i.e. an empty balance.
pub fn identity(env: &Env) -> Point {
    let mut bytes = [0u8; 96];
    bytes[0] = 0x40;
    BytesN::from_array(env, &bytes)
}

/// Value base `G`.
pub fn value_base(env: &Env) -> G1Affine {
    G1Affine::from_bytes(BytesN::from_array(env, &G1_GENERATOR))
}

/// Blinding base `H`.
pub fn blinding_base(env: &Env) -> G1Affine {
    hash_to_point(env, &Bytes::from_slice(env, b"pedersen/H"))
}

/// Bulletproofs vector generator `G_i` (`kind = b'G'`) or `H_i` (`kind = b'H'`).
pub fn vector_generator(env: &Env, kind: u8, index: u32) -> G1Affine {
    let mut msg = Bytes::from_slice(env, b"bulletproofs/");
    msg.push_back(kind);
    msg.extend_from_array(&index.to_be_bytes());
    hash_to_point(env, &msg)
}

fn hash_to_point(env: &Env, msg: &Bytes) -> G1Affine {
    env.crypto()
        .bls12_381()
        .hash_to_g1(msg, &Bytes::from_slice(env, DST))
}

/// `v*G + r*H`.
pub fn commit(env: &Env, value: &Fr, blinding: &Fr) -> G1Affine {
    let mut points = Vec::new(env);
    points.push_back(value_base(env));
    points.push_back(blinding_base(env));
    let mut scalars = Vec::new(env);
    scalars.push_back(value.clone());
    scalars.push_back(blinding.clone());
    env.crypto().bls12_381().g1_msm(points, scalars)
}

/// `a + b` (homomorphic addition of the committed values and blindings).
pub fn add(env: &Env, a: &Point, b: &Point) -> Point {
    env.crypto()
        .bls12_381()
        .g1_add(
            &G1Affine::from_bytes(a.clone()),
            &G1Affine::from_bytes(b.clone()),
        )
        .to_bytes()
}

/// `a - b`. Computed as an MSM with scalar `-1` rather than through the SDK's
/// point negation, which does not special-case the point at infinity.
pub fn sub(env: &Env, a: &Point, b: &Point) -> Point {
    let mut points = Vec::new(env);
    points.push_back(G1Affine::from_bytes(a.clone()));
    points.push_back(G1Affine::from_bytes(b.clone()));
    let mut scalars = Vec::new(env);
    scalars.push_back(fr_u64(env, 1));
    scalars.push_back(fr_neg(env, &fr_u64(env, 1)));
    env.crypto().bls12_381().g1_msm(points, scalars).to_bytes()
}

/// `amount * G`: a commitment to a public amount with zero blinding.
pub fn public_amount(env: &Env, amount: u64) -> Point {
    env.crypto()
        .bls12_381()
        .g1_mul(&value_base(env), &fr_u64(env, amount))
        .to_bytes()
}

/// Whether `p` decodes to a point in the prime-order G1 subgroup. Rejecting
/// everything else rules out small-subgroup and invalid-curve inputs.
pub fn is_valid_point(env: &Env, p: &Point) -> bool {
    env.crypto()
        .bls12_381()
        .g1_is_in_subgroup(&G1Affine::from_bytes(p.clone()))
}

pub fn fr_u64(env: &Env, value: u64) -> Fr {
    Fr::from_u256(U256::from_u128(env, u128::from(value)))
}

pub fn fr_neg(env: &Env, value: &Fr) -> Fr {
    env.crypto().bls12_381().fr_sub(&fr_u64(env, 0), value)
}
