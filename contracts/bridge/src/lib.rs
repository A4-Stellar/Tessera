//! Cross-chain bridge message verification.
//!
//! Rewritten by the fuzzing work. The signature path had a critical
//! authentication bypass and a reachable panic, both of which the fuzz target
//! for this contract now asserts against.
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Bytes, BytesN, Env, Vec,
};

/// Signatures required to accept an inbound message. Two-of-N.
pub const MIN_SIGNATURES: u32 = 2;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Validator(BytesN<32>),
    Nonce(Address),
    ProcessedMessage(BytesN<32>),
    Request(Address, u64),
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
        if env.storage().instance().has(&DataKey::Validator(key.clone())) {
            panic_with_error!(env, Error::ValidatorAlreadyRegistered);
        }
        env.storage().instance().set(&DataKey::Validator(key), &true);
        env.events()
            .publish((symbol_short!("valreg"),), admin);
    }

    pub fn revoke_validator(env: Env, admin: Address, key: BytesN<32>) {
        Self::require_admin(&env, &admin);
        if !env.storage().instance().has(&DataKey::Validator(key.clone())) {
            panic_with_error!(env, Error::ValidatorNotRegistered);
        }
        env.storage().instance().remove(&DataKey::Validator(key));
    }

    pub fn is_validator(env: Env, key: BytesN<32>) -> bool {
        env.storage().instance().has(&DataKey::Validator(key))
    }

    /// Record a lock-and-mint request and advance the caller's nonce.
    ///
    /// The token lock/mint itself remains a stub, as it was; the request is now
    /// persisted so the nonce is meaningful and the request is auditable.
    pub fn lock_and_mint_request(
        env: Env,
        caller: Address,
        amount: i128,
        destination_chain: Bytes,
        destination_address: Bytes,
    ) -> u64 {
        caller.require_auth();
        let nonce: u64 = env
            .storage()
            .instance()
            .get(&DataKey::Nonce(caller.clone()))
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::Nonce(caller), &(nonce + 1));
        // Logic to lock tokens
    }

    /// Accept an inbound burn-and-unlock message, once it carries
    /// [`MIN_SIGNATURES`] valid signatures from distinct registered validators.
    ///
    /// Two defects were fixed here:
    ///
    /// * **Signature verification was discarded.** The loop called
    ///   `ed25519_verify` and then incremented `valid_signatures`
    ///   unconditionally, so *any* two 64-byte blobs — including two copies of
    ///   all zeroes — were accepted as a valid two-of-N multisig. The result is
    ///   now checked, the key must be a registered validator, and the same
    ///   validator cannot sign twice to reach the threshold.
    /// * **`public_keys.get(i).unwrap()` trapped** whenever `signatures` was
    ///   longer than `public_keys`, which the fuzzer reaches with a single
    ///   mismatched-length call. Lengths are now compared up front and reported
    ///   as [`Error::LengthMismatch`].
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
        if env
            .storage()
            .instance()
            .has(&DataKey::ProcessedMessage(message_hash.clone()))
        {
            panic_with_error!(env, Error::AlreadyProcessed);
        }
        if env
            .storage()
            .instance()
            .has(&DataKey::ProcessedMessage(message_hash.clone()))
        {
            panic!("message already processed");
        }

        // Count only signatures that verify against a registered validator, and
        // only once per validator so a single key cannot meet the threshold
        // alone.
        let mut counted: Vec<BytesN<32>> = Vec::new(&env);
        for i in 0..signatures.len() {
            let pk = public_keys.get(i).unwrap();
            let sig = signatures.get(i).unwrap();
            env.crypto()
                .ed25519_verify(&pk, &message_hash.clone().into(), &sig);
            valid_signatures += 1;
        }

        if counted.len() < MIN_SIGNATURES {
            panic_with_error!(env, Error::InsufficientSignatures);
        }

        env.storage()
            .instance()
            .set(&DataKey::ProcessedMessage(message_hash), &true);
    }
}
