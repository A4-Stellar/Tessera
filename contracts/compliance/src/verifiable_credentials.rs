//! Typed W3C Verifiable Credential verification with zero on-chain claim storage.

use soroban_sdk::{contracttype, Address, Bytes, BytesN, Env, String, Vec};
use soroban_sdk::xdr::ToXdr;
use crate::DataKey;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiableCredential {
    pub context: Vec<String>,
    pub credential_type: Vec<String>,
    pub issuer: Address,
    pub subject: Address,
    pub issued_at: u32,
    pub expires_at: u32,
    pub is_over_18: bool,
    pub proof_signature: BytesN<64>,
}

pub(crate) fn verify(env: &Env, credential: &VerifiableCredential) -> bool {
    if credential.subject == credential.issuer
        || credential.expires_at != 0 && env.ledger().sequence() >= credential.expires_at
        || !credential.is_over_18
        || credential.issued_at > env.ledger().sequence()
    {
        return false;
    }
    let key: Option<BytesN<32>> = env.storage().instance().get(&DataKey::IssuerKey(credential.issuer.clone()));
    let key = match key { Some(key) => key, None => return false };
    let mut message = Bytes::from_slice(env, b"TESSERA_W3C_VC_V1");
    message.append(&credential.subject.to_xdr(env));
    message.append(&credential.issuer.to_xdr(env));
    message.append(&Bytes::from_array(env, &credential.issued_at.to_be_bytes()));
    message.append(&Bytes::from_array(env, &credential.expires_at.to_be_bytes()));
    message.append(&Bytes::from_array(env, &[credential.is_over_18 as u8]));
    let digest: BytesN<32> = env.crypto().sha256(&message).into();
    env.crypto().ed25519_verify(&key, &Bytes::from(digest), &credential.proof_signature);
    true
}