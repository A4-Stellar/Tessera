//! On-chain verifier for aggregated Bulletproofs range proofs
//! (Bünz et al., "Bulletproofs: Short Proofs for Confidential Transactions
//! and More", sections 4.2 and 6) over BLS12-381 G1.
//!
//! A proof shows that each of `m` Pedersen commitments `V_j = v_j*G + r_j*H`
//! opens to a value in `[0, 2^n)` without revealing `v_j`. Verification
//! follows the single multi-scalar-multiplication form used by
//! dalek-cryptography/bulletproofs: the range-proof polynomial check and the
//! inner-product argument are merged with a batching scalar `c` and checked
//! with one `g1_msm` against the identity. Challenges come from a SHA-256
//! Fiat-Shamir transcript that is bound to a caller-supplied context (the
//! contract, parties, nonce and current balance for a transfer), so a proof
//! cannot be replayed or redirected.

use soroban_sdk::crypto::bls12_381::{Fr, G1Affine};
use soroban_sdk::{contracttype, Bytes, BytesN, Env, Vec};

use crate::pedersen::{self, fr_neg, fr_u64, Point};

const TRANSCRIPT_DOMAIN: &[u8] = b"tessera.confidential-token.rangeproof.v1";

/// A (possibly aggregated) range proof. Scalars are canonical big-endian
/// field elements; points are uncompressed G1 encodings.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RangeProof {
    pub a: Point,
    pub s: Point,
    pub t1: Point,
    pub t2: Point,
    /// `t(x)`, the evaluation of the inner-product polynomial.
    pub t_x: BytesN<32>,
    /// Blinding of `t(x)` (`tau_x`).
    pub t_x_blinding: BytesN<32>,
    /// Blinding of `A + x*S` (`mu`).
    pub e_blinding: BytesN<32>,
    /// Inner-product argument round commitments, `log2(n*m)` of each.
    pub l_vec: Vec<Point>,
    pub r_vec: Vec<Point>,
    /// Final folded scalars of the inner-product argument.
    pub ipp_a: BytesN<32>,
    pub ipp_b: BytesN<32>,
}

/// Vector generators `G_i` and `H_i` (uncompressed encodings).
pub struct Generators {
    pub g: Vec<Point>,
    pub h: Vec<Point>,
}

/// Why a proof was rejected. Only surfaced as a single contract error, but
/// kept distinct for tests and debugging.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProofError {
    /// Wrong number of commitments, IPA rounds or generators.
    Shape,
    /// A scalar is not a canonical field element.
    NonCanonicalScalar,
    /// A Fiat-Shamir challenge was zero (negligible probability).
    ZeroChallenge,
    /// The verification equation does not hold.
    Rejected,
}

/// SHA-256 Fiat-Shamir transcript. Every absorb hashes the running state with
/// a label and a length-prefixed payload, so messages cannot be re-split.
pub struct Transcript {
    env: Env,
    state: BytesN<32>,
}

impl Transcript {
    pub fn new(env: &Env, context: &Bytes, n: u32, m: u32) -> Self {
        let state = env
            .crypto()
            .sha256(&Bytes::from_slice(env, TRANSCRIPT_DOMAIN))
            .to_bytes();
        let mut t = Self {
            env: env.clone(),
            state,
        };
        t.append(b"n", &Bytes::from_array(env, &n.to_be_bytes()));
        t.append(b"m", &Bytes::from_array(env, &m.to_be_bytes()));
        t.append(b"context", context);
        t
    }

    pub fn append(&mut self, label: &[u8], data: &Bytes) {
        let mut buf = Bytes::from_array(&self.env, &self.state.to_array());
        buf.extend_from_slice(label);
        buf.extend_from_array(&data.len().to_be_bytes());
        buf.append(data);
        self.state = self.env.crypto().sha256(&buf).to_bytes();
    }

    pub fn append_point(&mut self, label: &[u8], p: &Point) {
        self.append(label, &Bytes::from(p.clone()));
    }

    pub fn append_scalar(&mut self, label: &[u8], s: &BytesN<32>) {
        self.append(label, &Bytes::from(s.clone()));
    }

    pub fn challenge(&mut self, label: &[u8]) -> Fr {
        self.append(label, &Bytes::new(&self.env));
        Fr::from_bytes(self.state.clone())
    }
}

/// Parse a scalar, rejecting non-canonical encodings (`>= r`) so a proof has
/// exactly one byte representation.
pub fn canonical_scalar(bytes: &BytesN<32>) -> Result<Fr, ProofError> {
    let fr = Fr::from_bytes(bytes.clone());
    if fr.to_bytes() == *bytes {
        Ok(fr)
    } else {
        Err(ProofError::NonCanonicalScalar)
    }
}

pub fn is_zero(env: &Env, fr: &Fr) -> bool {
    *fr == fr_u64(env, 0)
}

/// A verified-shape proof reduced to one multi-scalar multiplication that
/// must equal the identity. Scalars are ordered `G_0..G_{size-1}`,
/// `H_0..H_{size-1}`, then one per entry of `points`.
///
/// The MSM is linear, so it can be evaluated in chunks (possibly across
/// transactions) and the partial sums added: every Fiat-Shamir challenge is
/// already fixed here, so chunking does not weaken soundness.
pub struct PreparedCheck {
    pub size: u32,
    pub points: Vec<Point>,
    pub scalars: Vec<Fr>,
}

/// `sum(scalars[k] * term[start + k])` over a prepared check's MSM terms.
pub fn msm_chunk(
    env: &Env,
    gens: &Generators,
    size: u32,
    points: &Vec<Point>,
    start: u32,
    scalars: Vec<Fr>,
) -> G1Affine {
    let mut vp: Vec<G1Affine> = Vec::new(env);
    for i in start..start + scalars.len() {
        let p = if i < size {
            gens.g.get_unchecked(i)
        } else if i < 2 * size {
            gens.h.get_unchecked(i - size)
        } else {
            points.get_unchecked(i - 2 * size)
        };
        vp.push_back(G1Affine::from_bytes(p));
    }
    env.crypto().bls12_381().g1_msm(vp, scalars)
}

/// Verify that every commitment in `commitments` opens to a value in
/// `[0, 2^n)`, in a single MSM.
pub fn verify(
    env: &Env,
    gens: &Generators,
    commitments: &Vec<Point>,
    proof: &RangeProof,
    n: u32,
    context: &Bytes,
) -> Result<(), ProofError> {
    let check = prepare(
        env,
        gens.g.len().min(gens.h.len()),
        commitments,
        proof,
        n,
        context,
    )?;
    let result = msm_chunk(env, gens, check.size, &check.points, 0, check.scalars);
    if result.to_bytes() == pedersen::identity(env) {
        Ok(())
    } else {
        Err(ProofError::Rejected)
    }
}

/// Check the proof's shape and encodings, replay the transcript and compute
/// every scalar of the final verification MSM.
///
/// Points are not subgroup-checked here: the host's `g1_msm` rejects any
/// input outside the prime-order subgroup, so an invalid point makes the
/// evaluation trap and the transaction fail.
pub fn prepare(
    env: &Env,
    generator_count: u32,
    commitments: &Vec<Point>,
    proof: &RangeProof,
    n: u32,
    context: &Bytes,
) -> Result<PreparedCheck, ProofError> {
    let bls = env.crypto().bls12_381();
    let m = commitments.len();
    if m == 0 || !m.is_power_of_two() || n == 0 || n > 64 || !n.is_power_of_two() {
        return Err(ProofError::Shape);
    }
    let size = n * m;
    let lg_size = size.trailing_zeros();
    if generator_count < size || proof.l_vec.len() != lg_size || proof.r_vec.len() != lg_size {
        return Err(ProofError::Shape);
    }

    let t_x = canonical_scalar(&proof.t_x)?;
    let t_x_blinding = canonical_scalar(&proof.t_x_blinding)?;
    let e_blinding = canonical_scalar(&proof.e_blinding)?;
    let a = canonical_scalar(&proof.ipp_a)?;
    let b = canonical_scalar(&proof.ipp_b)?;

    // Replay the prover's transcript.
    let mut transcript = Transcript::new(env, context, n, m);
    for v in commitments.iter() {
        transcript.append_point(b"V", &v);
    }
    transcript.append_point(b"A", &proof.a);
    transcript.append_point(b"S", &proof.s);
    let y = transcript.challenge(b"y");
    let z = transcript.challenge(b"z");
    transcript.append_point(b"T1", &proof.t1);
    transcript.append_point(b"T2", &proof.t2);
    let x = transcript.challenge(b"x");
    transcript.append_scalar(b"t_x", &proof.t_x);
    transcript.append_scalar(b"t_x_blinding", &proof.t_x_blinding);
    transcript.append_scalar(b"e_blinding", &proof.e_blinding);
    let w = transcript.challenge(b"w");

    let mut u_sq = Vec::new(env);
    let mut u_inv_sq = Vec::new(env);
    let mut all_inv = fr_u64(env, 1);
    for (l, r) in proof.l_vec.iter().zip(proof.r_vec.iter()) {
        transcript.append_point(b"L", &l);
        transcript.append_point(b"R", &r);
        let u = transcript.challenge(b"u");
        if is_zero(env, &u) {
            return Err(ProofError::ZeroChallenge);
        }
        let u_inv = bls.fr_inv(&u);
        u_sq.push_back(bls.fr_mul(&u, &u));
        u_inv_sq.push_back(bls.fr_mul(&u_inv, &u_inv));
        all_inv = bls.fr_mul(&all_inv, &u_inv);
    }
    // Batching scalar, derived only after every proof element is absorbed.
    let c = transcript.challenge(b"c");
    if is_zero(env, &y) || is_zero(env, &z) || is_zero(env, &x) {
        return Err(ProofError::ZeroChallenge);
    }

    // s_i = prod_j u_j^{+1 or -1} according to the bits of i (IPA folding).
    let mut s = Vec::new(env);
    s.push_back(all_inv);
    for i in 1..size {
        let lg_i = 31 - i.leading_zeros();
        let k = 1u32 << lg_i;
        let u_sq_lg = u_sq.get_unchecked(lg_size - 1 - lg_i);
        s.push_back(bls.fr_mul(&s.get_unchecked(i - k), &u_sq_lg));
    }

    let zz = bls.fr_mul(&z, &z);
    let minus_z = fr_neg(env, &z);
    let y_inv = bls.fr_inv(&y);
    let two = fr_u64(env, 2);

    let mut g_scalars: Vec<Fr> = Vec::new(env);
    let mut h_scalars: Vec<Fr> = Vec::new(env);
    let mut points: Vec<Point> = Vec::new(env);
    let mut extra_scalars: Vec<Fr> = Vec::new(env);
    let mut push = |p: &Point, k: Fr| {
        points.push_back(p.clone());
        extra_scalars.push_back(k);
    };

    // Vector generators.
    let mut y_inv_pow = fr_u64(env, 1);
    let mut z_pow_j = zz.clone(); // z^{2+j}
    let mut sum_y = fr_u64(env, 0); // sum_{i<size} y^i
    let mut y_pow = fr_u64(env, 1);
    for j in 0..m {
        let mut two_pow = fr_u64(env, 1);
        for k in 0..n {
            let i = j * n + k;
            let a_s = bls.fr_mul(&a, &s.get_unchecked(i));
            g_scalars.push_back(bls.fr_sub(&minus_z, &a_s));

            let b_s_inv = bls.fr_mul(&b, &s.get_unchecked(size - 1 - i));
            let z_two = bls.fr_mul(&z_pow_j, &two_pow);
            let inner = bls.fr_mul(&y_inv_pow, &bls.fr_sub(&z_two, &b_s_inv));
            h_scalars.push_back(bls.fr_add(&z, &inner));

            sum_y = bls.fr_add(&sum_y, &y_pow);
            y_pow = bls.fr_mul(&y_pow, &y);
            y_inv_pow = bls.fr_mul(&y_inv_pow, &y_inv);
            two_pow = bls.fr_mul(&two_pow, &two);
        }
        z_pow_j = bls.fr_mul(&z_pow_j, &z);
    }

    // delta(y, z) = (z - z^2) * <1, y^size> - z^3 * <1, 2^n> * sum_{j<m} z^j
    let sum_two = bls.fr_sub(&bls.fr_pow(&two, u64::from(n)), &fr_u64(env, 1));
    let mut sum_z = fr_u64(env, 0);
    let mut z_pow = fr_u64(env, 1);
    for _ in 0..m {
        sum_z = bls.fr_add(&sum_z, &z_pow);
        z_pow = bls.fr_mul(&z_pow, &z);
    }
    let zzz = bls.fr_mul(&zz, &z);
    let delta = bls.fr_sub(
        &bls.fr_mul(&bls.fr_sub(&z, &zz), &sum_y),
        &bls.fr_mul(&bls.fr_mul(&zzz, &sum_two), &sum_z),
    );

    // Value commitments: c * z^{2+j}.
    let mut z_pow_j = zz.clone();
    for v in commitments.iter() {
        push(&v, bls.fr_mul(&c, &z_pow_j));
        z_pow_j = bls.fr_mul(&z_pow_j, &z);
    }

    let cx = bls.fr_mul(&c, &x);
    push(&proof.a, fr_u64(env, 1));
    push(&proof.s, x.clone());
    push(&proof.t1, cx.clone());
    push(&proof.t2, bls.fr_mul(&cx, &x));
    for (l, (k_l, (r, k_r))) in proof
        .l_vec
        .iter()
        .zip(u_sq.iter().zip(proof.r_vec.iter().zip(u_inv_sq.iter())))
    {
        push(&l, k_l);
        push(&r, k_r);
    }

    // Blinding base H: -mu - c * tau_x.
    let h_scalar = fr_neg(
        env,
        &bls.fr_add(&e_blinding, &bls.fr_mul(&c, &t_x_blinding)),
    );
    push(&pedersen::blinding_base(env).to_bytes(), h_scalar);
    // Value base G: w * (t_x - a*b) + c * (delta - t_x).
    let g_scalar = bls.fr_add(
        &bls.fr_mul(&w, &bls.fr_sub(&t_x, &bls.fr_mul(&a, &b))),
        &bls.fr_mul(&c, &bls.fr_sub(&delta, &t_x)),
    );
    push(&pedersen::value_base(env).to_bytes(), g_scalar);

    let mut scalars = g_scalars;
    scalars.append(&h_scalars);
    scalars.append(&extra_scalars);
    Ok(PreparedCheck {
        size,
        points,
        scalars,
    })
}
