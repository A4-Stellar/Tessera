//! Append-only Merkle Mountain Range for administrative audit entries.
//!
//! Each leaf is `SHA256(prev_peak_hash || timestamp || payload_hash)`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::storage::rocksdb::RocksLedger;

pub type AuditHash = [u8; 32];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditMmrLeaf {
    pub leaf_index: usize,
    pub timestamp: String,
    pub payload_hash: String,
    pub prev_peak_hash: String,
    pub leaf_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditMmrProof {
    pub leaf_index: usize,
    pub leaf_count: usize,
    pub leaf_hash: String,
    pub path: Vec<String>,
    pub peaks: Vec<String>,
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditMmrProofResponse {
    pub sequence: u64,
    pub timestamp: String,
    pub payload_hash: String,
    pub prev_peak_hash: String,
    pub leaf_index: usize,
    pub leaf_count: usize,
    pub leaf_hash: String,
    pub path: Vec<String>,
    pub peaks: Vec<String>,
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditMmrState {
    pub leaf_count: usize,
    pub leaves: Vec<AuditMmrLeaf>,
    pub levels: Vec<Vec<AuditHash>>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuditMmrError {
    #[error("invalid 32-byte hash")]
    InvalidHash,
    #[error("RocksDB persistence failed: {0}")]
    RocksDb(#[from] rocksdb::Error),
    #[error("proof unavailable for leaf {0}")]
    ProofUnavailable(usize),
    #[error("invalid persisted MMR state: {0}")]
    InvalidState(String),
}

pub fn sha256_bytes(parts: &[&[u8]]) -> AuditHash {
    let mut h = Sha256::new();
    for part in parts {
        h.update(part);
    }
    h.finalize().into()
}

/// Canonical audit payload hash: SHA256 over canonical JSON bytes.
pub fn audit_payload_hash(payload: &serde_json::Value) -> AuditHash {
    let bytes = serde_json::to_vec(payload).unwrap_or_default();
    sha256_bytes(&[&bytes])
}

/// Required audit leaf hash. Timestamp bytes are the canonical RFC3339 string.
pub fn audit_leaf_hash(
    prev_peak_hash: &[u8; 32],
    timestamp: &str,
    payload_hash: &[u8; 32],
) -> AuditHash {
    sha256_bytes(&[prev_peak_hash, timestamp.as_bytes(), payload_hash])
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> AuditHash {
    sha256_bytes(&[left, right])
}

fn mountains(leaf_count: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for height in (0..=52).rev() {
        let size = 1usize << height;
        if (leaf_count / size) % 2 == 1 {
            out.push((start, height));
            start += size;
        }
    }
    out
}

#[derive(Clone)]
pub struct AuditMmr {
    levels: Vec<Vec<AuditHash>>,
    leaves: Vec<AuditMmrLeaf>,
    leaf_count: usize,
    persistence: Option<RocksLedger>,
}

impl std::fmt::Debug for AuditMmr {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuditMmr")
            .field("levels", &self.levels)
            .field("leaves", &self.leaves)
            .field("leaf_count", &self.leaf_count)
            .finish()
    }
}

impl Default for AuditMmr {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditMmr {
    pub fn new() -> Self {
        Self {
            levels: Vec::new(),
            leaves: Vec::new(),
            leaf_count: 0,
            persistence: None,
        }
    }

    pub fn with_rocksdb(path: impl AsRef<std::path::Path>) -> Result<Self, AuditMmrError> {
        let persistence = RocksLedger::open(path)?;
        let mut mmr = Self {
            persistence: Some(persistence),
            ..Self::new()
        };
        if let Some(state_json) = mmr.persistence.as_ref().unwrap().get_audit_mmr_state()? {
            let state: AuditMmrState = serde_json::from_slice(&state_json)
                .map_err(|error| AuditMmrError::InvalidState(error.to_string()))?;
            mmr.restore_state(state)?;
        }
        // Preserve compatibility with the peak-only key written by older builds.
        if mmr.leaf_count == 0 {
            if let Some(peaks) = mmr.persistence.as_ref().unwrap().get_audit_mmr_peaks()? {
                // Peaks alone cannot reconstruct historical leaves or proof paths.
                // Do not set a false leaf_count or fabricate leaf state.
                let _ = peaks;
            }
        }
        Ok(mmr)
    }

    pub fn load_audit_entries(&self) -> Result<Vec<Vec<u8>>, AuditMmrError> {
        match &self.persistence {
            Some(db) => Ok(db.get_audit_entries()?),
            None => Ok(Vec::new()),
        }
    }

    fn restore_state(&mut self, state: AuditMmrState) -> Result<(), AuditMmrError> {
        if state.leaf_count != state.leaves.len() {
            return Err(AuditMmrError::InvalidState(
                "leaf_count and leaf count disagree".to_string(),
            ));
        }
        if state.levels.is_empty() {
            if state.leaf_count == 0 {
                return Ok(());
            }
            return Err(AuditMmrError::InvalidState(
                "missing MMR levels".to_string(),
            ));
        }
        if state.levels.first().map_or(0, Vec::len) != state.leaf_count {
            return Err(AuditMmrError::InvalidState(
                "level-zero leaf count disagrees".to_string(),
            ));
        }
        for (i, leaf) in state.leaves.iter().enumerate() {
            if leaf.leaf_index != i {
                return Err(AuditMmrError::InvalidState(
                    "leaf indices are not contiguous".to_string(),
                ));
            }
            let (Ok(payload_hash), Ok(prev_peak_hash), Ok(leaf_hash)) = (
                hex::decode(&leaf.payload_hash),
                hex::decode(&leaf.prev_peak_hash),
                hex::decode(&leaf.leaf_hash),
            ) else {
                return Err(AuditMmrError::InvalidState(
                    "leaf hashes are not hexadecimal".to_string(),
                ));
            };
            if payload_hash.len() != 32
                || prev_peak_hash.len() != 32
                || leaf_hash.len() != 32
            {
                return Err(AuditMmrError::InvalidState(
                    "leaf hashes are not 32 bytes".to_string(),
                ));
            }
            let mut expected_payload = [0u8; 32];
            expected_payload.copy_from_slice(&payload_hash);
            let mut expected_prev = [0u8; 32];
            expected_prev.copy_from_slice(&prev_peak_hash);
            let mut stored_leaf = [0u8; 32];
            stored_leaf.copy_from_slice(&leaf_hash);
            if audit_leaf_hash(&expected_prev, &leaf.timestamp, &expected_payload) != stored_leaf {
                return Err(AuditMmrError::InvalidState(
                    "restored leaf hash does not match its inputs".to_string(),
                ));
            }
        }
        let mut reconstructed = AuditMmr::new();
        for leaf in &state.leaves {
            let payload = hex::decode(&leaf.payload_hash).map_err(|_| {
                AuditMmrError::InvalidState("invalid restored payload hash".to_string())
            })?;
            let mut payload_hash = [0u8; 32];
            if payload.len() != 32 {
                return Err(AuditMmrError::InvalidState(
                    "restored payload hash is not 32 bytes".to_string(),
                ));
            }
            payload_hash.copy_from_slice(&payload);
            let rebuilt = reconstructed.append_str(&leaf.timestamp, &payload_hash)?;
            if rebuilt.leaf_hash != leaf.leaf_hash {
                return Err(AuditMmrError::InvalidState(
                    "restored prev_peak_hash chain is inconsistent".to_string(),
                ));
            }
        }
        for height in 0..state.levels.len().saturating_sub(1) {
            let level = &state.levels[height];
            let next = &state.levels[height + 1];
            if next.len() != level.len() / 2 {
                return Err(AuditMmrError::InvalidState(
                    "MMR level sizes are inconsistent".to_string(),
                ));
            }
            for i in 0..next.len() {
                let expected = node_hash(&level[i * 2], &level[i * 2 + 1]);
                if next[i] != expected {
                    return Err(AuditMmrError::InvalidState(
                        "MMR level hash is inconsistent".to_string(),
                    ));
                }
            }
        }
        if reconstructed.levels != state.levels {
            return Err(AuditMmrError::InvalidState(
                "restored MMR levels do not match reconstructed levels".to_string(),
            ));
        }

        self.leaf_count = state.leaf_count;
        self.leaves = state.leaves;
        self.levels = state.levels;
        Ok(())
    }

    pub fn leaf_count(&self) -> usize {
        self.leaf_count
    }

    pub fn leaves(&self) -> &[AuditMmrLeaf] {
        &self.leaves
    }

    pub fn peaks(&self) -> Vec<AuditHash> {
        mountains(self.leaf_count)
            .into_iter()
            .filter_map(|(start, height)| {
                self.levels
                    .get(height)
                    .and_then(|level| level.get(start >> height))
                    .copied()
            })
            .collect()
    }

    pub fn prev_peak_hash(&self) -> AuditHash {
        self.root()
    }

    pub fn root(&self) -> AuditHash {
        let peaks = self.peaks();
        match peaks.as_slice() {
            [] => [0u8; 32],
            [peak] => *peak,
            _ => {
                let mut bagged = *peaks.last().unwrap();
                for peak in peaks.iter().rev().skip(1) {
                    bagged = node_hash(peak, &bagged);
                }
                bagged
            }
        }
    }

    pub fn append(
        &mut self,
        timestamp: &DateTime<Utc>,
        payload_hash: &[u8; 32],
    ) -> Result<AuditMmrLeaf, AuditMmrError> {
        self.append_str(&timestamp.to_rfc3339(), payload_hash)
    }

    pub fn append_str(
        &mut self,
        timestamp: &str,
        payload_hash: &[u8; 32],
    ) -> Result<AuditMmrLeaf, AuditMmrError> {
        let leaf = self.append_inner(timestamp, payload_hash);
        self.persist_state()?;
        Ok(leaf)
    }

    pub fn append_with_entries(
        &mut self,
        timestamp: &str,
        payload_hash: &[u8; 32],
        sequence: u64,
        entry_json: &[u8],
    ) -> Result<AuditMmrLeaf, AuditMmrError> {
        let mut staged = self.clone();
        let leaf = staged.append_inner(timestamp, payload_hash);
        if let Some(db) = &staged.persistence {
            let state_json = staged.state_json()?;
            db.write_audit_mmr_snapshot(
                &staged.peaks(),
                &state_json,
                sequence,
                entry_json,
            )?;
        }
        *self = staged;
        Ok(leaf)
    }

    fn append_inner(&mut self, timestamp: &str, payload_hash: &[u8; 32]) -> AuditMmrLeaf {
        let prev_peak_hash = self.prev_peak_hash();
        let leaf_hash = audit_leaf_hash(&prev_peak_hash, timestamp, payload_hash);
        let leaf_index = self.leaf_count;

        if self.levels.is_empty() {
            self.levels = vec![Vec::new()];
        }
        self.levels[0].push(leaf_hash);
        let mut height = 0usize;
        while self.levels[height].len() % 2 == 0 {
            let len = self.levels[height].len();
            let parent = node_hash(
                &self.levels[height][len - 2],
                &self.levels[height][len - 1],
            );
            if self.levels.len() <= height + 1 {
                self.levels.push(Vec::new());
            }
            self.levels[height + 1].push(parent);
            height += 1;
        }
        self.leaf_count += 1;

        let leaf = AuditMmrLeaf {
            leaf_index,
            timestamp: timestamp.to_string(),
            payload_hash: hex::encode(payload_hash),
            prev_peak_hash: hex::encode(prev_peak_hash),
            leaf_hash: hex::encode(leaf_hash),
        };
        self.leaves.push(leaf.clone());
        leaf
    }

    fn state_json(&self) -> Result<Vec<u8>, AuditMmrError> {
        serde_json::to_vec(&AuditMmrState {
            leaf_count: self.leaf_count,
            leaves: self.leaves.clone(),
            levels: self.levels.clone(),
        })
        .map_err(|error| AuditMmrError::InvalidState(error.to_string()))
    }

    fn persist_state(&self) -> Result<(), AuditMmrError> {
        if let Some(db) = &self.persistence {
            db.put_audit_mmr_peaks(&self.peaks())?;
            db.put_audit_mmr_state(&self.state_json()?)?;
        }
        Ok(())
    }

    pub fn proof(&self, leaf_index: usize) -> Option<AuditMmrProof> {
        if leaf_index >= self.leaf_count || leaf_index >= self.leaves.len() {
            return None;
        }
        let mountains = mountains(self.leaf_count);
        let pos = mountains
            .iter()
            .position(|(start, height)| leaf_index >= *start && leaf_index < *start + (1usize << height))?;
        let (start, height) = mountains[pos];
        let mut path = Vec::with_capacity(height);
        let mut idx = leaf_index - start;
        for level in 0..height {
            let sibling_idx = idx ^ 1;
            let level_index = (start >> level) + sibling_idx;
            let sibling = self.levels[level].get(level_index)?;
            path.push(hex::encode(sibling));
            idx >>= 1;
        }
        let peaks = self.peaks();
        Some(AuditMmrProof {
            leaf_index,
            leaf_count: self.leaf_count,
            leaf_hash: self.leaves[leaf_index].leaf_hash.clone(),
            path,
            peaks: peaks.iter().map(hex::encode).collect(),
            root: hex::encode(self.root()),
        })
    }
}

pub fn verify_audit_mmr_proof(leaf: &AuditHash, proof: &AuditMmrProof) -> bool {
    if proof.leaf_index >= proof.leaf_count {
        return false;
    }
    let Ok(leaf_bytes) = hex::decode(&proof.leaf_hash) else {
        return false;
    };
    if leaf_bytes.len() != 32 {
        return false;
    }
    let mut acc = [0u8; 32];
    acc.copy_from_slice(&leaf_bytes);
    if &acc != leaf {
        return false;
    }

    let all = mountains(proof.leaf_count);
    let Some(pos) = all
        .iter()
        .position(|(start, height)| {
            proof.leaf_index >= *start
                && proof.leaf_index < *start + (1usize << height)
        })
    else {
        return false;
    };
    let (start, height) = all[pos];
    if proof.path.len() != height {
        return false;
    }

    let mut idx = proof.leaf_index - start;
    for sibling_hex in &proof.path {
        let Ok(sibling_bytes) = hex::decode(sibling_hex) else {
            return false;
        };
        if sibling_bytes.len() != 32 {
            return false;
        }
        let mut sibling = [0u8; 32];
        sibling.copy_from_slice(&sibling_bytes);
        acc = if idx % 2 == 0 {
            node_hash(&acc, &sibling)
        } else {
            node_hash(&sibling, &acc)
        };
        idx /= 2;
    }

    let mut peaks = Vec::with_capacity(proof.peaks.len());
    for peak_hex in &proof.peaks {
        let Ok(bytes) = hex::decode(peak_hex) else {
            return false;
        };
        if bytes.len() != 32 {
            return false;
        }
        let mut peak = [0u8; 32];
        peak.copy_from_slice(&bytes);
        peaks.push(peak);
    }
    if peaks.len() != all.len() || peaks[pos] != acc {
        return false;
    }

    let mut bagged = *peaks.last().unwrap();
    for peak in peaks.iter().rev().skip(1) {
        bagged = node_hash(peak, &bagged);
    }
    hex::encode(bagged).eq_ignore_ascii_case(&proof.root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngCore;

    #[test]
    fn leaf_hash_is_exactly_concatenated_sha256_input() {
        let prev = [0x11; 32];
        let payload = [0x22; 32];
        let timestamp = "2026-11-21T15:53:10Z";
        let mut h = Sha256::new();
        h.update(prev);
        h.update(timestamp.as_bytes());
        h.update(payload);
        let expected: AuditHash = h.finalize().into();
        assert_eq!(audit_leaf_hash(&prev, timestamp, &payload), expected);
    }

    #[test]
    fn inclusion_proofs_hold_across_leaf_count_sweep() {
        for leaf_count in 1..=64usize {
            let mut mmr = AuditMmr::new();
            for _ in 0..leaf_count {
                let mut payload = [0u8; 32];
                rand::rng().fill_bytes(&mut payload);
                let timestamp = format!(
                    "2026-01-01T00:{:02}:{:02}Z",
                    mmr.leaf_count() / 60,
                    mmr.leaf_count() % 60
                );
                mmr.append_str(&timestamp, &payload).unwrap();
            }
            for i in 0..leaf_count {
                let proof = mmr.proof(i).unwrap();
                let leaf = hex::decode(&proof.leaf_hash).unwrap();
                let mut leaf_hash = [0u8; 32];
                leaf_hash.copy_from_slice(&leaf);
                assert!(verify_audit_mmr_proof(&leaf_hash, &proof), "leaf {i}");
            }
        }
    }
}
