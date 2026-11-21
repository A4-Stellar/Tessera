//! Criterion benchmark for the native BLS12-381 confidential-token verifier.
//!
//! The proof is well-formed but intentionally invalid, so each iteration
//! exercises transcript reconstruction, generator derivation and the final
//! verification MSM without requiring a secret witness.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    BadRequest(String),
    Unavailable(String),
}

pub mod routes {
    pub use crate::ApiError;
}

pub mod indexer {
    use std::sync::RwLock;

    pub mod mmr_checkpoint {
        pub fn hex32(bytes: &[u8; 32]) -> String {
            hex::encode(bytes)
        }

        pub fn mmr_leaf_hash(
            _sequence: u32,
            _ledger_hash: &[u8; 32],
            _state_root: &[u8; 32],
        ) -> [u8; 32] {
            [0; 32]
        }

        pub struct MmrRootResponse {
            pub leaf_count: u64,
            pub root: String,
            pub latest_ledger_sequence: Option<u32>,
        }

        pub struct StateEntryProof {
            pub asset_id: u64,
            pub address: String,
            pub balance: String,
            pub leaf_index: u64,
            pub path: Vec<String>,
        }

        pub struct AccountProof {
            pub address: String,
            pub state_leaf_count: u64,
            pub entries: Vec<StateEntryProof>,
        }

        pub struct MmrProofResponse {
            pub ledger_sequence: u32,
            pub ledger_hash: String,
            pub contract_state_root: String,
            pub leaf_hash: String,
            pub leaf_index: u64,
            pub leaf_count: u64,
            pub path: Vec<String>,
            pub peaks: Vec<String>,
            pub root: String,
            pub account: Option<AccountProof>,
        }

        pub struct Checkpoint {
            pub ledger_sequence: u32,
            pub ledger_hash: [u8; 32],
            pub state_root: [u8; 32],
        }

        pub struct InclusionProof {
            pub leaf_index: u64,
            pub leaf_count: u64,
            pub path: Vec<[u8; 32]>,
            pub peaks: Vec<[u8; 32]>,
        }

        pub struct StateEntry {
            pub asset_id: u64,
            pub address: String,
            pub balance: i128,
        }

        pub struct StateTree;

        impl StateTree {
            pub fn proofs_for(&self, _address: &str) -> Vec<(StateEntry, u64, Vec<[u8; 32]>)> {
                Vec::new()
            }

            pub fn len(&self) -> usize {
                0
            }
        }

        pub struct Mmr;

        impl Mmr {
            pub fn leaf_count(&self) -> u64 {
                0
            }

            pub fn root(&self) -> [u8; 32] {
                [0; 32]
            }

            pub fn latest(&self) -> Option<Checkpoint> {
                None
            }

            pub fn proof(&self, _sequence: u32) -> Option<(Checkpoint, InclusionProof)> {
                None
            }

            pub fn state(&self, _sequence: u32) -> Option<StateTree> {
                None
            }
        }
    }

    pub struct AppState {
        pub mmr: RwLock<mmr_checkpoint::Mmr>,
    }
}

#[path = "../src/routes/proofs.rs"]
mod proofs;

fn invalid_proof_bytes() -> Vec<u8> {
    let mut bytes = vec![0; 1696];
    for offset in [0, 96, 192, 288]
        .into_iter()
        .chain((0..6).map(|round| 480 + round * 96))
        .chain((0..6).map(|round| 1056 + round * 96))
    {
        bytes[offset] = 0x40;
    }
    bytes
}

fn bench_native_range_verifier(c: &mut Criterion) {
    let proof = invalid_proof_bytes();
    let inputs = proofs::RangePublicInputs {
        commitment: format!("40{}", "00".repeat(95)),
        context: String::new(),
    };

    c.bench_function("confidential_token_bls12_381_range_verify", |b| {
        b.iter(|| {
            black_box(
                proofs::verify_native_range(black_box(&inputs), black_box(&proof))
                    .expect("well-formed proof should complete verification"),
            )
        })
    });
}

criterion_group!(benches, bench_native_range_verifier);
criterion_main!(benches);
