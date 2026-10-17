/// Cryptographic hash-chained audit trail verifier.
/// #300: Append-only audit logs with SHA-256 hash chains and Stellar ledger commitment.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// Audit log entry with hash chain linkage.
/// #300: Hash-chain all logs sequentially: Hn = SHA256(Hn-1 || LogData).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub id: u64,
    pub timestamp: u64,
    pub event_type: String,
    pub actor: String,
    pub resource: String,
    pub action: String,
    pub details: String,
    /// SHA-256 hash of previous entry (chain linkage)
    pub previous_hash: [u8; 32],
    /// SHA-256 hash of this entry: H(previous_hash || data)
    pub entry_hash: [u8; 32],
}

impl AuditLogEntry {
    /// Create a new audit log entry with hash chain linkage.
    pub fn new(
        id: u64,
        event_type: String,
        actor: String,
        resource: String,
        action: String,
        details: String,
        previous_hash: [u8; 32],
    ) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let entry_hash = Self::compute_hash(id, timestamp, &previous_hash, &event_type, &actor, &resource, &action, &details);

        AuditLogEntry {
            id,
            timestamp,
            event_type,
            actor,
            resource,
            action,
            details,
            previous_hash,
            entry_hash,
        }
    }

    /// Compute SHA-256 hash of log entry with chain linkage.
    /// #300: Hn = SHA256(Hn-1 || LogData)
    fn compute_hash(
        id: u64,
        timestamp: u64,
        previous_hash: &[u8; 32],
        event_type: &str,
        actor: &str,
        resource: &str,
        action: &str,
        details: &str,
    ) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(previous_hash);
        hasher.update(id.to_le_bytes());
        hasher.update(timestamp.to_le_bytes());
        hasher.update(event_type.as_bytes());
        hasher.update(actor.as_bytes());
        hasher.update(resource.as_bytes());
        hasher.update(action.as_bytes());
        hasher.update(details.as_bytes());
        hasher.finalize().into()
    }
}

/// Hash-chained audit trail with integrity verification.
pub struct AuditTrailVerifier {
    entries: Vec<AuditLogEntry>,
    root_hash: [u8; 32],
    last_stellar_commitment: Option<String>,
}

impl AuditTrailVerifier {
    /// Create a new audit trail with empty state.
    pub fn new() -> Self {
        AuditTrailVerifier {
            entries: Vec::new(),
            root_hash: [0u8; 32],
            last_stellar_commitment: None,
        }
    }

    /// Append a new administrative event to the audit trail.
    /// #300: Hash-chain entries sequentially.
    pub fn append_event(
        &mut self,
        event_type: String,
        actor: String,
        resource: String,
        action: String,
        details: String,
    ) -> AuditLogEntry {
        let id = self.entries.len() as u64 + 1;
        let previous_hash = self.root_hash;

        let entry = AuditLogEntry::new(id, event_type, actor, resource, action, details, previous_hash);
        self.root_hash = entry.entry_hash;
        self.entries.push(entry.clone());

        entry
    }

    /// Verify the integrity of the entire audit trail.
    /// #300: Verify hash chain: each entry's previous_hash matches previous entry's entry_hash.
    pub fn verify_chain_integrity(&self) -> Result<(), String> {
        let mut expected_previous = [0u8; 32];

        for entry in &self.entries {
            if entry.previous_hash != expected_previous {
                return Err(format!(
                    "Chain broken at entry {}: expected previous {:?}, got {:?}",
                    entry.id, expected_previous, entry.previous_hash
                ));
            }
            expected_previous = entry.entry_hash;
        }

        Ok(())
    }

    /// Compute Merkle root hash for periodic Stellar ledger commitment.
    /// #300: Periodically commit audit root hashes to Stellar ledger.
    pub fn compute_root_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        for entry in &self.entries {
            hasher.update(entry.entry_hash);
        }
        hasher.finalize().into()
    }

    /// Record a commitment to the Stellar ledger.
    pub fn commit_to_stellar(&mut self, stellar_tx_hash: String) -> Result<(), String> {
        self.verify_chain_integrity()?;
        self.last_stellar_commitment = Some(stellar_tx_hash);
        Ok(())
    }

    /// Expose REST endpoint for auditor verification (GET /v1/audit/verify).
    /// #300: Auditors verify log chain integrity mathematically.
    pub fn verify_audit_trail(&self) -> AuditVerificationResponse {
        let chain_valid = self.verify_chain_integrity().is_ok();
        let root_hash = self.compute_root_hash();
        let entry_count = self.entries.len();

        AuditVerificationResponse {
            chain_valid,
            root_hash: root_hash.to_vec(),
            entry_count,
            last_stellar_commitment: self.last_stellar_commitment.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    /// Get audit entry by ID for detailed verification.
    pub fn get_entry(&self, id: u64) -> Option<AuditLogEntry> {
        self.entries.get(id as usize - 1).cloned()
    }

    /// Get all entries in range [start_id, end_id] for audit period verification.
    pub fn get_entries_range(&self, start_id: u64, end_id: u64) -> Vec<AuditLogEntry> {
        self.entries
            .iter()
            .filter(|e| e.id >= start_id && e.id <= end_id)
            .cloned()
            .collect()
    }
}

/// Response structure for /v1/audit/verify endpoint.
#[derive(Serialize, Deserialize)]
pub struct AuditVerificationResponse {
    pub chain_valid: bool,
    pub root_hash: Vec<u8>,
    pub entry_count: usize,
    pub last_stellar_commitment: Option<String>,
    pub timestamp: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_chain_integrity() {
        let mut trail = AuditTrailVerifier::new();

        trail.append_event(
            "user_created".to_string(),
            "admin".to_string(),
            "user:123".to_string(),
            "CREATE".to_string(),
            "New user registered".to_string(),
        );

        trail.append_event(
            "permission_granted".to_string(),
            "admin".to_string(),
            "user:123".to_string(),
            "GRANT".to_string(),
            "Admin role assigned".to_string(),
        );

        assert!(trail.verify_chain_integrity().is_ok());
    }

    #[test]
    fn test_entry_sequential_hash_chaining() {
        let mut trail = AuditTrailVerifier::new();
        let entry1 = trail.append_event(
            "event1".to_string(),
            "actor1".to_string(),
            "resource1".to_string(),
            "ACTION1".to_string(),
            "details1".to_string(),
        );

        let entry2 = trail.append_event(
            "event2".to_string(),
            "actor2".to_string(),
            "resource2".to_string(),
            "ACTION2".to_string(),
            "details2".to_string(),
        );

        // Second entry should have first entry's hash as previous
        assert_eq!(entry2.previous_hash, entry1.entry_hash);
    }
}
