//! TOML configuration for the orchestrator binary.
//!
//! Secrets are never read from the file: each node names the environment
//! variable holding its connection string, and DNS credentials come from the
//! provider's standard environment variables.

use std::time::Duration;

use serde::Deserialize;

use crate::failover::FailoverPolicy;
use crate::types::Node;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestratorConfig {
    #[serde(default)]
    pub policy: PolicyConfig,
    pub primary: NodeConfig,
    pub replicas: Vec<NodeConfig>,
    pub dns: DnsConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub name: String,
    pub region: String,
    pub endpoint: String,
    /// Environment variable holding the `postgres://` URL for this node.
    pub dsn_env: String,
}

impl NodeConfig {
    pub fn node(&self) -> Node {
        Node {
            name: self.name.clone(),
            region: self.region.clone(),
            endpoint: self.endpoint.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase", deny_unknown_fields)]
pub enum DnsConfig {
    /// Credentials: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`.
    Route53 {
        hosted_zone_id: String,
        record_name: String,
        #[serde(default = "default_ttl")]
        ttl: u32,
    },
    /// Credentials: `CLOUDFLARE_API_TOKEN` (Zone.DNS edit).
    Cloudflare {
        zone_id: String,
        record_id: String,
        record_name: String,
        #[serde(default = "default_ttl")]
        ttl: u32,
    },
}

fn default_ttl() -> u32 {
    30
}

/// Millisecond-based mirror of [`FailoverPolicy`]; every field is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    pub heartbeat_interval_ms: Option<u64>,
    pub probe_timeout_ms: Option<u64>,
    pub failure_threshold: Option<u32>,
    pub promotion_deadline_ms: Option<u64>,
    pub max_data_loss_bytes: Option<u64>,
    pub require_synchronous_candidate: Option<bool>,
    pub require_witness_quorum: Option<bool>,
    pub upstream_silence_threshold_ms: Option<u64>,
    pub catchup_poll_interval_ms: Option<u64>,
    pub fence_timeout_ms: Option<u64>,
    pub lag_alert_bytes: Option<u64>,
    pub dns_attempts: Option<u32>,
    pub dns_timeout_ms: Option<u64>,
}

impl PolicyConfig {
    pub fn to_policy(&self) -> FailoverPolicy {
        let d = FailoverPolicy::default();
        let ms =
            |v: Option<u64>, default: Duration| v.map(Duration::from_millis).unwrap_or(default);
        FailoverPolicy {
            heartbeat_interval: ms(self.heartbeat_interval_ms, d.heartbeat_interval),
            probe_timeout: ms(self.probe_timeout_ms, d.probe_timeout),
            failure_threshold: self.failure_threshold.unwrap_or(d.failure_threshold),
            promotion_deadline: ms(self.promotion_deadline_ms, d.promotion_deadline),
            max_data_loss_bytes: self.max_data_loss_bytes.unwrap_or(d.max_data_loss_bytes),
            require_synchronous_candidate: self
                .require_synchronous_candidate
                .unwrap_or(d.require_synchronous_candidate),
            require_witness_quorum: self
                .require_witness_quorum
                .unwrap_or(d.require_witness_quorum),
            upstream_silence_threshold: ms(
                self.upstream_silence_threshold_ms,
                d.upstream_silence_threshold,
            ),
            catchup_poll_interval: ms(self.catchup_poll_interval_ms, d.catchup_poll_interval),
            fence_timeout: ms(self.fence_timeout_ms, d.fence_timeout),
            lag_alert_bytes: self.lag_alert_bytes.unwrap_or(d.lag_alert_bytes),
            dns_attempts: self.dns_attempts.unwrap_or(d.dns_attempts),
            dns_timeout: ms(self.dns_timeout_ms, d.dns_timeout),
        }
    }
}

impl OrchestratorConfig {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        config.to_policy_checked()?;
        if config.replicas.is_empty() {
            return Err("at least one replica is required".into());
        }
        let mut names: Vec<&str> = std::iter::once(&config.primary)
            .chain(&config.replicas)
            .map(|n| n.name.as_str())
            .collect();
        names.sort_unstable();
        if names.windows(2).any(|w| w[0] == w[1]) {
            return Err("node names must be unique".into());
        }
        Ok(config)
    }

    pub fn to_policy_checked(&self) -> Result<FailoverPolicy, String> {
        let policy = self.policy.to_policy();
        policy.validate()?;
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../orchestrator.example.toml");

    #[test]
    fn example_config_parses() {
        let config = OrchestratorConfig::from_toml(EXAMPLE).unwrap();
        assert_eq!(config.primary.region, "us-east-1");
        assert_eq!(config.replicas[0].region, "eu-west-1");
        assert!(matches!(config.dns, DnsConfig::Route53 { ttl: 30, .. }));
        let policy = config.to_policy_checked().unwrap();
        assert_eq!(policy.promotion_deadline, Duration::from_secs(30));
        assert_eq!(policy.max_data_loss_bytes, 0);
    }

    #[test]
    fn rejects_duplicate_names_and_bad_policy() {
        let dup = EXAMPLE.replace("name = \"tessera-euw1\"", "name = \"tessera-use1\"");
        assert!(OrchestratorConfig::from_toml(&dup)
            .unwrap_err()
            .contains("unique"));
        let bad = EXAMPLE.replace("failure_threshold = 3", "failure_threshold = 0");
        assert!(OrchestratorConfig::from_toml(&bad).is_err());
    }
}
