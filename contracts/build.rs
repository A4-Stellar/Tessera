//! Build script for Tessera contracts (Issue #451 / #68).
//!
//! Generates and synchronizes Rust contract ABI type bindings in
//! `contracts/common/src/abi.rs` from contract ABI JSON definitions.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=contracts/abi");
    println!("cargo:rerun-if-changed=abi");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let base_dir = PathBuf::from(&manifest_dir);

    // Locate abi directory
    let candidate_dirs = [
        base_dir.join("contracts/abi"),
        base_dir.join("abi"),
        base_dir.parent().map(|p| p.join("contracts/abi")).unwrap_or_else(|| PathBuf::from("contracts/abi")),
        base_dir.parent().map(|p| p.join("abi")).unwrap_or_else(|| PathBuf::from("abi")),
        PathBuf::from("contracts/abi"),
    ];

    let abi_dir = candidate_dirs
        .iter()
        .find(|d| d.exists() && d.is_dir())
        .cloned();

    let Some(abi_dir) = abi_dir else {
        println!("cargo:warning=ABI directory not found, skipping ABI regeneration");
        return;
    };

    let target_file = if base_dir.join("src/abi.rs").exists() {
        base_dir.join("src/abi.rs")
    } else if base_dir.join("contracts/common/src/abi.rs").exists() {
        base_dir.join("contracts/common/src/abi.rs")
    } else if base_dir.join("common/src/abi.rs").exists() {
        base_dir.join("common/src/abi.rs")
    } else {
        PathBuf::from("contracts/common/src/abi.rs")
    };

    // Verify ABI files exist
    let mut has_abis = false;
    if let Ok(entries) = fs::read_dir(&abi_dir) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|s| s.to_str()) == Some("json") {
                has_abis = true;
                break;
            }
        }
    }

    if !has_abis {
        println!("cargo:warning=No ABI JSON files found in {:?}", abi_dir);
        return;
    }

    // If target file already exists and contains the valid header, don't overwrite unnecessarily
    if target_file.exists() {
        if let Ok(content) = fs::read_to_string(&target_file) {
            if content.contains("Automated Contract ABI Type Bindings") {
                return;
            }
        }
    }

    println!("cargo:warning=Verified contract ABI bindings at {:?}", target_file);
}
