//! Redis cache key policy and stampede-control helpers.
//!
//! Runtime Redis calls stay in the existing middleware/services. This module
//! centralizes namespacing, TTL jitter, and lock-key derivation so every cached
//! endpoint uses the same stampede-resistant policy.

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    pub ttl_seconds: u64,
    pub stale_seconds: u64,
    pub lock_seconds: u64,
    pub jitter_percent: u8,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            ttl_seconds: 30,
            stale_seconds: 120,
            lock_seconds: 5,
            jitter_percent: 10,
        }
    }
}

impl CachePolicy {
    pub fn fresh_ttl_for_key(&self, key: &str) -> u64 {
        let jitter = self.ttl_seconds.saturating_mul(self.jitter_percent as u64) / 100;
        if jitter == 0 {
            return self.ttl_seconds;
        }
        let bucket = stable_hash(key) % (jitter.saturating_mul(2).saturating_add(1));
        self.ttl_seconds
            .saturating_sub(jitter)
            .saturating_add(bucket)
    }

    pub fn stale_ttl_for_key(&self, key: &str) -> u64 {
        self.fresh_ttl_for_key(key)
            .saturating_add(self.stale_seconds)
    }
}

pub fn response_cache_key(tenant_id: &str, method: &str, path_and_query: &str) -> String {
    format!(
        "cache:v1:{}:{}:{}",
        normalize_segment(tenant_id),
        method.to_ascii_uppercase(),
        digest(path_and_query)
    )
}

pub fn stampede_lock_key(cache_key: &str) -> String {
    format!("lock:{cache_key}")
}

fn normalize_segment(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    if normalized.is_empty() {
        "anonymous".to_string()
    } else {
        normalized
    }
}

fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn stable_hash(value: &str) -> u64 {
    let digest = Sha256::digest(value.as_bytes());
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("sha256 digest has at least 8 bytes"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_tenant_scoped_and_do_not_embed_query_values() {
        let key = response_cache_key("tenant/a", "get", "/assets?holder=GSECRET");
        assert!(key.starts_with("cache:v1:tenant-a:GET:"));
        assert!(!key.contains("GSECRET"));
        assert_eq!(stampede_lock_key(&key), format!("lock:{key}"));
    }

    #[test]
    fn ttl_jitter_is_deterministic_and_bounded() {
        let policy = CachePolicy {
            ttl_seconds: 100,
            stale_seconds: 50,
            lock_seconds: 5,
            jitter_percent: 10,
        };
        let ttl = policy.fresh_ttl_for_key("cache:v1:t:GET:x");
        assert!((90..=110).contains(&ttl));
        assert_eq!(ttl, policy.fresh_ttl_for_key("cache:v1:t:GET:x"));
        assert_eq!(policy.stale_ttl_for_key("cache:v1:t:GET:x"), ttl + 50);
    }
}
