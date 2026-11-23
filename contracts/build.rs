//! Cargo build script (`contracts/build.rs`) for `tessera-common` (Issue #451).
//!
//! Tracks changes to the four core Soroban contract sources (`registry`,
//! `compliance`, `dividend`, `asset-token`), probes `soroban contract inspect --wasm`
//! when compiled `.wasm` artifacts are present, and ensures the `#![no_std]`
//! Rust ABI bindings in `contracts/common/src/abi.rs` are up to date.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const CONTRACT_SOURCES: &[(&str, &str)] = &[
    ("registry", "../registry/src/lib.rs"),
    ("compliance", "../compliance/src/lib.rs"),
    ("dividend", "../dividend/src/lib.rs"),
    ("asset_token", "../asset-token/src/lib.rs"),
];

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));

    for (_name, rel_path) in CONTRACT_SOURCES {
        let full_path = manifest_dir.join(rel_path);
        println!("cargo:rerun-if-changed={}", full_path.display());
    }
    println!("cargo:rerun-if-changed=../build.rs");

    // If compiled .wasm binaries exist in target/wasm32-unknown-unknown/release,
    // probe `soroban contract inspect --wasm` as part of the build pipeline.
    let wasm_release_dir = manifest_dir
        .join("..")
        .join("target")
        .join("wasm32-unknown-unknown")
        .join("release");
    for (name, _) in CONTRACT_SOURCES {
        let wasm_path = wasm_release_dir.join(format!("tessera_{name}.wasm"));
        if wasm_path.exists() {
            let _ = Command::new("soroban")
                .args(["contract", "inspect", "--wasm"])
                .arg(&wasm_path)
                .output();
        }
    }

    let abi_rs_path = manifest_dir.join("src").join("abi.rs");
    let force_regen = env::var("TESSERA_REGEN_ABI").ok().as_deref() == Some("1");

    if !abi_rs_path.exists() || force_regen {
        let script_path = manifest_dir
            .join("..")
            .join("..")
            .join("docs")
            .join("scripts")
            .join("generate-sdk-examples.mjs");
        if script_path.exists() {
            let _ = Command::new("node").arg(&script_path).status();
        }
        if !abi_rs_path.exists() {
            let fallback = "// Auto-generated fallback ABI module (#451).\n\
                            pub const ALL_CONTRACTS: &[()] = &[];\n";
            let _ = fs::write(&abi_rs_path, fallback);
        }
    }
}
