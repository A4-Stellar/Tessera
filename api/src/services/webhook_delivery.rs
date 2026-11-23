//! Reliable webhook delivery primitives (issue #458).
//!
//! Signing, retry scheduling, delivery headers and the auto-disable rule.
//! Persistence (`webhook_deliveries` with `FOR UPDATE SKIP LOCKED`) and the
//! Vault-held per-tenant key are supplied by the caller through
//! [`DeliveryStore`] and [`SigningKeyProvider`].

use std::time::Duration;

use sha2::{Digest, Sha256};

/// Consecutive failures after which an endpoint is disabled.
pub const AUTO_DISABLE_FAILURES: u32 = 100;
/// Upper bound on the delay between delivery attempts.
pub const MAX_RETRY_DELAY: Duration = Duration::from_secs(24 * 60 * 60);
const BASE_RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    Pending,
    Delivered,
    Failed,
}

/// One row of `webhook_deliveries`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub id: String,
    pub endpoint_id: String,
    pub event_type: String,
    pub body: Vec<u8>,
    pub status: DeliveryStatus,
    pub attempts: u32,
}

/// Source of the per-tenant signing key (Vault in production).
pub trait SigningKeyProvider: Send + Sync {
    fn key_for(&self, tenant_id: &str) -> Option<Vec<u8>>;
}

/// Storage contract for the delivery worker.
pub trait DeliveryStore: Send + Sync {
    /// Claim up to `limit` pending deliveries (in Postgres: `FOR UPDATE SKIP LOCKED`).
    fn claim_pending(&mut self, limit: usize) -> Vec<Delivery>;
    fn mark(&mut self, id: &str, status: DeliveryStatus);
}

/// Exponential back-off: 30 s, 60 s, 120 s, ... capped at 24 h.
pub fn retry_delay(attempt: u32) -> Duration {
    let factor = 1u64 << attempt.min(20);
    let secs = BASE_RETRY_DELAY.as_secs().saturating_mul(factor);
    Duration::from_secs(secs).min(MAX_RETRY_DELAY)
}

pub fn should_auto_disable(consecutive_failures: u32) -> bool {
    consecutive_failures >= AUTO_DISABLE_FAILURES
}

/// HMAC-SHA256 (RFC 2104) over `message`.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

/// Value of the `X-Tessera-Signature` header: `sha256=<hex>`.
pub fn signature_header(key: &[u8], body: &[u8]) -> String {
    format!("sha256={}", hex::encode(hmac_sha256(key, body)))
}

/// Headers sent with every delivery attempt.
pub fn delivery_headers(event_type: &str, delivery_id: &str, signature: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Content-Type", "application/json".to_string()),
        ("X-Tessera-Signature", signature.to_string()),
        ("X-Tessera-Event-Type", event_type.to_string()),
        ("X-Tessera-Delivery-ID", delivery_id.to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_test_case_1() {
        let key = [0x0bu8; 20];
        let mac = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            hex::encode(mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn retry_delay_doubles_and_caps_at_24_hours() {
        assert_eq!(retry_delay(0), Duration::from_secs(30));
        assert_eq!(retry_delay(1), Duration::from_secs(60));
        assert_eq!(retry_delay(2), Duration::from_secs(120));
        assert_eq!(retry_delay(40), MAX_RETRY_DELAY);
    }

    #[test]
    fn auto_disable_at_one_hundred_consecutive_failures() {
        assert!(!should_auto_disable(99));
        assert!(should_auto_disable(100));
    }

    #[test]
    fn signature_header_has_sha256_prefix_and_headers_are_complete() {
        let sig = signature_header(b"secret", b"{}");
        assert!(sig.starts_with("sha256=") && sig.len() == "sha256=".len() + 64);
        let headers = delivery_headers("transfer", "d-1", &sig);
        let names: Vec<_> = headers.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec!["Content-Type", "X-Tessera-Signature", "X-Tessera-Event-Type", "X-Tessera-Delivery-ID"]
        );
    }
}
