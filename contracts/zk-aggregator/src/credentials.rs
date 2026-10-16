//! Issuer credential trees, queued individual credentials, and the
//! aggregation accumulator.

use std::collections::BTreeSet;

use ark_bn254::Fr;
use ark_crypto_primitives::sponge::poseidon::PoseidonConfig;
use ark_ff::Zero;
use serde::{Deserialize, Serialize};

use ark_ff::PrimeField;
use sha2::{Digest, Sha256};

use crate::encode::{fr_from_hex, fr_to_bytes, fr_to_hex, subject_field};
use crate::poseidon2;

/// One investor's credential, as issued into the aggregation queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    /// Stellar strkey (`G…` account or `C…` contract).
    pub subject: String,
    /// Ledger sequence at which the credential stops being valid.
    pub expiry: u32,
    /// Leaf index in the issuer's tree.
    pub index: u64,
    /// Sibling hashes from the leaf up (big-endian scalar hex).
    pub path: Vec<String>,
    /// Root the path leads to (big-endian scalar hex).
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    BadSubject(String),
    BadPath(String),
    Expired(String, u32, u32),
    Duplicate(String),
    WrongDepth(String, usize, usize),
    Empty,
    TooMany(usize, usize),
    BadHex(String),
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadSubject(s) => write!(f, "{s}: invalid subject address"),
            Self::BadPath(s) => write!(f, "{s}: Merkle path does not lead to the issuer root"),
            Self::Expired(s, e, v) => write!(f, "{s}: credential expires at {e}, not after {v}"),
            Self::Duplicate(s) => write!(f, "{s}: appears more than once in the batch"),
            Self::WrongDepth(s, got, want) => {
                write!(f, "{s}: path length {got} does not match tree depth {want}")
            }
            Self::Empty => write!(f, "batch is empty"),
            Self::TooMany(n, slots) => {
                write!(f, "batch of {n} exceeds the circuit's {slots} slots")
            }
            Self::BadHex(h) => write!(f, "bad hex field element: {h}"),
        }
    }
}

impl std::error::Error for CredentialError {}

pub fn leaf_hash(config: &PoseidonConfig<Fr>, subject: Fr, expiry: u32) -> Fr {
    poseidon2(config, subject, Fr::from(expiry as u64))
}

/// A fixed-depth Poseidon Merkle tree of credential leaves. Empty positions
/// hold zero leaves.
pub struct CredentialTree {
    depth: usize,
    /// `levels[0]` are leaves; `levels[depth]` is `[root]`.
    levels: Vec<Vec<Fr>>,
    entries: Vec<(String, u32)>,
}

impl CredentialTree {
    /// Build a tree over `(subject strkey, expiry ledger)` entries.
    pub fn build(
        config: &PoseidonConfig<Fr>,
        depth: usize,
        entries: Vec<(String, u32)>,
    ) -> Result<Self, CredentialError> {
        assert!(depth < 64, "depth too large");
        assert!(
            (entries.len() as u128) <= (1u128 << depth),
            "too many entries for depth"
        );
        let mut leaves = Vec::with_capacity(entries.len());
        for (subject, expiry) in &entries {
            let s = subject_field(subject)
                .ok_or_else(|| CredentialError::BadSubject(subject.clone()))?;
            leaves.push(leaf_hash(config, s, *expiry));
        }

        // Zero subtrees so only populated paths are hashed.
        let mut zeros = vec![Fr::zero()];
        for h in 0..depth {
            zeros.push(poseidon2(config, zeros[h], zeros[h]));
        }

        let mut levels = vec![leaves];
        for h in 0..depth {
            let prev = &levels[h];
            let mut next = Vec::with_capacity(prev.len().div_ceil(2));
            for pair in prev.chunks(2) {
                let left = pair[0];
                let right = pair.get(1).copied().unwrap_or(zeros[h]);
                next.push(poseidon2(config, left, right));
            }
            if next.is_empty() {
                next.push(zeros[h + 1]);
            }
            levels.push(next);
        }
        // Keep the zero hashes at the tail of each level for path lookups.
        let tree = CredentialTree {
            depth,
            levels,
            entries,
        };
        Ok(tree.with_zero_padding(&zeros))
    }

    fn with_zero_padding(mut self, zeros: &[Fr]) -> Self {
        for (h, level) in self.levels.iter_mut().enumerate().take(self.depth) {
            if level.len() % 2 == 1 {
                level.push(zeros[h]);
            }
        }
        self
    }

    pub fn root(&self) -> Fr {
        self.levels[self.depth][0]
    }

    pub fn credentials(&self) -> Vec<Credential> {
        let root = fr_to_hex(&self.root());
        self.entries
            .iter()
            .enumerate()
            .map(|(i, (subject, expiry))| {
                let mut idx = i;
                let mut path = Vec::with_capacity(self.depth);
                for h in 0..self.depth {
                    path.push(fr_to_hex(&self.levels[h][idx ^ 1]));
                    idx >>= 1;
                }
                Credential {
                    subject: subject.clone(),
                    expiry: *expiry,
                    index: i as u64,
                    path,
                    root: root.clone(),
                }
            })
            .collect()
    }
}

/// Recompute the root a credential's path leads to.
pub fn credential_root(
    config: &PoseidonConfig<Fr>,
    cred: &Credential,
) -> Result<Fr, CredentialError> {
    let subject = subject_field(&cred.subject)
        .ok_or_else(|| CredentialError::BadSubject(cred.subject.clone()))?;
    let mut acc = leaf_hash(config, subject, cred.expiry);
    let mut idx = cred.index;
    for sibling in &cred.path {
        let s = fr_from_hex(sibling).ok_or_else(|| CredentialError::BadHex(sibling.clone()))?;
        acc = if idx & 1 == 0 {
            poseidon2(config, acc, s)
        } else {
            poseidon2(config, s, acc)
        };
        idx >>= 1;
    }
    Ok(acc)
}

/// Public inputs of one aggregated proof, plus the private witnesses.
#[derive(Debug, Clone)]
pub struct Accumulator {
    pub root: Fr,
    pub valid_until: u32,
    pub subjects: Vec<String>,
    pub subject_fields: Vec<Fr>,
    pub credentials: Vec<Credential>,
    pub slots: usize,
}

impl Accumulator {
    /// Check each credential individually and fold the batch.
    ///
    /// `valid_until` defaults to `min(expiry) - 1`, the latest ledger for
    /// which every credential in the batch is still unexpired.
    pub fn build(
        config: &PoseidonConfig<Fr>,
        root: Fr,
        depth: usize,
        slots: usize,
        credentials: Vec<Credential>,
        valid_until: Option<u32>,
    ) -> Result<Self, CredentialError> {
        if credentials.is_empty() {
            return Err(CredentialError::Empty);
        }
        if credentials.len() > slots {
            return Err(CredentialError::TooMany(credentials.len(), slots));
        }
        let min_expiry = credentials.iter().map(|c| c.expiry).min().unwrap_or(0);
        let valid_until = valid_until.unwrap_or(min_expiry.saturating_sub(1));

        let mut seen = BTreeSet::new();
        let mut subject_fields = Vec::with_capacity(credentials.len());
        for c in &credentials {
            if c.path.len() != depth {
                return Err(CredentialError::WrongDepth(
                    c.subject.clone(),
                    c.path.len(),
                    depth,
                ));
            }
            let s = subject_field(&c.subject)
                .ok_or_else(|| CredentialError::BadSubject(c.subject.clone()))?;
            if !seen.insert(fr_to_hex(&s)) {
                return Err(CredentialError::Duplicate(c.subject.clone()));
            }
            if credential_root(config, c)? != root {
                return Err(CredentialError::BadPath(c.subject.clone()));
            }
            if c.expiry <= valid_until {
                return Err(CredentialError::Expired(
                    c.subject.clone(),
                    c.expiry,
                    valid_until,
                ));
            }
            subject_fields.push(s);
        }
        Ok(Accumulator {
            root,
            valid_until,
            subjects: credentials.iter().map(|c| c.subject.clone()).collect(),
            subject_fields,
            credentials,
            slots,
        })
    }

    /// The proof's single public input; see [`commitment`].
    pub fn commitment(&self) -> Fr {
        commitment(
            &self.root,
            self.valid_until,
            self.subject_fields.len() as u32,
            &self.subject_fields,
            self.slots,
        )
    }

    pub fn public_inputs(&self) -> Vec<Fr> {
        vec![self.commitment()]
    }
}

/// `SHA-256(root ‖ valid_until ‖ count ‖ s_0 ‖ … ‖ s_{slots-1})` with the top
/// byte cleared, as a scalar. Every field is big-endian (scalars 32 bytes,
/// `valid_until`/`count` 4 bytes) and unused slots are 32 zero bytes. The
/// contract computes exactly this from its call arguments.
pub fn commitment(root: &Fr, valid_until: u32, count: u32, subjects: &[Fr], slots: usize) -> Fr {
    assert!(subjects.len() <= slots, "more subjects than slots");
    let mut h = Sha256::new();
    h.update(fr_to_bytes(root));
    h.update(valid_until.to_be_bytes());
    h.update(count.to_be_bytes());
    for s in subjects {
        h.update(fr_to_bytes(s));
    }
    for _ in subjects.len()..slots {
        h.update([0u8; 32]);
    }
    let mut digest: [u8; 32] = h.finalize().into();
    digest[0] = 0;
    Fr::from_be_bytes_mod_order(&digest)
}
