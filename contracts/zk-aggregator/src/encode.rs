//! Byte encodings shared with the Soroban compliance contract.
//!
//! - Scalars: 32-byte big-endian (`soroban_sdk::crypto::bn254::Fr`).
//! - G1: `be(x) ‖ be(y)`, 64 bytes; G2: `be(x.c1) ‖ be(x.c0) ‖ be(y.c1) ‖
//!   be(y.c0)`, 128 bytes; the point at infinity is all zeros. This is the
//!   Ethereum (EIP-196/197) layout Soroban's BN254 host functions consume.
//! - Subjects: `subject_field(address)` = SHA-256 of the address's `ScVal`
//!   XDR with the top byte cleared, matching
//!   `tessera_compliance::zk_aggregator::subject_field`.

use ark_bn254::{Bn254, Fq, Fq2, Fr, G1Affine, G2Affine};
use ark_ec::AffineRepr;
use ark_ff::{BigInteger, PrimeField};
use ark_groth16::{Proof, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use stellar_strkey::Strkey;

/// `ScValType::SCV_ADDRESS`.
const SCV_ADDRESS: u32 = 18;
const SC_ADDRESS_TYPE_ACCOUNT: u32 = 0;
const SC_ADDRESS_TYPE_CONTRACT: u32 = 1;
const PUBLIC_KEY_TYPE_ED25519: u32 = 0;

/// XDR of `ScVal::Address` for a `G…` or `C…` strkey.
pub fn address_scval_xdr(strkey: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(44);
    out.extend_from_slice(&SCV_ADDRESS.to_be_bytes());
    match Strkey::from_string(strkey).ok()? {
        Strkey::PublicKeyEd25519(pk) => {
            out.extend_from_slice(&SC_ADDRESS_TYPE_ACCOUNT.to_be_bytes());
            out.extend_from_slice(&PUBLIC_KEY_TYPE_ED25519.to_be_bytes());
            out.extend_from_slice(&pk.0);
        }
        Strkey::Contract(c) => {
            out.extend_from_slice(&SC_ADDRESS_TYPE_CONTRACT.to_be_bytes());
            out.extend_from_slice(&c.0);
        }
        _ => return None,
    }
    Some(out)
}

/// The circuit's public-input value for `strkey`.
pub fn subject_field(strkey: &str) -> Option<Fr> {
    let xdr = address_scval_xdr(strkey)?;
    let mut digest: [u8; 32] = Sha256::digest(&xdr).into();
    digest[0] = 0;
    Some(Fr::from_be_bytes_mod_order(&digest))
}

pub fn fr_to_bytes(f: &Fr) -> [u8; 32] {
    let bytes = f.into_bigint().to_bytes_be();
    let mut out = [0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(&bytes);
    out
}

pub fn fr_to_hex(f: &Fr) -> String {
    hex::encode(fr_to_bytes(f))
}

/// Parse a canonical big-endian scalar (rejects values >= the modulus).
pub fn fr_from_hex(h: &str) -> Option<Fr> {
    let bytes: [u8; 32] = hex::decode(h).ok()?.try_into().ok()?;
    let f = Fr::from_be_bytes_mod_order(&bytes);
    (fr_to_bytes(&f) == bytes).then_some(f)
}

fn fq_be(f: &Fq) -> [u8; 32] {
    let bytes = f.into_bigint().to_bytes_be();
    let mut out = [0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(&bytes);
    out
}

pub fn g1_bytes(p: &G1Affine) -> [u8; 64] {
    let mut out = [0u8; 64];
    if let Some((x, y)) = p.xy() {
        out[..32].copy_from_slice(&fq_be(x));
        out[32..].copy_from_slice(&fq_be(y));
    }
    out
}

pub fn g2_bytes(p: &G2Affine) -> [u8; 128] {
    let mut out = [0u8; 128];
    if let Some((x, y)) = p.xy() {
        out[..32].copy_from_slice(&fq_be(&x.c1));
        out[32..64].copy_from_slice(&fq_be(&x.c0));
        out[64..96].copy_from_slice(&fq_be(&y.c1));
        out[96..].copy_from_slice(&fq_be(&y.c0));
    }
    out
}

pub fn g1_hex(p: &G1Affine) -> String {
    hex::encode(g1_bytes(p))
}

pub fn g2_hex(p: &G2Affine) -> String {
    hex::encode(g2_bytes(p))
}

fn fq_from(bytes: &[u8]) -> Option<Fq> {
    let f = Fq::from_be_bytes_mod_order(bytes);
    (fq_be(&f) == bytes).then_some(f)
}

/// Parse and validate (on curve, in subgroup) a G1 point.
pub fn g1_from_hex(h: &str) -> Option<G1Affine> {
    let b: [u8; 64] = hex::decode(h).ok()?.try_into().ok()?;
    if b.iter().all(|x| *x == 0) {
        return Some(G1Affine::zero());
    }
    let p = G1Affine::new_unchecked(fq_from(&b[..32])?, fq_from(&b[32..])?);
    (p.is_on_curve() && p.is_in_correct_subgroup_assuming_on_curve()).then_some(p)
}

/// Parse and validate (on curve, in subgroup) a G2 point.
pub fn g2_from_hex(h: &str) -> Option<G2Affine> {
    let b: [u8; 128] = hex::decode(h).ok()?.try_into().ok()?;
    if b.iter().all(|x| *x == 0) {
        return Some(G2Affine::zero());
    }
    let x = Fq2::new(fq_from(&b[32..64])?, fq_from(&b[..32])?);
    let y = Fq2::new(fq_from(&b[96..])?, fq_from(&b[64..96])?);
    let p = G2Affine::new_unchecked(x, y);
    (p.is_on_curve() && p.is_in_correct_subgroup_assuming_on_curve()).then_some(p)
}

/// `AggregationVerifyingKey` in the contract's argument shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SorobanVerifyingKey {
    /// Circuit slot count; the contract pads the commitment to this many.
    pub slots: u32,
    pub alpha_g1: String,
    pub beta_g2: String,
    pub gamma_g2: String,
    pub delta_g2: String,
    /// `ic[0]` and `ic[1]`: the circuit has exactly one public input.
    pub ic: Vec<String>,
}

impl SorobanVerifyingKey {
    pub fn new(vk: &VerifyingKey<Bn254>, slots: usize) -> Self {
        SorobanVerifyingKey {
            slots: slots as u32,
            alpha_g1: g1_hex(&vk.alpha_g1),
            beta_g2: g2_hex(&vk.beta_g2),
            gamma_g2: g2_hex(&vk.gamma_g2),
            delta_g2: g2_hex(&vk.delta_g2),
            ic: vk.gamma_abc_g1.iter().map(g1_hex).collect(),
        }
    }

    /// Back to an arkworks key, validating every point.
    pub fn to_arkworks(&self) -> Option<VerifyingKey<Bn254>> {
        Some(VerifyingKey {
            alpha_g1: g1_from_hex(&self.alpha_g1)?,
            beta_g2: g2_from_hex(&self.beta_g2)?,
            gamma_g2: g2_from_hex(&self.gamma_g2)?,
            delta_g2: g2_from_hex(&self.delta_g2)?,
            gamma_abc_g1: self
                .ic
                .iter()
                .map(|p| g1_from_hex(p))
                .collect::<Option<_>>()?,
        })
    }
}

/// `AggregatedProof` in the contract's argument shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SorobanProof {
    pub a: String,
    pub b: String,
    pub c: String,
}

impl From<&Proof<Bn254>> for SorobanProof {
    fn from(p: &Proof<Bn254>) -> Self {
        SorobanProof {
            a: g1_hex(&p.a),
            b: g2_hex(&p.b),
            c: g1_hex(&p.c),
        }
    }
}

impl SorobanProof {
    /// Back to an arkworks proof, validating every point.
    pub fn to_arkworks(&self) -> Option<Proof<Bn254>> {
        Some(Proof {
            a: g1_from_hex(&self.a)?,
            b: g2_from_hex(&self.b)?,
            c: g1_from_hex(&self.c)?,
        })
    }
}

/// Everything `submit_aggregated_kyc` needs, ready to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofBundle {
    /// Issuer root the proof was made against (big-endian scalar hex).
    pub issuer_root: String,
    pub valid_until: u32,
    /// Subjects, in the order the contract must receive them.
    pub subjects: Vec<String>,
    pub proof: SorobanProof,
}
