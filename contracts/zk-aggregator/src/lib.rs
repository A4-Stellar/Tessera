//! Off-chain KYC proof aggregation for the Tessera compliance contract
//! (issue #317).
//!
//! Pipeline:
//!
//! 1. **Issue.** An authorized KYC issuer puts one leaf per accredited
//!    investor, `Poseidon(subject, expiry)`, into a fixed-depth Poseidon
//!    Merkle tree ([`CredentialTree`]) and publishes the root on-chain
//!    (`set_zk_issuer_root`). Each investor's individual credential
//!    ([`Credential`]: subject, expiry, Merkle path) is dropped into a queue
//!    directory.
//! 2. **Aggregate.** The aggregator drains up to `slots` credentials from the
//!    queue, checks each one individually, and folds the batch into one
//!    accumulator ([`Accumulator`]): `valid_until = min(expiry) - 1` and the
//!    commitment `C = SHA-256(root ‖ valid_until ‖ count ‖ s_0 … s_{slots-1})`
//!    (top byte cleared).
//! 3. **Prove.** One Groth16 proof over BN254 of [`AggregationCircuit`], whose
//!    only public input is `C`, attests every credential in the batch. The
//!    contract recomputes `C` from its own arguments and checks the proof with
//!    a single 4-pair BN254 multi-pairing (`submit_aggregated_kyc`).
//!
//! BN254 (rather than BLS12-381) and a single public input keep on-chain
//! verification at roughly a third of the transaction CPU limit; see the
//! README for the measured costs. Soroban encodings (see [`encode`]) follow
//! the host's BN254 functions: Ethereum-style big-endian points and scalars.

pub mod circuit;
pub mod credentials;
pub mod encode;

use ark_bn254::Fr;
use ark_crypto_primitives::sponge::{
    poseidon::{find_poseidon_ark_and_mds, PoseidonConfig, PoseidonSponge},
    CryptographicSponge,
};

pub use circuit::AggregationCircuit;
pub use credentials::{commitment, Accumulator, Credential, CredentialTree};

/// Default circuit shape: matches `MAX_SUBJECTS` on-chain.
pub const DEFAULT_SLOTS: usize = 32;
/// Default credential-tree depth: about a million investors per issuer root.
pub const DEFAULT_DEPTH: usize = 20;

/// Poseidon over the BN254 scalar field, width 3 (rate 2, capacity 1),
/// x^5 S-box, 8 full + 57 partial rounds (the circom / iden3 parameter
/// sizes for t = 3). Round constants and MDS come from the reference Grain
/// LFSR, so they are reproducible from these four numbers.
pub fn poseidon_config() -> PoseidonConfig<Fr> {
    const FULL: u64 = 8;
    const PARTIAL: u64 = 57;
    const ALPHA: u64 = 5;
    let (ark, mds) = find_poseidon_ark_and_mds::<Fr>(254, 2, FULL, PARTIAL, 0);
    PoseidonConfig::new(FULL as usize, PARTIAL as usize, ALPHA, mds, ark, 2, 1)
}

/// Two-to-one Poseidon hash, identical to the in-circuit gadget.
pub fn poseidon2(config: &PoseidonConfig<Fr>, a: Fr, b: Fr) -> Fr {
    let mut sponge = PoseidonSponge::new(config);
    sponge.absorb(&vec![a, b]);
    sponge.squeeze_field_elements::<Fr>(1)[0]
}

#[cfg(test)]
mod tests;
