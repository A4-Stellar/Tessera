//! Distributed Webhook Payload Signer & Key Rotation Service (Issue #232).
//!
//! Provides HMAC-SHA256 signing of outbound webhook payloads with automatic
//! 30-day key rotation.  During a configurable grace period after rotation,
//! **two** `X-Tessera-Signature` headers are emitted — one per key — so
//! consumers can verify against either while migrating.
//!
//! # Rotation lifecycle
//!
//! ```text
//!  ┌─────────────────────────────────────────────────────┐
//!  │ Day 0          Day 30 (rotation)     Day 31 (grace) │
//!  │   ▼                ▼                      ▼         │
//!  │ [key-A active] → [key-B active,      → [key-B only] │
//!  │                    key-A in grace]                    │
//!  └─────────────────────────────────────────────────────┘
//! ```
//!
//! Subscribers receive a `key.rotated` webhook event when a rotation occurs
//! and should update their verification logic within the grace period.

use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use sha2::Sha256;
use tracing::info;

// ── Constants ────────────────────────────────────────────────────────────────

const KEY_LENGTH: usize = 32;
const DEFAULT_ROTATION_DAYS: u64 = 30;
const DEFAULT_GRACE_PERIOD_HOURS: u64 = 24;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SigningKey {
    pub id: String,
    pub secret: Vec<u8>,
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
}

impl SigningKey {
    fn generate(rotation_days: u64) -> Self {
        use rand::Rng;
        let mut secret = vec![0u8; KEY_LENGTH];
        rand::rng().fill(&mut secret[..]);
        let now = SystemTime::now();
        Self {
            id: hex::encode(&secret[..8]),
            secret,
            created_at: now,
            expires_at: now + Duration::from_secs(rotation_days * 86400),
        }
    }

    fn is_expired(&self) -> bool {
        SystemTime::now() >= self.expires_at
    }
}

#[derive(Debug, Clone)]
pub struct SignatureHeader {
    pub key_id: String,
    pub signature: String,
}

impl SignatureHeader {
    pub fn to_header_value(&self) -> String {
        format!("t={},sha256={}", self.key_id, self.signature)
    }
}

// ── Signer state ─────────────────────────────────────────────────────────────

#[derive(Debug)]
struct SignerState {
    active_key: SigningKey,
    grace_key: Option<GraceKey>,
    rotation_days: u64,
    grace_period: Duration,
}

#[derive(Debug, Clone)]
struct GraceKey {
    key: SigningKey,
    retired_at: SystemTime,
    grace_until: SystemTime,
}

impl GraceKey {
    fn is_in_grace(&self) -> bool {
        SystemTime::now() < self.grace_until
    }
}

// ── WebhookSigner ────────────────────────────────────────────────────────────

/// Thread-safe webhook payload signer with automatic key rotation.
#[derive(Clone)]
pub struct WebhookSigner {
    state: Arc<RwLock<SignerState>>,
}

/// Event emitted on key rotation so callers can notify subscribers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KeyRotatedEvent {
    pub old_key_id: String,
    pub new_key_id: String,
    pub grace_period_hours: u64,
    pub rotated_at: String,
}

impl WebhookSigner {
    /// Create a new signer with default 30-day rotation and 24-hour grace period.
    pub fn new() -> Self {
        Self::with_config(DEFAULT_ROTATION_DAYS, DEFAULT_GRACE_PERIOD_HOURS)
    }

    /// Create a signer with custom rotation schedule.
    pub fn with_config(rotation_days: u64, grace_period_hours: u64) -> Self {
        let active_key = SigningKey::generate(rotation_days);
        info!(key_id = %active_key.id, "webhook signer initialised");
        Self {
            state: Arc::new(RwLock::new(SignerState {
                active_key,
                grace_key: None,
                rotation_days,
                grace_period: Duration::from_secs(grace_period_hours * 3600),
            })),
        }
    }

    /// Sign a payload, returning one or two signatures.
    ///
    /// During the grace period after a rotation, two signatures are returned:
    /// the active key's signature first, then the retiring key's.  Consumers
    /// should accept a request if *either* signature matches.
    pub fn sign(&self, payload: &[u8]) -> Vec<SignatureHeader> {
        self.maybe_rotate();

        let state = self.state.read().expect("signer lock poisoned");
        let mut headers = vec![Self::compute_signature(&state.active_key, payload)];

        if let Some(ref gk) = state.grace_key {
            if gk.is_in_grace() {
                headers.push(Self::compute_signature(&gk.key, payload));
            }
        }

        headers
    }

    /// The current active key ID (for subscription registration).
    pub fn active_key_id(&self) -> String {
        self.state.read().expect("signer lock poisoned").active_key.id.clone()
    }

    /// Whether the signer is currently in a grace period (dual-signing).
    pub fn in_grace_period(&self) -> bool {
        let state = self.state.read().expect("signer lock poisoned");
        state.grace_key.as_ref().map_or(false, |gk| gk.is_in_grace())
    }

    /// Force an immediate key rotation.  Returns a `KeyRotatedEvent` that
    /// should be dispatched to subscribers.
    pub fn force_rotate(&self) -> KeyRotatedEvent {
        let mut state = self.state.write().expect("signer lock poisoned");
        Self::rotate_inner(&mut state)
    }

    /// Verify that `payload` matches `signature` using either the active or
    /// grace key.
    pub fn verify(&self, payload: &[u8], signature: &str) -> bool {
        let state = self.state.read().expect("signer lock poisoned");

        let active_sig = Self::compute_signature(&state.active_key, payload);
        if constant_time_eq(active_sig.signature.as_bytes(), signature.as_bytes()) {
            return true;
        }

        if let Some(ref gk) = state.grace_key {
            if gk.is_in_grace() {
                let grace_sig = Self::compute_signature(&gk.key, payload);
                if constant_time_eq(grace_sig.signature.as_bytes(), signature.as_bytes()) {
                    return true;
                }
            }
        }

        false
    }

    // -- internals ---------------------------------------------------------

    fn maybe_rotate(&self) {
        let needs_rotation = {
            let state = self.state.read().expect("signer lock poisoned");
            state.active_key.is_expired()
        };
        if needs_rotation {
            let mut state = self.state.write().expect("signer lock poisoned");
            if state.active_key.is_expired() {
                Self::rotate_inner(&mut state);
            }
        }
    }

    fn rotate_inner(state: &mut SignerState) -> KeyRotatedEvent {
        let old_key = state.active_key.clone();
        let new_key = SigningKey::generate(state.rotation_days);
        let now = SystemTime::now();

        info!(
            old_key_id = %old_key.id,
            new_key_id = %new_key.id,
            "webhook signing key rotated"
        );

        let event = KeyRotatedEvent {
            old_key_id: old_key.id.clone(),
            new_key_id: new_key.id.clone(),
            grace_period_hours: state.grace_period.as_secs() / 3600,
            rotated_at: chrono::Utc::now().to_rfc3339(),
        };

        state.grace_key = Some(GraceKey {
            key: old_key,
            retired_at: now,
            grace_until: now + state.grace_period,
        });
        state.active_key = new_key;

        event
    }

    fn compute_signature(key: &SigningKey, payload: &[u8]) -> SignatureHeader {
        use sha2::Digest;

        // HMAC-SHA256 per RFC 2104.
        let block_size = 64;
        let mut k = key.secret.clone();
        if k.len() > block_size {
            let digest = Sha256::digest(&k);
            k = digest.to_vec();
        }
        k.resize(block_size, 0);

        let mut i_pad = vec![0x36u8; block_size];
        let mut o_pad = vec![0x5cu8; block_size];
        for i in 0..block_size {
            i_pad[i] ^= k[i];
            o_pad[i] ^= k[i];
        }

        let mut inner = Sha256::new();
        inner.update(&i_pad);
        inner.update(payload);
        let inner_hash = inner.finalize();

        let mut outer = Sha256::new();
        outer.update(&o_pad);
        outer.update(inner_hash);
        let mac = outer.finalize();

        SignatureHeader {
            key_id: key.id.clone(),
            signature: hex::encode(mac),
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_returns_single_header_without_grace() {
        let signer = WebhookSigner::new();
        let sigs = signer.sign(b"hello world");
        assert_eq!(sigs.len(), 1);
        assert!(!sigs[0].signature.is_empty());
        assert_eq!(sigs[0].signature.len(), 64);
    }

    #[test]
    fn sign_deterministic_for_same_payload() {
        let signer = WebhookSigner::new();
        let sig1 = signer.sign(b"test payload");
        let sig2 = signer.sign(b"test payload");
        assert_eq!(sig1[0].signature, sig2[0].signature);
    }

    #[test]
    fn sign_differs_for_different_payloads() {
        let signer = WebhookSigner::new();
        let sig1 = signer.sign(b"payload A");
        let sig2 = signer.sign(b"payload B");
        assert_ne!(sig1[0].signature, sig2[0].signature);
    }

    #[test]
    fn force_rotate_produces_dual_signatures() {
        let signer = WebhookSigner::new();
        let old_id = signer.active_key_id();

        let event = signer.force_rotate();
        assert_eq!(event.old_key_id, old_id);
        assert_ne!(event.new_key_id, old_id);

        let sigs = signer.sign(b"during grace");
        assert_eq!(sigs.len(), 2, "expected dual signatures during grace period");
        assert_ne!(sigs[0].key_id, sigs[1].key_id);
    }

    #[test]
    fn header_format() {
        let signer = WebhookSigner::new();
        let sigs = signer.sign(b"test");
        let header = sigs[0].to_header_value();
        assert!(header.starts_with("t="));
        assert!(header.contains(",sha256="));
    }

    #[test]
    fn verify_accepts_valid_signature() {
        let signer = WebhookSigner::new();
        let payload = b"verify me";
        let sigs = signer.sign(payload);
        assert!(signer.verify(payload, &sigs[0].signature));
    }

    #[test]
    fn verify_rejects_invalid_signature() {
        let signer = WebhookSigner::new();
        assert!(!signer.verify(b"data", "0000000000000000000000000000000000000000000000000000000000000000"));
    }

    #[test]
    fn verify_accepts_grace_key_during_grace_period() {
        let signer = WebhookSigner::new();
        let payload = b"grace test";

        let old_sigs = signer.sign(payload);
        let old_sig = old_sigs[0].signature.clone();

        signer.force_rotate();
        assert!(signer.in_grace_period());
        assert!(signer.verify(payload, &old_sig), "old key should still verify during grace");
    }

    #[test]
    fn key_rotation_event_fields() {
        let signer = WebhookSigner::new();
        let event = signer.force_rotate();
        assert!(!event.old_key_id.is_empty());
        assert!(!event.new_key_id.is_empty());
        assert_eq!(event.grace_period_hours, DEFAULT_GRACE_PERIOD_HOURS);
        assert!(!event.rotated_at.is_empty());
    }

    #[test]
    fn custom_config() {
        let signer = WebhookSigner::with_config(7, 48);
        let sigs = signer.sign(b"custom");
        assert_eq!(sigs.len(), 1);

        let event = signer.force_rotate();
        assert_eq!(event.grace_period_hours, 48);
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"ab", b"abc"));
    }
}
