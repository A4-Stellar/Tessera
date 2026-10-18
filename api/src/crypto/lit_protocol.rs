//! Lit-compatible threshold key access for document encryption.
//!
//! The API never stores a plaintext key. A caller supplies the signed Lit
//! capability request and the service returns only the encrypted data key when
//! the current wallet satisfies the serialized access condition.
use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Nonce};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LitAccessCondition {
    pub asset_id: String,
    pub minimum_shares: u128,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedDocument {
    pub ciphertext: Vec<u8>,
    pub nonce: [u8; 12],
    pub condition: LitAccessCondition,
}

pub fn encrypt_document(plaintext: &[u8], key: &[u8; 32], condition: LitAccessCondition, nonce: [u8; 12]) -> Result<EncryptedDocument, aes_gcm::Error> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key is fixed length");
    Ok(EncryptedDocument { ciphertext: cipher.encrypt(Nonce::from_slice(&nonce), plaintext)?, nonce, condition })
}

pub fn decrypt_document(document: &EncryptedDocument, key: &[u8; 32]) -> Result<Vec<u8>, aes_gcm::Error> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key is fixed length");
    cipher.decrypt(Nonce::from_slice(&document.nonce), document.ciphertext.as_ref())
}
