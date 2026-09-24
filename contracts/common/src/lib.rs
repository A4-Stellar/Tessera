//! Shared helpers for the Tessera RWA contract suite: emergency-pause state
//! and the WASM code-upgrade pattern (issue #11).
//!
//! This crate deliberately does *not* define a shared `Error` enum. Each
//! contract (registry, compliance, dividend, asset-token) keeps its own
//! error type and numbering — a new `Paused` variant is appended after that
//! specific contract's highest existing error code, rather than renumbering
//! anything or forcing all four contracts onto one shared enum. This crate
//! only factors out the mechanical, identical-everywhere bits: the storage
//! key for the pause flag, and the host call for a code upgrade.
#![no_std]

use soroban_sdk::{symbol_short, BytesN, Env, Symbol};

const PAUSED_KEY: Symbol = symbol_short!("paused");

/// Whether the calling contract is currently paused. Defaults to `false`
/// (not paused) when the flag has never been written.
pub fn is_paused(env: &Env) -> bool {
    env.storage().instance().get(&PAUSED_KEY).unwrap_or(false)
}

/// Set the pause flag for the calling contract.
///
/// This performs no authorization of its own — callers must run their own
/// admin check (`require_auth` + admin-identity comparison, in that
/// contract's own error style) before calling this.
pub fn set_paused(env: &Env, paused: bool) {
    env.storage().instance().set(&PAUSED_KEY, &paused);
}

/// Apply a new contract WASM (issue #11's upgrade path):
/// `env.deployer().update_current_contract_wasm(new_wasm_hash)`.
///
/// As with [`set_paused`], authorization is the caller's responsibility.
/// Centralizing the host call here just means all four contracts invoke the
/// upgrade path identically.
pub fn upgrade(env: &Env, new_wasm_hash: BytesN<32>) {
    env.deployer().update_current_contract_wasm(new_wasm_hash);
}
