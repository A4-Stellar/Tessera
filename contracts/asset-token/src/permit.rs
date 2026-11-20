use soroban_sdk::xdr::ToXdr;
use soroban_sdk::{
    contracttype, panic_with_error,
    xdr::{AccountId, PublicKey, ScAddress, ScVal, Uint256},
    Address, Bytes, BytesN, Env,
};

use crate::{DataKey, Error};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermitPayload {
    pub version: u32,
    pub contract: Address,
    pub network_id: BytesN<32>,
    pub nonce: u64,
    pub owner: Address,
    pub spender: Address,
    pub amount: i128,
    pub deadline_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowanceValue {
    pub amount: i128,
    pub expiration_ledger: u32,
}

pub(crate) fn permit(
    env: &Env,
    owner: &Address,
    spender: &Address,
    amount: i128,
    deadline_ledger: u32,
    signature: &BytesN<64>,
) {
    if amount < 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    if env.ledger().sequence() > deadline_ledger {
        panic_with_error!(env, Error::PermitExpired);
    }

    let nonce_key = DataKey::PermitNonceV1(owner.clone(), spender.clone());
    let nonce: u64 = env.storage().persistent().get(&nonce_key).unwrap_or(0);
    let payload = PermitPayload {
        version: 1,
        contract: env.current_contract_address(),
        network_id: env.ledger().network_id(),
        nonce,
        owner: owner.clone(),
        spender: spender.clone(),
        amount,
        deadline_ledger,
    };
    let digest: Bytes = env.crypto().sha256(&payload.to_xdr(env)).into();
    let public_key = owner_public_key(env, owner)
        .unwrap_or_else(|| panic_with_error!(env, Error::PermitInvalidOwnerKey));
    env.crypto().ed25519_verify(&public_key, &digest, signature);

    env.storage().persistent().set(&nonce_key, &(nonce + 1));
    env.storage().persistent().set(
        &DataKey::PermitAllowanceV1(owner.clone(), spender.clone()),
        &AllowanceValue {
            amount,
            expiration_ledger: deadline_ledger,
        },
    );
    env.events().publish(
        (
            soroban_sdk::symbol_short!("permit"),
            owner.clone(),
            spender.clone(),
        ),
        (amount, deadline_ledger, nonce),
    );
}

fn owner_public_key(env: &Env, owner: &Address) -> Option<BytesN<32>> {
    match ScVal::from(owner.clone()) {
        ScVal::Address(ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(
            Uint256(key),
        )))) => Some(BytesN::from_array(env, &key)),
        _ => None,
    }
}

pub(crate) fn allowance(env: &Env, owner: &Address, spender: &Address) -> i128 {
    let value: Option<AllowanceValue> = env
        .storage()
        .persistent()
        .get(&DataKey::PermitAllowanceV1(owner.clone(), spender.clone()));
    match value {
        Some(value) if value.expiration_ledger >= env.ledger().sequence() => value.amount,
        _ => 0,
    }
}

pub(crate) fn spend(env: &Env, owner: &Address, spender: &Address, amount: i128) {
    let key = DataKey::PermitAllowanceV1(owner.clone(), spender.clone());
    let value: AllowanceValue = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or(AllowanceValue {
            amount: 0,
            expiration_ledger: 0,
        });
    if value.expiration_ledger < env.ledger().sequence() {
        panic_with_error!(env, Error::AllowanceExpired);
    }
    if value.amount < amount {
        panic_with_error!(env, Error::InsufficientAllowance);
    }
    env.storage().persistent().set(
        &key,
        &AllowanceValue {
            amount: value.amount - amount,
            expiration_ledger: value.expiration_ledger,
        },
    );
}
