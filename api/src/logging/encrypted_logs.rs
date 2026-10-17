/// Zero-trust encrypted logging subsystem using AEAD encryption.
/// #303: Encrypt log payloads before transmission to external providers.
/// Ensures zero plaintext PII or sensitive keys in container console streams.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Key version header for offline decryption by authorized teams.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyVersionHeader {
    pub version: u32,
    pub algorithm: String, // "ChaCha20-Poly1305" or "AES-256-GCM"
    pub timestamp: u64,
}

/// Encrypted log entry with metadata for decryption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedLogEntry {
    pub key_version: KeyVersionHeader,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub aad: Vec<u8>, // Additional authenticated data (e.g., timestamp, requestID)
}

/// Log encryption context managing keys and nonces.
pub struct LogEncryptor {
    cipher: ChaCha20Poly1305,
    key_version: u32,
    algorithm: String,
}

impl LogEncryptor {
    /// Create a new log encryptor with ChaCha20-Poly1305.
    /// #303: Key initialization for authenticated encryption.
    pub fn new(key: &[u8; 32], key_version: u32) -> Result<Self, String> {
        let cipher = ChaCha20Poly1305::new_from_slice(key)
            .map_err(|_| "Invalid key size".to_string())?;

        Ok(LogEncryptor {
            cipher,
            key_version,
            algorithm: "ChaCha20-Poly1305".to_string(),
        })
    }

    /// Encrypt a log payload with authenticated additional data.
    /// #303: AEAD encryption guarantees confidentiality and authenticity.
    pub fn encrypt(
        &self,
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<EncryptedLogEntry, String> {
        let mut rng = rand::thread_rng();
        let mut nonce_bytes = [0u8; 12];
        rng.fill(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let payload = Payload {
            msg: plaintext,
            aad,
        };

        let ciphertext = self
            .cipher
            .encrypt(nonce, payload)
            .map_err(|e| format!("Encryption failed: {}", e))?;

        Ok(EncryptedLogEntry {
            key_version: KeyVersionHeader {
                version: self.key_version,
                algorithm: self.algorithm.clone(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            },
            nonce: nonce_bytes.to_vec(),
            ciphertext,
            aad: aad.to_vec(),
        })
    }

    /// Decrypt a log entry using stored nonce and AAD.
    /// #303: Authorized security teams can decrypt logs offline using key version.
    pub fn decrypt(&self, entry: &EncryptedLogEntry) -> Result<Vec<u8>, String> {
        if entry.nonce.len() != 12 {
            return Err("Invalid nonce length".to_string());
        }

        let nonce = Nonce::from_slice(&entry.nonce);
        let payload = Payload {
            msg: &entry.ciphertext,
            aad: &entry.aad,
        };

        self.cipher
            .decrypt(nonce, payload)
            .map_err(|e| format!("Decryption failed: {}", e))
    }
}

/// Secure log exporter that encrypts logs before streaming.
pub struct SecureLogExporter {
    encryptor: LogEncryptor,
    buffer: Vec<EncryptedLogEntry>,
}

impl SecureLogExporter {
    /// Create a new secure log exporter.
    pub fn new(key: &[u8; 32], key_version: u32) -> Result<Self, String> {
        let encryptor = LogEncryptor::new(key, key_version)?;
        Ok(SecureLogExporter {
            encryptor,
            buffer: Vec::new(),
        })
    }

    /// Add a plaintext log to the export queue (encrypted before storage).
    /// #303: Zero plaintext PII or sensitive keys in this buffer.
    pub fn enqueue_log(&mut self, message: &str, context: &str) -> Result<(), String> {
        let aad = format!("context:{}", context);
        let encrypted = self.encryptor.encrypt(message.as_bytes(), aad.as_bytes())?;
        self.buffer.push(encrypted);
        Ok(())
    }

    /// Export all encrypted logs for transmission to external provider.
    pub fn export(&mut self) -> Result<Vec<EncryptedLogEntry>, String> {
        Ok(std::mem::take(&mut self.buffer))
    }

    /// Get the current buffer size (number of pending encrypted entries).
    pub fn buffer_size(&self) -> usize {
        self.buffer.len()
    }
}

impl fmt::Debug for LogEncryptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogEncryptor")
            .field("algorithm", &self.algorithm)
            .field("key_version", &self.key_version)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [1u8; 32];
        let encryptor = LogEncryptor::new(&key, 1).unwrap();
        let plaintext = b"Sensitive log message with PII";
        let aad = b"request_id:12345";

        let encrypted = encryptor.encrypt(plaintext, aad).unwrap();
        let decrypted = encryptor.decrypt(&encrypted).unwrap();

        assert_eq!(plaintext, &decrypted[..]);
    }

    #[test]
    fn test_encryption_changes_plaintext() {
        let key = [1u8; 32];
        let encryptor = LogEncryptor::new(&key, 1).unwrap();
        let plaintext = b"Test message";
        let aad = b"context:test";

        let encrypted = encryptor.encrypt(plaintext, aad).unwrap();
        assert_ne!(&encrypted.ciphertext, plaintext);
    }
}
