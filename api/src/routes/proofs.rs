//! Light-client MMR proofs (issue #318).
//!
//! - `GET /v1/proofs/mmr` — the current MMR root and leaf count.
//! - `GET /v1/proofs/mmr/:ledger_seq?account=:address` — the checkpoint for
//!   `ledger_seq` with its MMR inclusion path, plus (when `account` is given)
//!   every cap-table entry of that account at that ledger with its path to
//!   the checkpoint's `contract_state_root`.
//!
//! See `crate::indexer::mmr_checkpoint` for the hashing rules, and
//! `sdk/typescript/src/mmr.ts` for the client-side verifier.

use ark_bls12_381::{
    g1::Config as BlsG1Config, Fq as BlsFq, Fr as BlsFr, G1Affine as BlsG1Affine,
    G1Projective as BlsG1Projective,
};
use ark_bn254::{Bn254, Fq, Fq2, Fr, G1Affine, G2Affine};
use ark_ec::{
    hashing::{curve_maps::wb::WBMap, map_to_curve_hasher::MapToCurveBasedHasher, HashToCurve},
    AffineRepr,
};
use ark_ff::{field_hashers::DefaultFieldHasher, BigInteger, Field, One, PrimeField, Zero};
use ark_groth16::{Groth16, Proof, VerifyingKey};
use ark_snark::SNARK;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use super::ApiError;
use crate::indexer::mmr_checkpoint::{
    hex32, mmr_leaf_hash, AccountProof, MmrProofResponse, MmrRootResponse, StateEntryProof,
};
use crate::indexer::AppState;

/// `proof_bytes` for `balance_range` is the concatenation of the contract
/// `RangeProof` fields in declaration order: A, S, T1, T2 (96-byte points),
/// t_x, t_x_blinding, e_blinding (32-byte scalars), six L points, six R
/// points, then ipp_a and ipp_b (32-byte scalars). This is the contract's
/// uncompressed BLS12-381 point and canonical big-endian scalar format for
/// the one-commitment, 64-bit balance proof.
///
/// `proof_bytes` for `compliance_status` is hex-encoded JSON for the
/// `SorobanProof { a, b, c }` emitted by `tessera-zk-aggregator`. Its
/// `public_inputs` are `{issuer_root, valid_until, subjects}`; the configured
/// aggregator verification key supplies the circuit slot count.
///
/// The range verifier ports the contract's BLS12-381 Bulletproof equation and
/// RFC 9380 generator derivation. It intentionally does not use
/// `ark-bulletproofs`, whose proof format is incompatible. Ownership remains
/// unsupported until its statement and key format are defined.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofType {
    BalanceRange,
    Ownership,
    ComplianceStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RangePublicInputs {
    /// BLS12-381 G1 commitment encoded as 96-byte Soroban point hex.
    pub(super) commitment: String,
    /// Exact contract transcript context, encoded as hex XDR bytes.
    pub(super) context: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyProofRequest {
    pub proof_type: ProofType,
    /// Hex-encoded bytes. For compliance this is JSON-serialized SorobanProof.
    pub proof_bytes: String,
    pub public_inputs: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct VerifyProofResponse {
    pub asset_id: String,
    pub proof_type: ProofType,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<RangeBounds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RangeBounds {
    pub min: u64,
    pub max: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompliancePublicInputs {
    issuer_root: String,
    valid_until: u32,
    subjects: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AggregatorProof {
    a: String,
    b: String,
    c: String,
}

#[derive(Debug, Deserialize)]
struct AggregatorVerifyingKey {
    slots: u32,
    alpha_g1: String,
    beta_g2: String,
    gamma_g2: String,
    delta_g2: String,
    ic: Vec<String>,
}

const MAX_PROOF_BYTES: usize = 64 * 1024;
const MAX_RANGE_CONTEXT_BYTES: usize = 4096;
const MAX_AGGREGATOR_SLOTS: u32 = 32;
const VERIFYING_KEY_ENV: &str = "TESSERA_ZK_AGGREGATOR_VERIFYING_KEY";
const PROOF_REQUEST_INTERVAL: Duration = Duration::from_millis(100);
static LAST_PROOF_REQUEST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
const RANGE_BITS: u32 = 64;
const RANGE_TRANSCRIPT_DOMAIN: &[u8] = b"tessera.confidential-token.rangeproof.v1";
const RANGE_HASH_TO_G1_DST: &[u8] =
    b"TESSERA-CONFIDENTIAL-TOKEN-V1_BLS12381G1_XMD:SHA-256_SSWU_RO_";

type BlsHashToCurve =
    MapToCurveBasedHasher<BlsG1Projective, DefaultFieldHasher<Sha256, 128>, WBMap<BlsG1Config>>;

/// Verify an asset proof. Router wiring is intentionally left to the caller.
pub async fn verify(
    Path(asset_id): Path<String>,
    Json(request): Json<VerifyProofRequest>,
) -> Result<Response, ApiError> {
    if asset_id.is_empty() || asset_id.len() > 128 {
        return Err(ApiError::BadRequest("invalid asset id".into()));
    }
    if !allow_proof_request() {
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": "rate_limited",
                "message": "proof verification is limited to 10 requests per second"
            })),
        )
            .into_response());
    }

    match request.proof_type {
        ProofType::BalanceRange => {
            let proof_bytes = decode_proof_bytes(&request.proof_bytes)?;
            let public_inputs: RangePublicInputs = serde_json::from_value(request.public_inputs)
                .map_err(|_| ApiError::BadRequest("invalid balance_range public_inputs".into()))?;
            let valid = verify_native_range(&public_inputs, &proof_bytes)?;
            Ok(Json(VerifyProofResponse {
                asset_id,
                proof_type: ProofType::BalanceRange,
                valid,
                range: valid.then_some(RangeBounds {
                    min: 0,
                    max: u64::MAX,
                }),
                reason: None,
            })
            .into_response())
        }
        ProofType::Ownership => Ok(unsupported_response(
            asset_id,
            ProofType::Ownership,
            "ownership proof verification is deferred until its proof statement and verification-key format are defined",
        )),
        ProofType::ComplianceStatus => {
            let proof_bytes = decode_proof_bytes(&request.proof_bytes)?;
            let public_inputs: CompliancePublicInputs =
                serde_json::from_value(request.public_inputs)
                    .map_err(|_| ApiError::BadRequest("invalid compliance public_inputs".into()))?;
            let proof: AggregatorProof = serde_json::from_slice(&proof_bytes)
                .map_err(|_| ApiError::BadRequest("invalid aggregator proof JSON".into()))?;

            let key_path = std::env::var(VERIFYING_KEY_ENV).map_err(|_| {
                ApiError::Unavailable(format!(
                    "set {VERIFYING_KEY_ENV} to the zk-aggregator verifying_key.json output"
                ))
            })?;
            let key_json = tokio::fs::read_to_string(&key_path)
                .await
                .map_err(|_| {
                    ApiError::Unavailable("cannot read zk-aggregator verification key".into())
                })?;
            let encoded_key: AggregatorVerifyingKey = serde_json::from_str(&key_json)
                .map_err(|_| {
                    ApiError::Unavailable("invalid zk-aggregator verification key JSON".into())
                })?;
            let (key, slots) = encoded_key
                .to_arkworks()
                .ok_or_else(|| {
                    ApiError::Unavailable("invalid zk-aggregator verification key points".into())
                })?;
            let proof = proof
                .to_arkworks()
                .ok_or_else(|| ApiError::BadRequest("invalid aggregator proof points".into()))?;
            let public_input = compliance_commitment(&public_inputs, slots)?;
            let valid = Groth16::<Bn254>::verify(&key, &[public_input], &proof)
                .map_err(|_| ApiError::BadRequest("aggregator proof verification failed".into()))?;

            Ok(Json(VerifyProofResponse {
                asset_id,
                proof_type: ProofType::ComplianceStatus,
                valid,
                range: None,
                reason: None,
            })
            .into_response())
        }
    }
}

fn allow_proof_request() -> bool {
    let last = LAST_PROOF_REQUEST.get_or_init(|| Mutex::new(None));
    let mut last = last
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    if last.is_some_and(|previous| now.duration_since(previous) < PROOF_REQUEST_INTERVAL) {
        return false;
    }
    *last = Some(now);
    true
}

fn unsupported_response(asset_id: String, proof_type: ProofType, reason: &str) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(VerifyProofResponse {
            asset_id,
            proof_type,
            valid: false,
            range: None,
            reason: Some(reason.to_string()),
        }),
    )
        .into_response()
}

fn decode_proof_bytes(encoded: &str) -> Result<Vec<u8>, ApiError> {
    let bytes =
        hex::decode(encoded).map_err(|_| ApiError::BadRequest("proof_bytes must be hex".into()))?;
    if bytes.is_empty() || bytes.len() > MAX_PROOF_BYTES {
        return Err(ApiError::BadRequest(
            "proof_bytes must contain 1..=65536 bytes".into(),
        ));
    }
    Ok(bytes)
}

struct NativeRangeProof {
    a_bytes: [u8; 96],
    s_bytes: [u8; 96],
    t1_bytes: [u8; 96],
    t2_bytes: [u8; 96],
    t_x_bytes: [u8; 32],
    t_x_blinding_bytes: [u8; 32],
    e_blinding_bytes: [u8; 32],
    l_bytes: Vec<[u8; 96]>,
    r_bytes: Vec<[u8; 96]>,
    ipp_a_bytes: [u8; 32],
    ipp_b_bytes: [u8; 32],
}

pub(super) fn verify_native_range(
    inputs: &RangePublicInputs,
    proof_bytes: &[u8],
) -> Result<bool, ApiError> {
    let commitment_bytes: [u8; 96] = hex::decode(&inputs.commitment)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| ApiError::BadRequest("commitment must be a 96-byte point in hex".into()))?;
    let commitment = bls_point_from_bytes(&commitment_bytes)
        .ok_or_else(|| ApiError::BadRequest("invalid BLS12-381 commitment point".into()))?;
    let context = hex::decode(&inputs.context)
        .map_err(|_| ApiError::BadRequest("context must be hex".into()))?;
    if context.len() > MAX_RANGE_CONTEXT_BYTES {
        return Err(ApiError::BadRequest("context exceeds 4096 bytes".into()));
    }

    let proof = parse_native_range_proof(proof_bytes)?;
    let point = |bytes: &[u8; 96]| {
        bls_point_from_bytes(bytes)
            .ok_or_else(|| ApiError::BadRequest("invalid BLS12-381 proof point".into()))
    };
    let a_point = point(&proof.a_bytes)?;
    let s_point = point(&proof.s_bytes)?;
    let t1_point = point(&proof.t1_bytes)?;
    let t2_point = point(&proof.t2_bytes)?;
    let l_points = proof
        .l_bytes
        .iter()
        .map(point)
        .collect::<Result<Vec<_>, _>>()?;
    let r_points = proof
        .r_bytes
        .iter()
        .map(point)
        .collect::<Result<Vec<_>, _>>()?;

    let t_x = bls_scalar_from_bytes(&proof.t_x_bytes)
        .ok_or_else(|| ApiError::BadRequest("non-canonical t_x scalar".into()))?;
    let t_x_blinding = bls_scalar_from_bytes(&proof.t_x_blinding_bytes)
        .ok_or_else(|| ApiError::BadRequest("non-canonical t_x_blinding scalar".into()))?;
    let e_blinding = bls_scalar_from_bytes(&proof.e_blinding_bytes)
        .ok_or_else(|| ApiError::BadRequest("non-canonical e_blinding scalar".into()))?;
    let ipp_a = bls_scalar_from_bytes(&proof.ipp_a_bytes)
        .ok_or_else(|| ApiError::BadRequest("non-canonical ipp_a scalar".into()))?;
    let ipp_b = bls_scalar_from_bytes(&proof.ipp_b_bytes)
        .ok_or_else(|| ApiError::BadRequest("non-canonical ipp_b scalar".into()))?;

    let hasher = BlsHashToCurve::new(RANGE_HASH_TO_G1_DST)
        .map_err(|_| ApiError::Unavailable("cannot initialize BLS12-381 hash-to-curve".into()))?;
    let value_base = BlsG1Affine::generator();
    let blinding_base = derive_bls_point(&hasher, b"pedersen/H")?;
    let mut g_generators = Vec::with_capacity(RANGE_BITS as usize);
    let mut h_generators = Vec::with_capacity(RANGE_BITS as usize);
    for i in 0..RANGE_BITS {
        let mut g_message = b"bulletproofs/".to_vec();
        g_message.push(b'G');
        g_message.extend_from_slice(&i.to_be_bytes());
        g_generators.push(derive_bls_point(&hasher, &g_message)?);
        let mut h_message = b"bulletproofs/".to_vec();
        h_message.push(b'H');
        h_message.extend_from_slice(&i.to_be_bytes());
        h_generators.push(derive_bls_point(&hasher, &h_message)?);
    }

    let mut transcript = RangeTranscript::new(&context);
    transcript.append_point(b"V", &commitment_bytes);
    transcript.append_point(b"A", &proof.a_bytes);
    transcript.append_point(b"S", &proof.s_bytes);
    let y = transcript.challenge(b"y");
    let z = transcript.challenge(b"z");
    transcript.append_point(b"T1", &proof.t1_bytes);
    transcript.append_point(b"T2", &proof.t2_bytes);
    let x = transcript.challenge(b"x");
    transcript.append_scalar(b"t_x", &proof.t_x_bytes);
    transcript.append_scalar(b"t_x_blinding", &proof.t_x_blinding_bytes);
    transcript.append_scalar(b"e_blinding", &proof.e_blinding_bytes);
    let w = transcript.challenge(b"w");

    let mut u_squared = Vec::with_capacity(proof.l_bytes.len());
    let mut u_inverse_squared = Vec::with_capacity(proof.l_bytes.len());
    let mut all_inverse = BlsFr::one();
    for (l_bytes, r_bytes) in proof.l_bytes.iter().zip(&proof.r_bytes) {
        transcript.append_point(b"L", l_bytes);
        transcript.append_point(b"R", r_bytes);
        let u = transcript.challenge(b"u");
        let Some(u_inverse) = u.inverse() else {
            return Ok(false);
        };
        u_squared.push(u.square());
        u_inverse_squared.push(u_inverse.square());
        all_inverse *= u_inverse;
    }
    let c = transcript.challenge(b"c");
    if y.is_zero() || z.is_zero() || x.is_zero() {
        return Ok(false);
    }

    let mut s = Vec::with_capacity(RANGE_BITS as usize);
    s.push(all_inverse);
    let log_size = RANGE_BITS.trailing_zeros();
    for i in 1..RANGE_BITS {
        let log_i = 31 - i.leading_zeros();
        let block = 1u32 << log_i;
        s.push(s[(i - block) as usize] * u_squared[(log_size - 1 - log_i) as usize]);
    }

    let two = BlsFr::from(2u64);
    let zz = z.square();
    let zzz = zz * z;
    let y_inverse = y
        .inverse()
        .ok_or_else(|| ApiError::BadRequest("zero Fiat-Shamir challenge in range proof".into()))?;
    let minus_z = -z;
    let mut g_scalars = Vec::with_capacity(RANGE_BITS as usize);
    let mut h_scalars = Vec::with_capacity(RANGE_BITS as usize);
    let mut y_inverse_power = BlsFr::one();
    let mut y_power = BlsFr::one();
    let mut two_power = BlsFr::one();
    let mut sum_y = BlsFr::zero();
    for i in 0..RANGE_BITS as usize {
        let a_s = ipp_a * s[i];
        g_scalars.push(minus_z - a_s);

        let b_s_inverse = ipp_b * s[RANGE_BITS as usize - 1 - i];
        let inner = y_inverse_power * (zz * two_power - b_s_inverse);
        h_scalars.push(z + inner);

        sum_y += y_power;
        y_power *= y;
        y_inverse_power *= y_inverse;
        two_power *= two;
    }

    let sum_two = two.pow([RANGE_BITS as u64]) - BlsFr::one();
    let sum_z = BlsFr::one();
    let delta = (z - zz) * sum_y - zzz * sum_two * sum_z;

    let mut check = BlsG1Projective::zero();
    for (generator, scalar) in g_generators.iter().zip(&g_scalars) {
        check += generator.mul_bigint(scalar.into_bigint());
    }
    for (generator, scalar) in h_generators.iter().zip(&h_scalars) {
        check += generator.mul_bigint(scalar.into_bigint());
    }

    let mut add_term = |point: &BlsG1Affine, scalar: BlsFr| {
        check += point.mul_bigint(scalar.into_bigint());
    };
    add_term(&commitment, c * zz);
    add_term(&a_point, BlsFr::one());
    add_term(&s_point, x);
    let cx = c * x;
    add_term(&t1_point, cx);
    add_term(&t2_point, cx * x);
    for ((l, r), (u2, u_inverse2)) in l_points
        .iter()
        .zip(&r_points)
        .zip(u_squared.iter().zip(&u_inverse_squared))
    {
        add_term(l, *u2);
        add_term(r, *u_inverse2);
    }
    add_term(&blinding_base, -(e_blinding + c * t_x_blinding));
    add_term(&value_base, w * (t_x - ipp_a * ipp_b) + c * (delta - t_x));

    Ok(check.is_zero())
}

fn parse_native_range_proof(bytes: &[u8]) -> Result<NativeRangeProof, ApiError> {
    let rounds = RANGE_BITS.trailing_zeros() as usize;
    let expected_len = 4 * 96 + 3 * 32 + 2 * rounds * 96 + 2 * 32;
    if bytes.len() != expected_len {
        return Err(ApiError::BadRequest(format!(
            "balance_range proof must be exactly {expected_len} bytes for a single 64-bit commitment"
        )));
    }
    let mut offset = 0;
    let a_bytes = take_bytes::<96>(bytes, &mut offset);
    let s_bytes = take_bytes::<96>(bytes, &mut offset);
    let t1_bytes = take_bytes::<96>(bytes, &mut offset);
    let t2_bytes = take_bytes::<96>(bytes, &mut offset);
    let t_x_bytes = take_bytes::<32>(bytes, &mut offset);
    let t_x_blinding_bytes = take_bytes::<32>(bytes, &mut offset);
    let e_blinding_bytes = take_bytes::<32>(bytes, &mut offset);
    let l_bytes = (0..rounds)
        .map(|_| take_bytes::<96>(bytes, &mut offset))
        .collect::<Option<Vec<_>>>();
    let r_bytes = (0..rounds)
        .map(|_| take_bytes::<96>(bytes, &mut offset))
        .collect::<Option<Vec<_>>>();
    let ipp_a_bytes = take_bytes::<32>(bytes, &mut offset);
    let ipp_b_bytes = take_bytes::<32>(bytes, &mut offset);

    match (
        a_bytes,
        s_bytes,
        t1_bytes,
        t2_bytes,
        t_x_bytes,
        t_x_blinding_bytes,
        e_blinding_bytes,
        l_bytes,
        r_bytes,
        ipp_a_bytes,
        ipp_b_bytes,
    ) {
        (
            Some(a_bytes),
            Some(s_bytes),
            Some(t1_bytes),
            Some(t2_bytes),
            Some(t_x_bytes),
            Some(t_x_blinding_bytes),
            Some(e_blinding_bytes),
            Some(l_bytes),
            Some(r_bytes),
            Some(ipp_a_bytes),
            Some(ipp_b_bytes),
        ) if offset == bytes.len() => Ok(NativeRangeProof {
            a_bytes,
            s_bytes,
            t1_bytes,
            t2_bytes,
            t_x_bytes,
            t_x_blinding_bytes,
            e_blinding_bytes,
            l_bytes,
            r_bytes,
            ipp_a_bytes,
            ipp_b_bytes,
        }),
        _ => Err(ApiError::BadRequest("malformed balance_range proof".into())),
    }
}

fn take_bytes<const N: usize>(bytes: &[u8], offset: &mut usize) -> Option<[u8; N]> {
    let end = offset.checked_add(N)?;
    let value = bytes.get(*offset..end)?.try_into().ok()?;
    *offset = end;
    Some(value)
}

fn bls_point_from_bytes(bytes: &[u8; 96]) -> Option<BlsG1Affine> {
    if bytes[0] == 0x40 && bytes[1..].iter().all(|byte| *byte == 0) {
        return Some(BlsG1Affine::zero());
    }
    let x = bls_fq_from_bytes(&bytes[..48])?;
    let y = bls_fq_from_bytes(&bytes[48..])?;
    let point = BlsG1Affine::new_unchecked(x, y);
    (point.is_on_curve() && point.is_in_correct_subgroup_assuming_on_curve()).then_some(point)
}

fn bls_fq_from_bytes(bytes: &[u8]) -> Option<BlsFq> {
    let value = BlsFq::from_be_bytes_mod_order(bytes);
    (field_bytes::<48, BlsFq>(&value).as_slice() == bytes).then_some(value)
}

fn bls_scalar_from_bytes(bytes: &[u8; 32]) -> Option<BlsFr> {
    let value = BlsFr::from_be_bytes_mod_order(bytes);
    (field_bytes::<32, BlsFr>(&value) == *bytes).then_some(value)
}

fn field_bytes<const N: usize, F: PrimeField>(value: &F) -> [u8; N] {
    let bytes = value.into_bigint().to_bytes_be();
    let mut out = [0; N];
    out[N - bytes.len()..].copy_from_slice(&bytes);
    out
}

fn derive_bls_point(hasher: &BlsHashToCurve, message: &[u8]) -> Result<BlsG1Affine, ApiError> {
    hasher
        .hash(message)
        .map_err(|_| ApiError::Unavailable("BLS12-381 hash-to-curve failed".into()))
}

struct RangeTranscript {
    state: [u8; 32],
}

impl RangeTranscript {
    fn new(context: &[u8]) -> Self {
        let mut transcript = Self {
            state: Sha256::digest(RANGE_TRANSCRIPT_DOMAIN).into(),
        };
        transcript.append(b"n", &RANGE_BITS.to_be_bytes());
        transcript.append(b"m", &1u32.to_be_bytes());
        transcript.append(b"context", context);
        transcript
    }

    fn append(&mut self, label: &[u8], data: &[u8]) {
        let mut hash = Sha256::new();
        hash.update(self.state);
        hash.update(label);
        hash.update((data.len() as u32).to_be_bytes());
        hash.update(data);
        self.state = hash.finalize().into();
    }

    fn append_point(&mut self, label: &[u8], point: &[u8; 96]) {
        self.append(label, point);
    }

    fn append_scalar(&mut self, label: &[u8], scalar: &[u8; 32]) {
        self.append(label, scalar);
    }

    fn challenge(&mut self, label: &[u8]) -> BlsFr {
        self.append(label, &[]);
        BlsFr::from_be_bytes_mod_order(&self.state)
    }
}

impl AggregatorVerifyingKey {
    fn to_arkworks(&self) -> Option<(VerifyingKey<Bn254>, usize)> {
        if self.slots == 0 || self.slots > MAX_AGGREGATOR_SLOTS || self.ic.len() != 2 {
            return None;
        }
        Some((
            VerifyingKey {
                alpha_g1: g1_from_hex(&self.alpha_g1)?,
                beta_g2: g2_from_hex(&self.beta_g2)?,
                gamma_g2: g2_from_hex(&self.gamma_g2)?,
                delta_g2: g2_from_hex(&self.delta_g2)?,
                gamma_abc_g1: self
                    .ic
                    .iter()
                    .map(|point| g1_from_hex(point))
                    .collect::<Option<_>>()?,
            },
            self.slots as usize,
        ))
    }
}

impl AggregatorProof {
    fn to_arkworks(&self) -> Option<Proof<Bn254>> {
        Some(Proof {
            a: g1_from_hex(&self.a)?,
            b: g2_from_hex(&self.b)?,
            c: g1_from_hex(&self.c)?,
        })
    }
}

fn compliance_commitment(inputs: &CompliancePublicInputs, slots: usize) -> Result<Fr, ApiError> {
    if inputs.subjects.len() > slots {
        return Err(ApiError::BadRequest(
            "more subjects than the verification key's circuit slots".into(),
        ));
    }

    let root = scalar_from_hex(&inputs.issuer_root)
        .ok_or_else(|| ApiError::BadRequest("issuer_root is not a canonical scalar".into()))?;
    let root_bytes = scalar_bytes(&root);
    let mut hash = Sha256::new();
    hash.update(root_bytes);
    hash.update(inputs.valid_until.to_be_bytes());
    hash.update((inputs.subjects.len() as u32).to_be_bytes());
    for subject in &inputs.subjects {
        let subject = subject_field(subject)
            .ok_or_else(|| ApiError::BadRequest("invalid subject address".into()))?;
        hash.update(scalar_bytes(&subject));
    }
    for _ in inputs.subjects.len()..slots {
        hash.update([0u8; 32]);
    }
    let mut digest: [u8; 32] = hash.finalize().into();
    digest[0] = 0;
    Ok(Fr::from_be_bytes_mod_order(&digest))
}

fn scalar_from_hex(encoded: &str) -> Option<Fr> {
    let bytes: [u8; 32] = hex::decode(encoded).ok()?.try_into().ok()?;
    let value = Fr::from_be_bytes_mod_order(&bytes);
    (scalar_bytes(&value) == bytes).then_some(value)
}

fn scalar_bytes(value: &Fr) -> [u8; 32] {
    let bytes = value.into_bigint().to_bytes_be();
    let mut out = [0; 32];
    out[32 - bytes.len()..].copy_from_slice(&bytes);
    out
}

fn subject_field(address: &str) -> Option<Fr> {
    const SCV_ADDRESS: u32 = 18;
    const SC_ADDRESS_TYPE_ACCOUNT: u32 = 0;
    const SC_ADDRESS_TYPE_CONTRACT: u32 = 1;
    const PUBLIC_KEY_TYPE_ED25519: u32 = 0;

    let mut xdr = Vec::with_capacity(44);
    xdr.extend_from_slice(&SCV_ADDRESS.to_be_bytes());
    match stellar_strkey::Strkey::from_string(address).ok()? {
        stellar_strkey::Strkey::PublicKeyEd25519(key) => {
            xdr.extend_from_slice(&SC_ADDRESS_TYPE_ACCOUNT.to_be_bytes());
            xdr.extend_from_slice(&PUBLIC_KEY_TYPE_ED25519.to_be_bytes());
            xdr.extend_from_slice(&key.0);
        }
        stellar_strkey::Strkey::Contract(contract) => {
            xdr.extend_from_slice(&SC_ADDRESS_TYPE_CONTRACT.to_be_bytes());
            xdr.extend_from_slice(&contract.0);
        }
        _ => return None,
    }
    let mut digest: [u8; 32] = Sha256::digest(&xdr).into();
    digest[0] = 0;
    Some(Fr::from_be_bytes_mod_order(&digest))
}

fn fq_from_be(bytes: &[u8]) -> Option<Fq> {
    let value = Fq::from_be_bytes_mod_order(bytes);
    let canonical = value.into_bigint().to_bytes_be();
    let mut encoded = vec![0; 32];
    encoded[32 - canonical.len()..].copy_from_slice(&canonical);
    (encoded == bytes).then_some(value)
}

fn g1_from_hex(encoded: &str) -> Option<G1Affine> {
    let bytes = hex::decode(encoded).ok()?;
    if bytes.len() != 64 {
        return None;
    }
    if bytes.iter().all(|byte| *byte == 0) {
        return Some(G1Affine::zero());
    }
    let point = G1Affine::new_unchecked(fq_from_be(&bytes[..32])?, fq_from_be(&bytes[32..])?);
    (point.is_on_curve() && point.is_in_correct_subgroup_assuming_on_curve()).then_some(point)
}

fn g2_from_hex(encoded: &str) -> Option<G2Affine> {
    let bytes = hex::decode(encoded).ok()?;
    if bytes.len() != 128 {
        return None;
    }
    if bytes.iter().all(|byte| *byte == 0) {
        return Some(G2Affine::zero());
    }
    let c1 = fq_from_be(&bytes[..32])?;
    let c0 = fq_from_be(&bytes[32..64])?;
    let y1 = fq_from_be(&bytes[64..96])?;
    let y0 = fq_from_be(&bytes[96..])?;
    let point = G2Affine::new_unchecked(Fq2::new(c0, c1), Fq2::new(y0, y1));
    (point.is_on_curve() && point.is_in_correct_subgroup_assuming_on_curve()).then_some(point)
}

#[derive(Debug, Deserialize)]
pub struct ProofQuery {
    pub account: Option<String>,
}

pub async fn root(State(state): State<AppState>) -> Json<MmrRootResponse> {
    let mmr = state.mmr.read().expect("mmr lock");
    Json(MmrRootResponse {
        leaf_count: mmr.leaf_count(),
        root: hex32(&mmr.root()),
        latest_ledger_sequence: mmr.latest().map(|cp| cp.ledger_sequence),
    })
}

pub async fn proof(
    State(state): State<AppState>,
    Path(ledger_seq): Path<u32>,
    Query(query): Query<ProofQuery>,
) -> Result<Json<MmrProofResponse>, ApiError> {
    let mmr = state.mmr.read().expect("mmr lock");
    let (cp, proof) = mmr
        .proof(ledger_seq)
        .ok_or_else(|| ApiError::NotFound(format!("no MMR checkpoint for ledger {ledger_seq}")))?;

    let account = match query.account {
        None => None,
        Some(address) => {
            if address.is_empty() || address.len() > 128 {
                return Err(ApiError::BadRequest("invalid account".into()));
            }
            let tree = mmr.state(ledger_seq).ok_or_else(|| {
                ApiError::NotFound(format!(
                    "cap-table state for ledger {ledger_seq} is past the retention window; \
                     only the MMR inclusion proof is available"
                ))
            })?;
            let entries = tree
                .proofs_for(&address)
                .into_iter()
                .map(|(entry, leaf_index, path)| StateEntryProof {
                    asset_id: entry.asset_id,
                    address: entry.address,
                    balance: entry.balance.to_string(),
                    leaf_index,
                    path: path.iter().map(hex32).collect(),
                })
                .collect();
            Some(AccountProof {
                address,
                state_leaf_count: tree.len() as u64,
                entries,
            })
        }
    };

    Ok(Json(MmrProofResponse {
        ledger_sequence: cp.ledger_sequence,
        ledger_hash: hex32(&cp.ledger_hash),
        contract_state_root: hex32(&cp.state_root),
        leaf_hash: hex32(&mmr_leaf_hash(
            cp.ledger_sequence,
            &cp.ledger_hash,
            &cp.state_root,
        )),
        leaf_index: proof.leaf_index,
        leaf_count: proof.leaf_count,
        path: proof.path.iter().map(hex32).collect(),
        peaks: proof.peaks.iter().map(hex32).collect(),
        root: hex32(&mmr.root()),
        account,
    }))
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use super::{verify, ProofType, VerifyProofRequest};
    use crate::indexer::mmr_checkpoint::{StateEntry, StateTree};
    use crate::indexer::Snapshot;
    use crate::routes::{router, test_support::state_with};

    async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let res = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    #[tokio::test]
    async fn serves_checkpoint_and_account_proofs() {
        let state = state_with(Snapshot::default());
        {
            let mut mmr = state.mmr.write().unwrap();
            for seq in 1..=5u32 {
                let tree = StateTree::new(vec![
                    StateEntry {
                        asset_id: 1,
                        address: "GALICE".into(),
                        balance: 10 * seq as i128,
                    },
                    StateEntry {
                        asset_id: 1,
                        address: "GBOB".into(),
                        balance: 3,
                    },
                ]);
                mmr.record(seq, [seq as u8; 32], tree).unwrap();
            }
        }
        let app = router(state);

        let (status, root) = get_json(app.clone(), "/v1/proofs/mmr").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(root["leaf_count"], 5);
        assert_eq!(root["latest_ledger_sequence"], 5);

        let (status, body) = get_json(app.clone(), "/v1/proofs/mmr/3?account=GALICE").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ledger_sequence"], 3);
        assert_eq!(body["leaf_index"], 2);
        assert_eq!(body["root"], root["root"]);
        assert_eq!(body["account"]["entries"][0]["balance"], "30");
        assert_eq!(body["account"]["state_leaf_count"], 2);

        let (status, body) = get_json(app.clone(), "/v1/proofs/mmr/3").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.get("account").is_none());

        let (status, _) = get_json(app, "/v1/proofs/mmr/99").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn malformed_balance_range_proof_is_rejected() {
        let result = verify(
            axum::extract::Path("asset-1".to_string()),
            axum::Json(VerifyProofRequest {
                proof_type: ProofType::BalanceRange,
                proof_bytes: "00".into(),
                public_inputs: serde_json::json!({
                    "commitment": format!("40{}", "00".repeat(95)),
                    "context": ""
                }),
            }),
        )
        .await;
        assert!(result.is_err());
    }

    #[test]
    fn aggregator_point_decoders_reject_malformed_encodings() {
        assert!(super::g1_from_hex("00").is_none());
        assert!(super::g2_from_hex("00").is_none());
        assert!(super::g1_from_hex(&"00".repeat(64)).is_some());
        assert!(super::g2_from_hex(&"00".repeat(128)).is_some());
    }
}
