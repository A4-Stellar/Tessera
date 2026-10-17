//! Zero-trust encrypted log envelopes (Issue #303).
//!
//! Every log line is scrubbed of PII and secrets, then sealed with AES-256-GCM
//! before it leaves the process. The envelope is the only thing written to the
//! console stream or shipped to an external logging provider:
//!
//! ```json
//! {"alg":"AES-256-GCM","key_version":2,"nonce":"<hex>","ciphertext":"<hex>"}
//! ```
//!
//! * `key_version` is a header that tells the security team which key to use
//!   for offline decryption. It is bound into the GCM additional authenticated
//!   data, so flipping it makes decryption fail instead of silently selecting
//!   another key.
//! * Each envelope uses a fresh 96-bit random nonce.
//! * Keys live in a [`LogKeyRing`]; key material is never printed or serialized.

use std::collections::BTreeMap;

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Key, Nonce,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::middleware::pii_scrubber::scrub;

/// Algorithm identifier written into every envelope.
pub const ALGORITHM: &str = "AES-256-GCM";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LogCryptoError {
    #[error("no key registered for log key version {0}")]
    UnknownKeyVersion(u32),
    #[error("active key version {0} is not present in the key ring")]
    MissingActiveKey(u32),
    #[error("unsupported envelope algorithm: {0}")]
    UnsupportedAlgorithm(String),
    #[error("malformed envelope: {0}")]
    Malformed(String),
    #[error("authentication failed: envelope was tampered with or the wrong key was used")]
    AuthenticationFailed,
}

/// Versioned set of log encryption keys with one active version for sealing.
#[derive(Clone)]
pub struct LogKeyRing {
    active: u32,
    keys: BTreeMap<u32, [u8; KEY_LEN]>,
}

// Manual impl so key bytes never appear in debug output.
impl std::fmt::Debug for LogKeyRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogKeyRing")
            .field("active", &self.active)
            .field("versions", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl LogKeyRing {
    /// Builds a key ring. `active` must be one of the registered versions.
    pub fn new(active: u32, keys: BTreeMap<u32, [u8; KEY_LEN]>) -> Result<Self, LogCryptoError> {
        if !keys.contains_key(&active) {
            return Err(LogCryptoError::MissingActiveKey(active));
        }
        Ok(Self { active, keys })
    }

    /// Version used for all newly sealed envelopes.
    pub fn active_version(&self) -> u32 {
        self.active
    }

    fn cipher(&self, version: u32) -> Result<Aes256Gcm, LogCryptoError> {
        let key = self
            .keys
            .get(&version)
            .ok_or(LogCryptoError::UnknownKeyVersion(version))?;
        Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key)))
    }
}

/// The on-the-wire log record. Contains no plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedLogEnvelope {
    pub alg: String,
    pub key_version: u32,
    /// Hex-encoded 96-bit nonce.
    pub nonce: String,
    /// Hex-encoded ciphertext with the 128-bit GCM tag appended.
    pub ciphertext: String,
}

/// Binds the algorithm and key version into the authenticated data.
fn associated_data(alg: &str, key_version: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(alg.len() + 4);
    aad.extend_from_slice(alg.as_bytes());
    aad.extend_from_slice(&key_version.to_be_bytes());
    aad
}

/// Encrypts `plaintext` under the ring's active key.
pub fn seal(ring: &LogKeyRing, plaintext: &[u8]) -> Result<EncryptedLogEnvelope, LogCryptoError> {
    let version = ring.active_version();
    let cipher = ring.cipher(version)?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext,
                aad: &associated_data(ALGORITHM, version),
            },
        )
        .map_err(|_| LogCryptoError::AuthenticationFailed)?;

    Ok(EncryptedLogEnvelope {
        alg: ALGORITHM.to_string(),
        key_version: version,
        nonce: hex::encode(nonce_bytes),
        ciphertext: hex::encode(ciphertext),
    })
}

/// Decrypts an envelope using the key version named in its header.
pub fn open(ring: &LogKeyRing, envelope: &EncryptedLogEnvelope) -> Result<Vec<u8>, LogCryptoError> {
    if envelope.alg != ALGORITHM {
        return Err(LogCryptoError::UnsupportedAlgorithm(envelope.alg.clone()));
    }
    let nonce = hex::decode(&envelope.nonce)
        .map_err(|e| LogCryptoError::Malformed(format!("nonce: {e}")))?;
    if nonce.len() != NONCE_LEN {
        return Err(LogCryptoError::Malformed(format!(
            "nonce must be {NONCE_LEN} bytes, found {}",
            nonce.len()
        )));
    }
    let ciphertext = hex::decode(&envelope.ciphertext)
        .map_err(|e| LogCryptoError::Malformed(format!("ciphertext: {e}")))?;

    let cipher = ring.cipher(envelope.key_version)?;
    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &associated_data(&envelope.alg, envelope.key_version),
            },
        )
        .map_err(|_| LogCryptoError::AuthenticationFailed)
}

/// Scrubs PII and secrets from a raw log line, seals it, and returns the
/// single-line JSON envelope that is safe to write to the console stream.
pub fn seal_log_line(ring: &LogKeyRing, line: &str) -> Result<String, LogCryptoError> {
    let redacted = scrub(line);
    let envelope = seal(ring, redacted.as_bytes())?;
    serde_json::to_string(&envelope).map_err(|e| LogCryptoError::Malformed(e.to_string()))
}

/// Inverse of [`seal_log_line`], used by security teams during offline review.
pub fn open_log_line(ring: &LogKeyRing, json: &str) -> Result<String, LogCryptoError> {
    let envelope: EncryptedLogEnvelope =
        serde_json::from_str(json).map_err(|e| LogCryptoError::Malformed(e.to_string()))?;
    let plaintext = open(ring, &envelope)?;
    String::from_utf8(plaintext).map_err(|e| LogCryptoError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(active: u32, versions: &[(u32, u8)]) -> LogKeyRing {
        let keys = versions
            .iter()
            .map(|&(v, b)| (v, [b; KEY_LEN]))
            .collect::<BTreeMap<_, _>>();
        LogKeyRing::new(active, keys).unwrap()
    }

    #[test]
    fn roundtrip_restores_original_line_after_scrubbing() {
        let ring = ring(1, &[(1, 7)]);
        let line = "request served for user alice@example.com";
        let sealed = seal_log_line(&ring, line).unwrap();
        let opened = open_log_line(&ring, &sealed).unwrap();
        assert!(!opened.contains("alice@example.com"));
        assert_eq!(opened, &*scrub(line));
    }

    #[test]
    fn sealed_output_contains_no_plaintext_pii() {
        let ring = ring(1, &[(1, 7)]);
        let sealed = seal_log_line(&ring, "login from bob@example.com").unwrap();
        assert!(!sealed.contains("bob@example.com"));
        assert!(!sealed.contains("login"));
    }

    #[test]
    fn envelope_carries_active_key_version_header() {
        let ring = ring(3, &[(2, 1), (3, 2)]);
        let envelope = seal(&ring, b"event").unwrap();
        assert_eq!(envelope.key_version, 3);
        assert_eq!(envelope.alg, ALGORITHM);
    }

    #[test]
    fn older_key_versions_still_decrypt_after_rotation() {
        let old = ring(1, &[(1, 9)]);
        let sealed = seal(&old, b"before rotation").unwrap();
        let rotated = ring(2, &[(1, 9), (2, 5)]);
        assert_eq!(open(&rotated, &sealed).unwrap(), b"before rotation");
    }

    #[test]
    fn tampered_key_version_header_is_rejected() {
        let ring = ring(1, &[(1, 7), (2, 7)]);
        let mut envelope = seal(&ring, b"payload").unwrap();
        envelope.key_version = 2;
        assert_eq!(
            open(&ring, &envelope),
            Err(LogCryptoError::AuthenticationFailed)
        );
    }

    #[test]
    fn wrong_key_material_is_rejected() {
        let sealed = seal(&ring(1, &[(1, 7)]), b"payload").unwrap();
        let other = ring(1, &[(1, 8)]);
        assert_eq!(
            open(&other, &sealed),
            Err(LogCryptoError::AuthenticationFailed)
        );
    }

    #[test]
    fn unknown_key_version_is_reported() {
        let sealed = seal(&ring(4, &[(4, 1)]), b"payload").unwrap();
        let ring = ring(1, &[(1, 1)]);
        assert_eq!(
            open(&ring, &sealed),
            Err(LogCryptoError::UnknownKeyVersion(4))
        );
    }

    #[test]
    fn nonces_are_unique_per_envelope() {
        let ring = ring(1, &[(1, 7)]);
        let a = seal(&ring, b"same").unwrap();
        let b = seal(&ring, b"same").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn active_version_must_exist_in_ring() {
        let keys = BTreeMap::from([(1u32, [0u8; KEY_LEN])]);
        assert_eq!(
            LogKeyRing::new(2, keys).unwrap_err(),
            LogCryptoError::MissingActiveKey(2)
        );
    }

    #[test]
    fn debug_output_does_not_leak_key_bytes() {
        let ring = ring(1, &[(1, 0xAB)]);
        let rendered = format!("{ring:?}");
        assert!(rendered.contains("versions"));
        assert!(!rendered.contains("171"));
        assert!(!rendered.contains("AB"));
    }
}
