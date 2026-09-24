#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, Address, Bytes, BytesN, Env, Vec};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Validator(BytesN<32>),
    Nonce(Address),
    ProcessedMessage(BytesN<32>),
}

#[contract]
pub struct BridgeProtocol;

#[contractimpl]
impl BridgeProtocol {
    pub fn lock_and_mint_request(
        env: Env,
        caller: Address,
        _amount: i128,
        _destination_chain: Bytes,
        _destination_address: Bytes,
    ) {
        caller.require_auth();
        let nonce: u64 = env.storage().instance().get(&DataKey::Nonce(caller.clone())).unwrap_or(0);
        env.storage().instance().set(&DataKey::Nonce(caller), &(nonce + 1));
        // Logic to lock tokens
    }

    pub fn burn_and_unlock_request(
        env: Env,
        caller: Address,
        _amount: i128,
        message_hash: BytesN<32>,
        signatures: Vec<BytesN<64>>,
        public_keys: Vec<BytesN<32>>,
        ttl: u64,
    ) {
        caller.require_auth();
        if env.ledger().timestamp() > ttl {
            panic!("message expired");
        }
        if env.storage().instance().has(&DataKey::ProcessedMessage(message_hash.clone())) {
            panic!("message already processed");
        }

        let mut valid_signatures = 0;
        for i in 0..signatures.len() {
            let pk = public_keys.get(i).unwrap();
            let sig = signatures.get(i).unwrap();
            env.crypto().ed25519_verify(&pk, &message_hash.clone().into(), &sig);
            valid_signatures += 1;
        }

        if valid_signatures < 2 {
            panic!("insufficient signatures");
        }

        env.storage().instance().set(&DataKey::ProcessedMessage(message_hash), &true);
    }
}
