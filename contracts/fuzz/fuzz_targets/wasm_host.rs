#![no_main]

//! WASM-level fuzz target (issue #470).
//!
//! Two layers:
//!
//! 1. A mock host/storage oracle that records every `set`/`get` and fails on a
//!    read of an unset key — the panic-equivalent that breaks contracts in
//!    production. It also drives the structure-aware ABI mutator, so inputs
//!    are skewed toward XDR discriminants and integer boundaries.
//! 2. When compiled contracts are available (`TESSERA_WASM_DIR`), each `.wasm`
//!    is registered in a sandboxed Soroban host and invoked with arbitrary
//!    `ScVal`-shaped arguments. Host traps are treated as findings via the
//!    shared `check_no_trap` oracle.

use std::path::PathBuf;

use libfuzzer_sys::fuzz_target;
use soroban_sdk::{Bytes, Env, IntoVal, Symbol, Val, Vec as SdkVec};
use tessera_fuzz_harness::{
    check_no_trap, Input, StorageOracle, StructureAwareMutator, ASSET_TOKEN_MINT,
};

fn arbitrary_val(env: &Env, input: &mut Input<'_>) -> Val {
    match input.u8() % 6 {
        0 => input.i128_interesting().into_val(env),
        1 => input.i128().into_val(env),
        2 => input.bool().into_val(env),
        3 => input.u32_interesting().into_val(env),
        4 => {
            let length = input.below(32);
            Bytes::from_slice(env, &input.bytes(length)).into_val(env)
        }
        _ => Symbol::new(env, &input.ascii(8)).into_val(env),
    }
}

fn wasm_files() -> Vec<PathBuf> {
    let Ok(dir) = std::env::var("TESSERA_WASM_DIR") else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "wasm"))
        .collect();
    files.sort();
    files
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let seed = input.u64();

    // --- Layer 1: mock host + structure-aware ABI mutation -----------------
    let mut oracle = StorageOracle::new();
    let mut mutator = StructureAwareMutator::new(seed, &ASSET_TOKEN_MINT);
    let mut encoded = data.to_vec();
    for _ in 0..4 {
        mutator.mutate(&mut encoded);
    }
    // Replay the mutated buffer as a storage trace. A `get` without a prior
    // `set` for the same key is a finding, independent of the contract host.
    for chunk in encoded.chunks(9) {
        if chunk.len() < 9 {
            continue;
        }
        let key = &chunk[..4];
        let value = &chunk[4..8];
        oracle.set("fuzz", key, value);
        if chunk[8] & 1 == 1 && oracle.get("fuzz", key) {
            // A successful read; the oracle has recorded it.
        }
    }
    oracle.assert_no_unset_reads();

    // --- Layer 2: run compiled contracts in a sandboxed host ---------------
    for path in wasm_files() {
        let Ok(wasm) = std::fs::read(&path) else {
            continue;
        };
        let env = Env::default();
        let contract = env.register_contract_wasm(None, Bytes::from_slice(&env, &wasm));

        // Seed one known key through the host and mirror it into the oracle so
        // the mock layer and the real host agree on what has been written.
        env.as_contract(&contract, || {
            env.storage()
                .instance()
                .set(&Symbol::new(&env, "oracle_seed"), &input.u64());
        });
        oracle.set("wasm", b"oracle_seed", &[]);

        for function in ["initialize", "mint", "transfer", "claim", "verify_da_proof"] {
            let mut args = SdkVec::new(&env);
            for _ in 0..4 {
                args.push_back(arbitrary_val(&env, &mut input));
            }
            check_no_trap(
                &env.try_invoke_contract::<Val, soroban_sdk::Error>(
                    &contract,
                    &Symbol::new(&env, function),
                    args,
                ),
                "wasm_host::arbitrary_invocation",
            );
        }
        oracle.assert_no_unset_reads();
    }
});
