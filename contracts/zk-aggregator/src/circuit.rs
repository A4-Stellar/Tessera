//! The aggregation circuit.
//!
//! **Public input:** one scalar, the batch commitment
//! `C = SHA-256(root ‖ valid_until ‖ count ‖ s_0 ‖ … ‖ s_{slots-1})` with the
//! top byte cleared (see [`crate::credentials::commitment`]).
//!
//! **Private witnesses:** `root`, `valid_until`, `count`, every slot's subject
//! `s_i`, plus for each slot an `active_i` bit, the credential's `expiry_i`,
//! and its Merkle path (sibling hashes + direction bits).
//!
//! The circuit enforces:
//!
//! - the SHA-256 of the private values' canonical big-endian bytes equals
//!   `C`, so they are exactly what the contract committed to;
//! - `active` is a prefix (`active_i` implies `active_{i-1}`) and
//!   `Σ active_i = count`, so exactly the first `count` slots are live;
//! - an inactive slot's subject is `0` (the contract zero-pads too);
//! - an active slot's leaf `Poseidon(s_i, expiry_i)` hashes up its path to
//!   `root`;
//! - an active slot's `expiry_i > valid_until`.
//!
//! Distinctness of subjects is not enforced in-circuit: the contract derives
//! the subjects from distinct addresses before it computes `C`.

use std::cmp::Ordering;

use ark_bn254::Fr;
use ark_crypto_primitives::crh::sha256::constraints::Sha256Gadget;
use ark_crypto_primitives::sponge::{
    constraints::CryptographicSpongeVar,
    poseidon::{constraints::PoseidonSpongeVar, PoseidonConfig},
};
use ark_ff::Zero;
use ark_r1cs_std::{
    alloc::AllocVar,
    boolean::Boolean,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
    select::CondSelectGadget,
    uint32::UInt32,
    uint8::UInt8,
    ToBitsGadget, ToBytesGadget,
};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

use crate::credentials::Accumulator;
use crate::encode::fr_from_hex;

#[derive(Clone)]
pub struct AggregationCircuit {
    pub config: PoseidonConfig<Fr>,
    pub slots: usize,
    pub depth: usize,
    /// The single public input.
    pub commitment: Fr,
    // Private.
    pub root: Fr,
    pub valid_until: u32,
    pub count: u32,
    pub subjects: Vec<Fr>,
    pub active: Vec<bool>,
    pub expiries: Vec<Fr>,
    pub paths: Vec<Vec<Fr>>,
    pub index_bits: Vec<Vec<bool>>,
}

impl AggregationCircuit {
    /// All-zero instance with the right shape, for key generation.
    pub fn blank(config: PoseidonConfig<Fr>, slots: usize, depth: usize) -> Self {
        AggregationCircuit {
            config,
            slots,
            depth,
            commitment: Fr::zero(),
            root: Fr::zero(),
            valid_until: 0,
            count: 0,
            subjects: vec![Fr::zero(); slots],
            active: vec![false; slots],
            expiries: vec![Fr::zero(); slots],
            paths: vec![vec![Fr::zero(); depth]; slots],
            index_bits: vec![vec![false; depth]; slots],
        }
    }

    /// Witness an accumulator built by [`Accumulator::build`].
    pub fn from_accumulator(config: PoseidonConfig<Fr>, depth: usize, acc: &Accumulator) -> Self {
        let mut c = Self::blank(config, acc.slots, depth);
        c.commitment = acc.commitment();
        c.root = acc.root;
        c.valid_until = acc.valid_until;
        c.count = acc.credentials.len() as u32;
        for (i, cred) in acc.credentials.iter().enumerate() {
            c.subjects[i] = acc.subject_fields[i];
            c.active[i] = true;
            c.expiries[i] = Fr::from(cred.expiry as u64);
            c.paths[i] = cred
                .path
                .iter()
                .map(|h| fr_from_hex(h).expect("validated by Accumulator::build"))
                .collect();
            c.index_bits[i] = (0..depth).map(|h| (cred.index >> h) & 1 == 1).collect();
        }
        c
    }
}

fn hash2(
    cs: &ConstraintSystemRef<Fr>,
    config: &PoseidonConfig<Fr>,
    a: &FpVar<Fr>,
    b: &FpVar<Fr>,
) -> Result<FpVar<Fr>, SynthesisError> {
    let mut sponge = PoseidonSpongeVar::new(cs.clone(), config);
    sponge.absorb(&vec![a.clone(), b.clone()])?;
    Ok(sponge.squeeze_field_elements(1)?.remove(0))
}

/// Canonical 32-byte big-endian encoding of a field element.
fn be_bytes32(f: &FpVar<Fr>) -> Result<Vec<UInt8<Fr>>, SynthesisError> {
    // `to_bytes` is little-endian and strict (enforces the canonical value).
    let mut le = f.to_bytes()?;
    le.resize(32, UInt8::constant(0));
    le.reverse();
    Ok(le)
}

fn u32_be(v: &UInt32<Fr>) -> Result<Vec<UInt8<Fr>>, SynthesisError> {
    let mut le = v.to_bytes()?;
    le.reverse();
    Ok(le)
}

fn u32_to_fp(v: &UInt32<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    Boolean::le_bits_to_fp_var(&v.to_bits_le())
}

impl ConstraintSynthesizer<Fr> for AggregationCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let commitment = FpVar::new_input(cs.clone(), || Ok(self.commitment))?;

        let root = FpVar::new_witness(cs.clone(), || Ok(self.root))?;
        let valid_until_u32 = UInt32::new_witness(cs.clone(), || Ok(self.valid_until))?;
        let count_u32 = UInt32::new_witness(cs.clone(), || Ok(self.count))?;
        let valid_until = u32_to_fp(&valid_until_u32)?;
        let count = u32_to_fp(&count_u32)?;
        let subjects = (0..self.slots)
            .map(|i| FpVar::new_witness(cs.clone(), || Ok(self.subjects[i])))
            .collect::<Result<Vec<_>, _>>()?;

        // Bind the private statement to the public commitment.
        let mut preimage = be_bytes32(&root)?;
        preimage.extend(u32_be(&valid_until_u32)?);
        preimage.extend(u32_be(&count_u32)?);
        for s in &subjects {
            preimage.extend(be_bytes32(s)?);
        }
        let digest = Sha256Gadget::digest(&preimage)?.0;
        // Bytes 1..32 big-endian (byte 0 cleared) -> little-endian bits.
        let bits: Vec<Boolean<Fr>> = digest[1..]
            .iter()
            .rev()
            .map(|b| b.to_bits_le())
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        Boolean::le_bits_to_fp_var(&bits)?.enforce_equal(&commitment)?;

        let mut live = FpVar::<Fr>::zero();
        let mut prev_active = Boolean::TRUE;
        for (i, subject) in subjects.iter().enumerate() {
            let active = Boolean::new_witness(cs.clone(), || Ok(self.active[i]))?;
            // Prefix: active_i => active_{i-1}.
            active
                .and(&prev_active.not())?
                .enforce_equal(&Boolean::FALSE)?;
            prev_active = active.clone();
            live += FpVar::from(active.clone());

            // Padding slots carry subject 0.
            (subject * FpVar::from(active.not())).enforce_equal(&FpVar::zero())?;

            let expiry = FpVar::new_witness(cs.clone(), || Ok(self.expiries[i]))?;
            let mut node = hash2(&cs, &self.config, subject, &expiry)?;
            for h in 0..self.depth {
                let is_right = Boolean::new_witness(cs.clone(), || Ok(self.index_bits[i][h]))?;
                let sibling = FpVar::new_witness(cs.clone(), || Ok(self.paths[i][h]))?;
                let left = FpVar::conditionally_select(&is_right, &sibling, &node)?;
                let right = FpVar::conditionally_select(&is_right, &node, &sibling)?;
                node = hash2(&cs, &self.config, &left, &right)?;
            }
            // active => path reaches root.
            ((node - &root) * FpVar::from(active.clone())).enforce_equal(&FpVar::zero())?;
            // active => expiry > valid_until.
            let unexpired = expiry.is_cmp(&valid_until, Ordering::Greater, false)?;
            active
                .and(&unexpired.not())?
                .enforce_equal(&Boolean::FALSE)?;
        }
        live.enforce_equal(&count)?;
        Ok(())
    }
}
