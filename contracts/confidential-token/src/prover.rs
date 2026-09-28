//! Reference Bulletproofs prover matching [`crate::bulletproofs::verify`].
//!
//! Compiled only for tests and the `testutils` feature. It runs on the
//! Soroban host's BLS12-381 functions, so wallets and SDKs can use it
//! off-chain through a testutils `Env`, or treat it as the specification for
//! a native prover. Blinding factors are derived from `seed`, which must be
//! 32 secret, uniformly random bytes that are never reused.

use soroban_sdk::crypto::bls12_381::{Fr, G1Affine};
use soroban_sdk::{Bytes, BytesN, Env, Vec};

use crate::bulletproofs::{Generators, RangeProof, Transcript};
use crate::pedersen::{self, fr_u64, Point};

/// Deterministic scalar stream derived from a secret seed.
struct Rng {
    env: Env,
    seed: BytesN<32>,
    counter: u32,
}

impl Rng {
    fn scalar(&mut self) -> Fr {
        let mut buf = Bytes::from_array(&self.env, &self.seed.to_array());
        buf.extend_from_array(&self.counter.to_be_bytes());
        self.counter += 1;
        Fr::from_bytes(self.env.crypto().sha256(&buf).to_bytes())
    }
}

fn inner_product(env: &Env, a: &Vec<Fr>, b: &Vec<Fr>) -> Fr {
    let bls = env.crypto().bls12_381();
    let mut acc = fr_u64(env, 0);
    for (x, y) in a.iter().zip(b.iter()) {
        acc = bls.fr_add(&acc, &bls.fr_mul(&x, &y));
    }
    acc
}

fn msm(env: &Env, points: &Vec<G1Affine>, scalars: &Vec<Fr>) -> G1Affine {
    env.crypto()
        .bls12_381()
        .g1_msm(points.clone(), scalars.clone())
}

/// Low 64 bits of a scalar. For an in-range value this is the value itself;
/// for anything else the bit decomposition is wrong and the proof fails.
fn low_bits(value: &Fr) -> u64 {
    let bytes = value.to_bytes().to_array();
    u64::from_be_bytes(bytes[24..32].try_into().unwrap())
}

/// Prove that each `values[j]` (committed with `blindings[j]`) lies in
/// `[0, 2^n)`. Returns the proof and the commitments it is about.
pub fn prove(
    env: &Env,
    gens: &Generators,
    values: &Vec<Fr>,
    blindings: &Vec<Fr>,
    n: u32,
    context: &Bytes,
    seed: &BytesN<32>,
) -> (RangeProof, Vec<Point>) {
    let bls = env.crypto().bls12_381();
    let m = values.len();
    let size = n * m;
    let mut rng = Rng {
        env: env.clone(),
        seed: seed.clone(),
        counter: 0,
    };
    let one = fr_u64(env, 1);
    let g_base = pedersen::value_base(env);
    let h_base = pedersen::blinding_base(env);

    let mut commitments = Vec::new(env);
    for (v, r) in values.iter().zip(blindings.iter()) {
        commitments.push_back(pedersen::commit(env, &v, &r).to_bytes());
    }

    let g_vec: Vec<G1Affine> = {
        let mut out = Vec::new(env);
        for p in gens.g.slice(0..size).iter() {
            out.push_back(G1Affine::from_bytes(p));
        }
        out
    };
    let h_vec: Vec<G1Affine> = {
        let mut out = Vec::new(env);
        for p in gens.h.slice(0..size).iter() {
            out.push_back(G1Affine::from_bytes(p));
        }
        out
    };

    // Bit decomposition a_L and a_R = a_L - 1.
    let mut a_l = Vec::new(env);
    let mut a_r = Vec::new(env);
    for v in values.iter() {
        let bits = low_bits(&v);
        for k in 0..n {
            let bit = fr_u64(env, (bits >> k) & 1);
            a_r.push_back(bls.fr_sub(&bit, &one));
            a_l.push_back(bit);
        }
    }

    let with_blinding = |blind: &Fr, left: &Vec<Fr>, right: &Vec<Fr>| -> Point {
        let mut pts = Vec::new(env);
        let mut sc = Vec::new(env);
        pts.push_back(h_base.clone());
        sc.push_back(blind.clone());
        pts.append(&g_vec);
        sc.append(left);
        pts.append(&h_vec);
        sc.append(right);
        msm(env, &pts, &sc).to_bytes()
    };

    let alpha = rng.scalar();
    let a_commit = with_blinding(&alpha, &a_l, &a_r);
    let mut s_l = Vec::new(env);
    let mut s_r = Vec::new(env);
    for _ in 0..size {
        s_l.push_back(rng.scalar());
        s_r.push_back(rng.scalar());
    }
    let rho = rng.scalar();
    let s_commit = with_blinding(&rho, &s_l, &s_r);

    let mut transcript = Transcript::new(env, context, n, m);
    for v in commitments.iter() {
        transcript.append_point(b"V", &v);
    }
    transcript.append_point(b"A", &a_commit);
    transcript.append_point(b"S", &s_commit);
    let y = transcript.challenge(b"y");
    let z = transcript.challenge(b"z");

    // l(X) = l0 + l1 X, r(X) = r0 + r1 X.
    let zz = bls.fr_mul(&z, &z);
    let two = fr_u64(env, 2);
    let mut l0 = Vec::new(env);
    let mut r0 = Vec::new(env);
    let mut r1 = Vec::new(env);
    let mut y_pow = one.clone();
    let mut z_pow_j = zz.clone();
    for j in 0..m {
        let mut two_pow = one.clone();
        for k in 0..n {
            let i = j * n + k;
            l0.push_back(bls.fr_sub(&a_l.get_unchecked(i), &z));
            let r0_i = bls.fr_add(
                &bls.fr_mul(&y_pow, &bls.fr_add(&a_r.get_unchecked(i), &z)),
                &bls.fr_mul(&z_pow_j, &two_pow),
            );
            r0.push_back(r0_i);
            r1.push_back(bls.fr_mul(&y_pow, &s_r.get_unchecked(i)));
            y_pow = bls.fr_mul(&y_pow, &y);
            two_pow = bls.fr_mul(&two_pow, &two);
        }
        z_pow_j = bls.fr_mul(&z_pow_j, &z);
    }
    let l1 = s_l;
    let t1 = bls.fr_add(&inner_product(env, &l0, &r1), &inner_product(env, &l1, &r0));
    let t2 = inner_product(env, &l1, &r1);

    let tau1 = rng.scalar();
    let tau2 = rng.scalar();
    let t1_commit = pedersen::commit(env, &t1, &tau1).to_bytes();
    let t2_commit = pedersen::commit(env, &t2, &tau2).to_bytes();
    transcript.append_point(b"T1", &t1_commit);
    transcript.append_point(b"T2", &t2_commit);
    let x = transcript.challenge(b"x");

    // tau_x = tau2 x^2 + tau1 x + sum_j z^{2+j} gamma_j,  mu = alpha + rho x.
    let mut tau_x = bls.fr_add(
        &bls.fr_mul(&tau2, &bls.fr_mul(&x, &x)),
        &bls.fr_mul(&tau1, &x),
    );
    let mut z_pow_j = zz.clone();
    for gamma in blindings.iter() {
        tau_x = bls.fr_add(&tau_x, &bls.fr_mul(&z_pow_j, &gamma));
        z_pow_j = bls.fr_mul(&z_pow_j, &z);
    }
    let mu = bls.fr_add(&alpha, &bls.fr_mul(&rho, &x));

    let mut l = Vec::new(env);
    let mut r = Vec::new(env);
    for i in 0..size {
        l.push_back(bls.fr_add(&l0.get_unchecked(i), &bls.fr_mul(&l1.get_unchecked(i), &x)));
        r.push_back(bls.fr_add(&r0.get_unchecked(i), &bls.fr_mul(&r1.get_unchecked(i), &x)));
    }
    let t_x = inner_product(env, &l, &r);

    transcript.append_scalar(b"t_x", &t_x.to_bytes());
    transcript.append_scalar(b"t_x_blinding", &tau_x.to_bytes());
    transcript.append_scalar(b"e_blinding", &mu.to_bytes());
    let w = transcript.challenge(b"w");
    let q = bls.g1_mul(&g_base, &w);

    // Inner-product argument on (G, H' = y^{-i} H_i, Q).
    let y_inv = bls.fr_inv(&y);
    let mut h_prime = Vec::new(env);
    let mut y_inv_pow = one.clone();
    for h in h_vec.iter() {
        h_prime.push_back(bls.g1_mul(&h, &y_inv_pow));
        y_inv_pow = bls.fr_mul(&y_inv_pow, &y_inv);
    }
    let (mut a, mut b, mut g, mut h) = (l, r, g_vec, h_prime);
    let mut l_vec = Vec::new(env);
    let mut r_vec = Vec::new(env);
    let mut len = size;
    while len > 1 {
        len /= 2;
        let (a_lo, a_hi) = (a.slice(0..len), a.slice(len..2 * len));
        let (b_lo, b_hi) = (b.slice(0..len), b.slice(len..2 * len));
        let (g_lo, g_hi) = (g.slice(0..len), g.slice(len..2 * len));
        let (h_lo, h_hi) = (h.slice(0..len), h.slice(len..2 * len));
        let c_l = inner_product(env, &a_lo, &b_hi);
        let c_r = inner_product(env, &a_hi, &b_lo);

        let fold_commit =
            |sa: &Vec<Fr>, pg: &Vec<G1Affine>, sb: &Vec<Fr>, ph: &Vec<G1Affine>, c: &Fr| {
                let mut pts = pg.clone();
                pts.append(ph);
                pts.push_back(q.clone());
                let mut sc = sa.clone();
                sc.append(sb);
                sc.push_back(c.clone());
                msm(env, &pts, &sc).to_bytes()
            };
        let l_commit = fold_commit(&a_lo, &g_hi, &b_hi, &h_lo, &c_l);
        let r_commit = fold_commit(&a_hi, &g_lo, &b_lo, &h_hi, &c_r);
        transcript.append_point(b"L", &l_commit);
        transcript.append_point(b"R", &r_commit);
        l_vec.push_back(l_commit);
        r_vec.push_back(r_commit);
        let u = transcript.challenge(b"u");
        let u_inv = bls.fr_inv(&u);

        let (mut na, mut nb, mut ng, mut nh) =
            (Vec::new(env), Vec::new(env), Vec::new(env), Vec::new(env));
        for i in 0..len {
            na.push_back(bls.fr_add(
                &bls.fr_mul(&a_lo.get_unchecked(i), &u),
                &bls.fr_mul(&a_hi.get_unchecked(i), &u_inv),
            ));
            nb.push_back(bls.fr_add(
                &bls.fr_mul(&b_lo.get_unchecked(i), &u_inv),
                &bls.fr_mul(&b_hi.get_unchecked(i), &u),
            ));
            let mut pg = Vec::new(env);
            pg.push_back(g_lo.get_unchecked(i));
            pg.push_back(g_hi.get_unchecked(i));
            let mut sg = Vec::new(env);
            sg.push_back(u_inv.clone());
            sg.push_back(u.clone());
            ng.push_back(msm(env, &pg, &sg));
            let mut ph = Vec::new(env);
            ph.push_back(h_lo.get_unchecked(i));
            ph.push_back(h_hi.get_unchecked(i));
            let mut sh = Vec::new(env);
            sh.push_back(u.clone());
            sh.push_back(u_inv.clone());
            nh.push_back(msm(env, &ph, &sh));
        }
        (a, b, g, h) = (na, nb, ng, nh);
    }

    let proof = RangeProof {
        a: a_commit,
        s: s_commit,
        t1: t1_commit,
        t2: t2_commit,
        t_x: t_x.to_bytes(),
        t_x_blinding: tau_x.to_bytes(),
        e_blinding: mu.to_bytes(),
        l_vec,
        r_vec,
        ipp_a: a.get_unchecked(0).to_bytes(),
        ipp_b: b.get_unchecked(0).to_bytes(),
    };
    (proof, commitments)
}
