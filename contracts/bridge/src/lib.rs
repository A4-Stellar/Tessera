//! Cross-chain bridge message verification.
//!
//! Rewritten by the fuzzing work. The signature path had a critical
//! authentication bypass and a reachable panic, both of which the fuzz target
//! for this contract now asserts against.
//!
//! Issue #475 adds monitoring hooks and a circuit breaker:
//!
//! * Every outbound `lock_and_mint_request` publishes a `SendPacket` event
//!   with a global packet sequence, and every accepted inbound
//!   `burn_and_unlock_request` publishes `RecvPacket`. The API's
//!   `bridge_monitor` pairs them across chains and raises `BridgeTimeout`.
//! * Outbound volume and packet count are capped per window of ledgers, so a
//!   drain is bounded even before anyone reacts.
//! * The anomaly reporter (the API's bridge anomaly detector) submits a score.
//!   At or above `anomaly_threshold_bps` the bridge pauses new `SendPacket`
//!   calls. Only a quorum of Guardians can unpause: neither the admin nor the
//!   reporter can.
//! * The contract is upgradeable by the admin (`upgrade`).
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Bytes, BytesN, Env, Symbol, Vec,
};

/// Signatures required to accept an inbound message. Two-of-N.
pub const MIN_SIGNATURES: u32 = 2;
/// Upper bound on the Guardian set.
pub const MAX_GUARDIANS: u32 = 20;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Validator(BytesN<32>),
    Nonce(Address),
    ProcessedMessage(BytesN<32>),
    Request(Address, u64),
    // ---- issue #475 ----
    /// Global outbound packet sequence (next value to assign).
    Sequence,
    Paused,
    Circuit,
    /// `(window_start_ledger, volume, packets)` of the current window.
    Window,
    Guardians,
    GuardianThreshold,
    AnomalyReporter,
}

/// Declared errors.
///
/// This contract previously had no `#[contracterror]` enum: every rejection
/// was a bare `panic!("...")`, which the host reports identically to an
/// arithmetic overflow or a failed `.unwrap()`.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    /// `signatures` and `public_keys` had different lengths. This used to be an
    /// `unwrap()` on `public_keys.get(i)`, i.e. a trap, and the fuzzer reaches
    /// it with a single mismatched-length call.
    LengthMismatch = 4,
    /// Fewer than [`MIN_SIGNATURES`] *valid, registered, distinct* validator
    /// signatures were supplied.
    InsufficientSignatures = 5,
    MessageExpired = 6,
    AlreadyProcessed = 7,
    ValidatorAlreadyRegistered = 8,
    ValidatorNotRegistered = 9,
    /// A caller's nonce would exceed the `u64` ceiling.
    Overflow = 10,
    /// No recorded request for `(caller, nonce)`.
    RequestNotFound = 11,
    // ---- issue #475 ----
    InvalidAmount = 12,
    /// The circuit breaker is open: new `SendPacket` calls are refused.
    Paused = 13,
    /// The send would exceed the per-window volume or packet cap.
    WindowCapExceeded = 14,
    InvalidCircuitConfig = 15,
    InvalidGuardians = 16,
    /// Fewer than the threshold of distinct Guardians signed the unpause.
    InsufficientGuardians = 17,
    NotGuardian = 18,
    NotPaused = 19,
}

/// One recorded lock-and-mint request.
#[contracttype]
#[derive(Clone)]
pub struct BridgeRequest {
    pub caller: Address,
    pub nonce: u64,
    pub amount: i128,
    pub destination_chain: Bytes,
    pub destination_address: Bytes,
    pub created_at: u64,
    /// Issue #475: global packet sequence carried by the `SendPacket` event.
    pub sequence: u64,
}

/// Circuit breaker limits (issue #475).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CircuitConfig {
    /// Length of the rolling volume window, in ledgers.
    pub window_ledgers: u32,
    /// Maximum outbound amount per window. `0` disables the volume cap.
    pub max_window_volume: i128,
    /// Maximum outbound packets per window. `0` disables the packet cap.
    pub max_window_packets: u32,
    /// Anomaly score (basis points, 0–10 000) that opens the breaker.
    pub anomaly_threshold_bps: u32,
}

/// Breaker state, for monitoring.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CircuitState {
    pub paused: bool,
    pub window_start: u32,
    pub window_volume: i128,
    pub window_packets: u32,
}

#[contract]
pub struct BridgeProtocol;

#[contractimpl]
impl BridgeProtocol {
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    /// Register a validator public key that may sign inbound messages.
    pub fn register_validator(env: Env, admin: Address, key: BytesN<32>) {
        Self::require_admin(&env, &admin);
        if env
            .storage()
            .instance()
            .has(&DataKey::Validator(key.clone()))
        {
            panic_with_error!(env, Error::ValidatorAlreadyRegistered);
        }
        env.storage()
            .instance()
            .set(&DataKey::Validator(key), &true);
        env.events().publish((symbol_short!("valreg"),), admin);
    }

    pub fn revoke_validator(env: Env, admin: Address, key: BytesN<32>) {
        Self::require_admin(&env, &admin);
        if !env
            .storage()
            .instance()
            .has(&DataKey::Validator(key.clone()))
        {
            panic_with_error!(env, Error::ValidatorNotRegistered);
        }
        env.storage().instance().remove(&DataKey::Validator(key));
    }

    pub fn is_validator(env: Env, key: BytesN<32>) -> bool {
        env.storage().instance().has(&DataKey::Validator(key))
    }

    pub fn is_processed(env: Env, message_hash: BytesN<32>) -> bool {
        env.storage()
            .instance()
            .has(&DataKey::ProcessedMessage(message_hash))
    }

    pub fn nonce_of(env: Env, caller: Address) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::Nonce(caller))
            .unwrap_or(0)
    }

    pub fn get_request(env: Env, caller: Address, nonce: u64) -> BridgeRequest {
        env.storage()
            .persistent()
            .get(&DataKey::Request(caller, nonce))
            .unwrap_or_else(|| panic_with_error!(env, Error::RequestNotFound))
    }

    /// Record a lock-and-mint request, advance the caller's nonce and publish
    /// a `SendPacket` event. Returns the nonce the request is filed under.
    ///
    /// The token lock/mint itself remains a stub, as it was; the request is
    /// persisted so the nonce is meaningful and the request is auditable.
    pub fn lock_and_mint_request(
        env: Env,
        caller: Address,
        amount: i128,
        destination_chain: Bytes,
        destination_address: Bytes,
    ) -> u64 {
        caller.require_auth();
        Self::require_initialized(&env);
        if Self::paused(&env) {
            panic_with_error!(env, Error::Paused);
        }
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        Self::record_outbound(&env, amount);

        let nonce = Self::nonce_of(env.clone(), caller.clone());
        let next = nonce
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
        env.storage()
            .instance()
            .set(&DataKey::Nonce(caller.clone()), &next);

        let sequence: u64 = env
            .storage()
            .instance()
            .get(&DataKey::Sequence)
            .unwrap_or(0);
        let next_sequence = sequence
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
        env.storage()
            .instance()
            .set(&DataKey::Sequence, &next_sequence);

        let request = BridgeRequest {
            caller: caller.clone(),
            nonce,
            amount,
            destination_chain: destination_chain.clone(),
            destination_address: destination_address.clone(),
            created_at: env.ledger().timestamp(),
            sequence,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Request(caller.clone(), nonce), &request);

        env.events().publish(
            (Symbol::new(&env, "SendPacket"), caller),
            (
                sequence,
                nonce,
                amount,
                destination_chain,
                destination_address,
                env.ledger().sequence(),
            ),
        );
        nonce
    }

    /// Accept an inbound burn-and-unlock message, once it carries
    /// [`MIN_SIGNATURES`] valid signatures from distinct registered validators.
    ///
    /// Two defects were fixed here:
    ///
    /// * **Signature verification was discarded.** The loop called
    ///   `ed25519_verify` and then incremented `valid_signatures`
    ///   unconditionally, so *any* two 64-byte blobs — including two copies of
    ///   all zeroes — were accepted as a valid two-of-N multisig. A signature
    ///   now counts only if it verifies, its key is a registered validator,
    ///   and that validator has not already been counted.
    /// * **`public_keys.get(i).unwrap()` trapped** whenever `signatures` was
    ///   longer than `public_keys`, which the fuzzer reaches with a single
    ///   mismatched-length call. Lengths are now compared up front and reported
    ///   as [`Error::LengthMismatch`].
    ///
    /// Issue #475: an accepted message publishes `RecvPacket`.
    pub fn burn_and_unlock_request(
        env: Env,
        caller: Address,
        amount: i128,
        message_hash: BytesN<32>,
        signatures: Vec<BytesN<64>>,
        public_keys: Vec<BytesN<32>>,
        ttl: u64,
    ) {
        caller.require_auth();
        Self::require_initialized(&env);

        if env.ledger().timestamp() > ttl {
            panic_with_error!(env, Error::MessageExpired);
        }
        if Self::is_processed(env.clone(), message_hash.clone()) {
            panic_with_error!(env, Error::AlreadyProcessed);
        }
        if signatures.len() != public_keys.len() {
            panic_with_error!(env, Error::LengthMismatch);
        }

        // Count only signatures from registered validators, and only once per
        // validator so a single key cannot meet the threshold alone.
        let mut counted: Vec<BytesN<32>> = Vec::new(&env);
        for i in 0..signatures.len() {
            let (Some(pk), Some(sig)) = (public_keys.get(i), signatures.get(i)) else {
                panic_with_error!(env, Error::LengthMismatch);
            };
            if counted.contains(&pk)
                || !Self::is_validator(env.clone(), pk.clone())
                || !verify(&pk, &message_hash, &sig)
            {
                continue;
            }
            counted.push_back(pk);
        }

        if counted.len() < MIN_SIGNATURES {
            panic_with_error!(env, Error::InsufficientSignatures);
        }

        env.storage()
            .instance()
            .set(&DataKey::ProcessedMessage(message_hash.clone()), &true);
        env.events().publish(
            (Symbol::new(&env, "RecvPacket"), caller),
            (message_hash, amount, env.ledger().sequence()),
        );
    }

    // ---- issue #475: circuit breaker ----

    pub fn configure_circuit_breaker(env: Env, admin: Address, config: CircuitConfig) {
        Self::require_admin(&env, &admin);
        if config.window_ledgers == 0
            || config.max_window_volume < 0
            || config.anomaly_threshold_bps == 0
            || config.anomaly_threshold_bps > 10_000
        {
            panic_with_error!(env, Error::InvalidCircuitConfig);
        }
        env.storage().instance().set(&DataKey::Circuit, &config);
    }

    pub fn get_circuit_config(env: Env) -> Option<CircuitConfig> {
        env.storage().instance().get(&DataKey::Circuit)
    }

    /// The Guardians and how many of them must sign an unpause.
    pub fn set_guardians(env: Env, admin: Address, guardians: Vec<Address>, threshold: u32) {
        Self::require_admin(&env, &admin);
        let n = guardians.len();
        let mut seen: Vec<Address> = Vec::new(&env);
        for g in guardians.iter() {
            if seen.contains(&g) {
                panic_with_error!(env, Error::InvalidGuardians);
            }
            seen.push_back(g);
        }
        if n == 0 || n > MAX_GUARDIANS || threshold == 0 || threshold > n {
            panic_with_error!(env, Error::InvalidGuardians);
        }
        env.storage()
            .instance()
            .set(&DataKey::Guardians, &guardians);
        env.storage()
            .instance()
            .set(&DataKey::GuardianThreshold, &threshold);
    }

    pub fn get_guardians(env: Env) -> (Vec<Address>, u32) {
        (
            env.storage()
                .instance()
                .get(&DataKey::Guardians)
                .unwrap_or_else(|| Vec::new(&env)),
            env.storage()
                .instance()
                .get(&DataKey::GuardianThreshold)
                .unwrap_or(0),
        )
    }

    /// The address allowed to report anomaly scores (the API's monitor).
    pub fn set_anomaly_reporter(env: Env, admin: Address, reporter: Address) {
        Self::require_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::AnomalyReporter, &reporter);
    }

    /// Report the current anomaly score (basis points). At or above the
    /// configured threshold the breaker opens. Returns whether the bridge is
    /// paused afterwards.
    pub fn report_anomaly(env: Env, reporter: Address, score_bps: u32) -> bool {
        reporter.require_auth();
        let expected: Address = env
            .storage()
            .instance()
            .get(&DataKey::AnomalyReporter)
            .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized));
        if reporter != expected {
            panic_with_error!(env, Error::Unauthorized);
        }
        let threshold = Self::get_circuit_config(env.clone())
            .map(|c| c.anomaly_threshold_bps)
            .unwrap_or_else(|| panic_with_error!(env, Error::InvalidCircuitConfig));
        env.events()
            .publish((symbol_short!("anomaly"),), (score_bps, threshold));
        if score_bps >= threshold && !Self::paused(&env) {
            Self::set_paused(&env, symbol_short!("anomaly"), score_bps);
        }
        Self::paused(&env)
    }

    /// Manual emergency stop by the admin. Unpausing still needs Guardians.
    pub fn pause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        if !Self::paused(&env) {
            Self::set_paused(&env, symbol_short!("admin"), 0);
        }
    }

    /// Close the breaker. Every address in `signers` must authorize the call,
    /// be a Guardian, and appear once; at least the Guardian threshold must
    /// sign. Resets the current volume window.
    pub fn unpause(env: Env, signers: Vec<Address>) {
        if !Self::paused(&env) {
            panic_with_error!(env, Error::NotPaused);
        }
        let (guardians, threshold) = Self::get_guardians(env.clone());
        if threshold == 0 {
            panic_with_error!(env, Error::InsufficientGuardians);
        }
        let mut seen: Vec<Address> = Vec::new(&env);
        for signer in signers.iter() {
            if !guardians.contains(&signer) {
                panic_with_error!(env, Error::NotGuardian);
            }
            if seen.contains(&signer) {
                continue;
            }
            signer.require_auth();
            seen.push_back(signer);
        }
        if seen.len() < threshold {
            panic_with_error!(env, Error::InsufficientGuardians);
        }
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().instance().remove(&DataKey::Window);
        env.events()
            .publish((Symbol::new(&env, "BridgeUnpaused"),), seen);
    }

    pub fn is_paused(env: Env) -> bool {
        Self::paused(&env)
    }

    pub fn circuit_state(env: Env) -> CircuitState {
        let (window_start, window_volume, window_packets) = Self::window(&env);
        CircuitState {
            paused: Self::paused(&env),
            window_start,
            window_volume,
            window_packets,
        }
    }

    /// Replace the contract code, keeping storage.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::require_admin(&env, &admin);
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }

    // ---- internal ----

    fn require_admin(env: &Env, admin: &Address) {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        admin.require_auth();
        if admin != &stored {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    fn require_initialized(env: &Env) {
        if !env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::NotInitialized);
        }
    }

    fn paused(env: &Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    fn set_paused(env: &Env, reason: Symbol, score_bps: u32) {
        env.storage().instance().set(&DataKey::Paused, &true);
        env.events().publish(
            (Symbol::new(env, "BridgePaused"), reason),
            (score_bps, env.ledger().sequence()),
        );
    }

    fn window(env: &Env) -> (u32, i128, u32) {
        env.storage()
            .instance()
            .get(&DataKey::Window)
            .unwrap_or((0, 0, 0))
    }

    /// Adds an outbound send to the current window, refusing it when a cap
    /// would be exceeded. A refused send changes nothing (the call reverts).
    fn record_outbound(env: &Env, amount: i128) {
        let Some(config) = Self::get_circuit_config(env.clone()) else {
            return;
        };
        let now = env.ledger().sequence();
        let (mut start, mut volume, mut packets) = Self::window(env);
        if now >= start.saturating_add(config.window_ledgers) {
            start = now;
            volume = 0;
            packets = 0;
        }
        let volume = volume.saturating_add(amount);
        let packets = packets.saturating_add(1);
        if (config.max_window_volume > 0 && volume > config.max_window_volume)
            || (config.max_window_packets > 0 && packets > config.max_window_packets)
        {
            panic_with_error!(env, Error::WindowCapExceeded);
        }
        env.storage()
            .instance()
            .set(&DataKey::Window, &(start, volume, packets));
    }
}

/// Strict Ed25519 verification that reports failure instead of trapping.
fn verify(key: &BytesN<32>, message: &BytesN<32>, signature: &BytesN<64>) -> bool {
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&key.to_array()) else {
        return false;
    };
    let signature = ed25519_dalek::Signature::from_bytes(&signature.to_array());
    key.verify_strict(&message.to_array(), &signature).is_ok()
}

#[cfg(test)]
mod test;
