//! Tenant API-key identity and rate-tier helpers.

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateTier {
    Free,
    Growth,
    Enterprise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierLimit {
    pub requests: u64,
    pub window_seconds: u64,
}

impl RateTier {
    pub fn limit(self) -> TierLimit {
        match self {
            RateTier::Free => TierLimit {
                requests: 60,
                window_seconds: 60,
            },
            RateTier::Growth => TierLimit {
                requests: 600,
                window_seconds: 60,
            },
            RateTier::Enterprise => TierLimit {
                requests: 6_000,
                window_seconds: 60,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantIdentity {
    pub tenant_id: String,
    pub key_hash: String,
    pub tier: RateTier,
}

impl TenantIdentity {
    pub fn from_api_key(api_key: &str, tier: RateTier) -> Option<Self> {
        let (tenant_id, secret) = api_key.split_once('.')?;
        if tenant_id.is_empty() || secret.len() < 24 {
            return None;
        }
        Some(Self {
            tenant_id: normalize_tenant_id(tenant_id),
            key_hash: hash_api_key(api_key),
            tier,
        })
    }

    pub fn rate_limit_key(&self, route_group: &str) -> String {
        format!(
            "tenant:{}:{}:{}",
            self.tenant_id,
            route_group.replace(':', "-"),
            self.key_hash
        )
    }
}

pub fn hash_api_key(api_key: &str) -> String {
    hex::encode(Sha256::digest(api_key.as_bytes()))
}

fn normalize_tenant_id(tenant_id: &str) -> String {
    tenant_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_malformed_api_keys() {
        assert!(TenantIdentity::from_api_key("missing-separator", RateTier::Free).is_none());
        assert!(TenantIdentity::from_api_key("tenant.short", RateTier::Free).is_none());
    }

    #[test]
    fn derives_stable_redacted_rate_limit_keys() {
        let tenant =
            TenantIdentity::from_api_key("Acme_Corp.abcdefghijklmnopqrstuvwxyz", RateTier::Growth)
                .unwrap();
        assert_eq!(tenant.tenant_id, "acme_corp");
        assert_eq!(
            tenant.tier.limit(),
            TierLimit {
                requests: 600,
                window_seconds: 60,
            }
        );
        let key = tenant.rate_limit_key("assets:list");
        assert!(key.starts_with("tenant:acme_corp:assets-list:"));
        assert!(!key.contains("abcdefghijklmnopqrstuvwxyz"));
    }
}
