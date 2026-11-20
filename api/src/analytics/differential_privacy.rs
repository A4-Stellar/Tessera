//! (ε, δ)-differential privacy for aggregate analytics endpoints (issue #26).
//!
//! `GET /v1/stats` and `GET /v1/assets/:id/holders?view=aggregate` report
//! aggregates computed over every holder. Reported exactly, a sequence of
//! narrow, repeated queries can recover an individual holder's position by
//! difference (query, wait for one holder to exit, query again). This module
//! perturbs each aggregate with calibrated noise so that no response reveals
//! whether any single holder's data was included, and meters the cumulative
//! privacy loss per API key so that averaging many responses cannot cancel
//! the noise out.
//!
//! The formal analysis (unit of privacy, sensitivities, composition and the
//! meaning of the δ term) is published in
//! `docs/content/en/docs/privacy-analysis.mdx`.
//!
//! # Mechanism
//!
//! Every aggregate is released through the Laplace mechanism: the true value
//! plus a draw from `Lap(Δ/ε)`, where `Δ` is the aggregate's global
//! sensitivity (see [`Aggregate::sensitivity`]). That gives pure
//! `ε`-differential privacy per aggregate.
//!
//! Sums whose per-holder contribution is unbounded (TVL, transfer volume) are
//! made bounded by clipping each holder's contribution to a configured `C`
//! before summing, so `Δ = C`. The clip is applied to the data, not assumed.
//!
//! # Budget
//!
//! A query that releases `k` aggregates under an endpoint configured with
//! `(ε, δ)` splits `ε` evenly (`ε/k` per aggregate), so by basic sequential
//! composition the whole response costs exactly `(ε, δ)`. The endpoint's `δ`
//! is charged as configured; it is the operator's allowance for the
//! residual failure probability of the deployment's sensitivity bounds and is
//! accounted for so `(ε, δ)` composes the same way across endpoints.
//!
//! Spend is tracked per API key in Redis (in memory without Redis) over a
//! rolling refresh window. When a query would exceed the per-key limit the
//! API answers `429 Too Many Requests` with `Retry-After` set to the seconds
//! until the key's budget refreshes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::models::{Asset, Event, Holder};

// ─── Parameters ─────────────────────────────────────────────────────────────

/// Configurable privacy parameters for one query or one budgeted session.
///
/// `epsilon` must be finite and strictly positive: it is the privacy loss per
/// query; smaller is more private and noisier. `delta`, when set, must be in
/// `(0, 1)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrivacyParams {
    pub epsilon: f64,
    pub delta: Option<f64>,
}

impl PrivacyParams {
    pub fn new(epsilon: f64) -> Result<Self, PrivacyError> {
        Self::validate_epsilon(epsilon)?;
        Ok(Self {
            epsilon,
            delta: None,
        })
    }

    pub fn with_delta(epsilon: f64, delta: f64) -> Result<Self, PrivacyError> {
        Self::validate_epsilon(epsilon)?;
        if !(delta > 0.0 && delta < 1.0) {
            return Err(PrivacyError::InvalidDelta(delta));
        }
        Ok(Self {
            epsilon,
            delta: Some(delta),
        })
    }

    fn validate_epsilon(epsilon: f64) -> Result<(), PrivacyError> {
        if epsilon.is_nan() || epsilon <= 0.0 || !epsilon.is_finite() {
            return Err(PrivacyError::InvalidEpsilon(epsilon));
        }
        Ok(())
    }

    /// `δ` as a number; `0` for a pure-`ε` configuration.
    pub fn delta_or_zero(&self) -> f64 {
        self.delta.unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PrivacyError {
    InvalidEpsilon(f64),
    InvalidDelta(f64),
    InvalidSensitivity(f64),
    #[cfg_attr(not(test), expect(dead_code, reason = "raised by the Gaussian mechanism"))]
    MissingDelta,
    #[cfg_attr(not(test), expect(dead_code, reason = "raised by PrivacyBudget"))]
    BudgetExceeded { spent: f64, limit: f64 },
}

impl std::fmt::Display for PrivacyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrivacyError::InvalidEpsilon(e) => {
                write!(f, "epsilon must be finite and > 0, got {e}")
            }
            PrivacyError::InvalidDelta(d) => write!(f, "delta must be in (0, 1), got {d}"),
            PrivacyError::InvalidSensitivity(s) => {
                write!(f, "sensitivity must be finite and > 0, got {s}")
            }
            PrivacyError::MissingDelta => {
                write!(f, "the Gaussian mechanism requires delta to be set")
            }
            PrivacyError::BudgetExceeded { spent, limit } => write!(
                f,
                "privacy budget exceeded: {spent} epsilon spent against a {limit} limit"
            ),
        }
    }
}

impl std::error::Error for PrivacyError {}

// ─── Mechanisms ─────────────────────────────────────────────────────────────

/// Draws one sample from `Laplace(0, scale)` by inverse-CDF sampling:
/// `-scale · sign(u) · ln(1 − 2|u|)` for `u ~ Uniform(-1/2, 1/2)`.
fn sample_laplace(scale: f64, rng: &mut impl Rng) -> f64 {
    // The range excludes 0.5, so `u` never reaches the ln(0) singularity.
    let u: f64 = rng.random_range(-0.5..0.5);
    -scale * u.signum() * (1.0 - 2.0 * u.abs()).ln()
}

#[cfg_attr(not(test), expect(dead_code, reason = "kept for callers that need the Gaussian mechanism or an in-process budget"))]
/// Draws one sample from `Normal(0, std_dev)` via the Box–Muller transform.
fn sample_gaussian(std_dev: f64, rng: &mut impl Rng) -> f64 {
    // `u1` starts at f64::EPSILON to keep away from ln(0).
    let u1: f64 = rng.random_range(f64::EPSILON..1.0);
    let u2: f64 = rng.random_range(0.0..1.0);
    let radius = (-2.0 * u1.ln()).sqrt();
    std_dev * radius * (2.0 * std::f64::consts::PI * u2).cos()
}

fn validate_sensitivity(sensitivity: f64) -> Result<(), PrivacyError> {
    if sensitivity.is_nan() || sensitivity <= 0.0 || !sensitivity.is_finite() {
        return Err(PrivacyError::InvalidSensitivity(sensitivity));
    }
    Ok(())
}

/// Laplace mechanism: `value + Lap(sensitivity / ε)`. `params.delta` is not
/// used by the mechanism itself.
pub fn add_laplace_noise(
    value: f64,
    sensitivity: f64,
    params: &PrivacyParams,
    rng: &mut impl Rng,
) -> Result<f64, PrivacyError> {
    validate_sensitivity(sensitivity)?;
    let scale = sensitivity / params.epsilon;
    Ok(value + sample_laplace(scale, rng))
}

/// Gaussian mechanism: `value + N(0, σ)` with
/// `σ = Δ·√(2·ln(1.25/δ))/ε` (Dwork & Roth, Theorem 3.22). Returns
/// [`PrivacyError::MissingDelta`] if `params.delta` is `None`.
#[cfg_attr(not(test), expect(dead_code, reason = "kept for callers that need the Gaussian mechanism or an in-process budget"))]
pub fn add_gaussian_noise(
    value: f64,
    sensitivity: f64,
    params: &PrivacyParams,
    rng: &mut impl Rng,
) -> Result<f64, PrivacyError> {
    validate_sensitivity(sensitivity)?;
    let delta = params.delta.ok_or(PrivacyError::MissingDelta)?;
    let std_dev = sensitivity * (2.0 * (1.25_f64 / delta).ln()).sqrt() / params.epsilon;
    Ok(value + sample_gaussian(std_dev, rng))
}

/// Tracks cumulative privacy loss in-process using basic composition (`k`
/// queries at `ε` each cost `k·ε`).
#[cfg_attr(not(test), expect(dead_code, reason = "kept for callers that need the Gaussian mechanism or an in-process budget"))]
#[derive(Debug, Clone)]
pub struct PrivacyBudget {
    limit: f64,
    spent: f64,
}

#[cfg_attr(not(test), expect(dead_code, reason = "kept for callers that need the Gaussian mechanism or an in-process budget"))]
impl PrivacyBudget {
    pub fn new(limit: f64) -> Result<Self, PrivacyError> {
        if limit.is_nan() || limit <= 0.0 || !limit.is_finite() {
            return Err(PrivacyError::InvalidEpsilon(limit));
        }
        Ok(Self { limit, spent: 0.0 })
    }

    pub fn remaining(&self) -> f64 {
        self.limit - self.spent
    }

    /// Reserves `epsilon` against the budget. Fails, without mutating state,
    /// if that would exceed the limit.
    pub fn charge(&mut self, epsilon: f64) -> Result<(), PrivacyError> {
        let next = self.spent + epsilon;
        if next > self.limit {
            return Err(PrivacyError::BudgetExceeded {
                spent: next,
                limit: self.limit,
            });
        }
        self.spent = next;
        Ok(())
    }
}

// ─── Aggregates and sensitivity ─────────────────────────────────────────────

/// The aggregates released under differential privacy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Aggregate {
    /// Total value locked, in USD cents, as a sum of per-holder positions
    /// each clipped to [`SensitivityBounds::tvl_clip_cents`].
    TvlSum,
    /// Number of distinct holders.
    HolderCount,
    /// 24 h transfer volume in token base units, as a sum of per-sender
    /// volumes each clipped to [`SensitivityBounds::transfer_volume_clip`].
    TransferVolume,
}

impl Aggregate {
    /// Global L1 sensitivity: the most one holder's presence or absence can
    /// change the aggregate, given the clip bounds.
    pub fn sensitivity(self, bounds: &SensitivityBounds) -> f64 {
        match self {
            // Adding or removing one holder changes a distinct count by 1.
            Aggregate::HolderCount => 1.0,
            // Each holder contributes at most the clip to the clipped sum.
            Aggregate::TvlSum => bounds.tvl_clip_cents,
            Aggregate::TransferVolume => bounds.transfer_volume_clip,
        }
    }
}

/// Per-holder contribution bounds that make the sums' sensitivity finite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SensitivityBounds {
    /// Largest per-holder TVL contribution counted, in USD cents.
    pub tvl_clip_cents: f64,
    /// Largest per-sender 24 h transfer volume counted, in base units.
    pub transfer_volume_clip: f64,
}

/// Default TVL clip: $1,000,000 per holder.
pub const DEFAULT_TVL_CLIP_CENTS: f64 = 100_000_000.0;
/// Default transfer-volume clip: 10^13 base units (1,000,000 tokens at the
/// default 7 decimals) per sender per 24 h.
pub const DEFAULT_TRANSFER_VOLUME_CLIP: f64 = 1e13;

impl Default for SensitivityBounds {
    fn default() -> Self {
        Self {
            tvl_clip_cents: DEFAULT_TVL_CLIP_CENTS,
            transfer_volume_clip: DEFAULT_TRANSFER_VOLUME_CLIP,
        }
    }
}

/// TVL in USD cents over active assets, as the sum of each holder's position
/// (`valuation × share`) clipped to `clip`.
pub fn clipped_tvl_cents(
    assets: &[Asset],
    holders: &HashMap<u64, Vec<Holder>>,
    clip: f64,
) -> f64 {
    assets
        .iter()
        .filter(|asset| asset.active)
        .map(|asset| {
            let valuation = asset.valuation_cents.parse::<f64>().unwrap_or(0.0).max(0.0);
            holders
                .get(&asset.id)
                .map(|list| {
                    list.iter()
                        .map(|h| (valuation * h.share_percent.max(0.0) / 100.0).min(clip))
                        .sum::<f64>()
                })
                .unwrap_or(0.0)
        })
        .sum()
}

/// Number of distinct addresses holding any asset.
pub fn distinct_holders(holders: &HashMap<u64, Vec<Holder>>) -> usize {
    holders
        .values()
        .flatten()
        .map(|h| h.address.as_str())
        .collect::<HashSet<_>>()
        .len()
}

/// Transfer volume of `contract` in the 24 h before `now`, summing each
/// sender's volume clipped to `clip`.
pub fn clipped_transfer_volume_24h(
    events: &[Event],
    contract: &str,
    now: DateTime<Utc>,
    clip: f64,
) -> f64 {
    let cutoff = now.timestamp() - 24 * 60 * 60;
    let mut per_sender: BTreeMap<&str, f64> = BTreeMap::new();
    for event in events {
        if event.contract != contract || event.event_type != "transfer" {
            continue;
        }
        let Some(at) = event
            .timestamp
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp())
        else {
            continue;
        };
        if at <= cutoff || at > now.timestamp() {
            continue;
        }
        let amount = match event.data.get("amount") {
            Some(serde_json::Value::Object(fields)) => fields.get("amount"),
            other => other,
        }
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse::<i128>().ok())
        .filter(|&v| v >= 0);
        let Some(amount) = amount else { continue };
        let sender = event
            .data
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        *per_sender.entry(sender).or_default() += amount as f64;
    }
    per_sender.values().map(|v| v.min(clip)).sum()
}

// ─── Configuration ──────────────────────────────────────────────────────────

/// Endpoints that release differentially private aggregates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateEndpoint {
    /// `GET /v1/stats`
    Stats,
    /// `GET /v1/assets/:id/holders?view=aggregate`
    Holders,
}

impl PrivateEndpoint {
    pub fn name(self) -> &'static str {
        match self {
            PrivateEndpoint::Stats => "stats",
            PrivateEndpoint::Holders => "holders",
        }
    }
}

/// Deployment configuration, read from the environment by
/// [`PrivacyConfig::from_env`].
#[derive(Debug, Clone)]
pub struct PrivacyConfig {
    pub enabled: bool,
    pub stats: PrivacyParams,
    pub holders: PrivacyParams,
    pub bounds: SensitivityBounds,
    /// Per-key `ε` limit per refresh window.
    pub budget_epsilon: f64,
    /// Per-key `δ` limit per refresh window.
    pub budget_delta: f64,
    /// Length of the budget refresh window.
    pub budget_window: Duration,
    /// API keys that get their own budget. Any other caller (including a
    /// request with an unknown key) shares the `anonymous` budget, so a
    /// client cannot mint fresh budgets by inventing keys.
    pub api_keys: HashSet<String>,
}

pub const DEFAULT_STATS_EPSILON: f64 = 0.5;
pub const DEFAULT_HOLDERS_EPSILON: f64 = 0.5;
pub const DEFAULT_DELTA: f64 = 1e-6;
pub const DEFAULT_BUDGET_EPSILON: f64 = 10.0;
pub const DEFAULT_BUDGET_DELTA: f64 = 1e-4;
pub const DEFAULT_BUDGET_WINDOW_SECS: u64 = 24 * 60 * 60;
/// Budget id shared by every caller without a configured API key.
pub const ANONYMOUS: &str = "anonymous";

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stats: PrivacyParams::with_delta(DEFAULT_STATS_EPSILON, DEFAULT_DELTA)
                .expect("default parameters are valid"),
            holders: PrivacyParams::with_delta(DEFAULT_HOLDERS_EPSILON, DEFAULT_DELTA)
                .expect("default parameters are valid"),
            bounds: SensitivityBounds::default(),
            budget_epsilon: DEFAULT_BUDGET_EPSILON,
            budget_delta: DEFAULT_BUDGET_DELTA,
            budget_window: Duration::from_secs(DEFAULT_BUDGET_WINDOW_SECS),
            api_keys: HashSet::new(),
        }
    }
}

impl PrivacyConfig {
    /// Read the configuration:
    ///
    /// | Variable | Default |
    /// |---|---|
    /// | `RWA_DP_ENABLED` | `true` |
    /// | `RWA_DP_STATS_EPSILON` / `RWA_DP_STATS_DELTA` | `0.5` / `1e-6` |
    /// | `RWA_DP_HOLDERS_EPSILON` / `RWA_DP_HOLDERS_DELTA` | `0.5` / `1e-6` |
    /// | `RWA_DP_TVL_CLIP_CENTS` | `100000000` |
    /// | `RWA_DP_TRANSFER_VOLUME_CLIP` | `1e13` |
    /// | `RWA_DP_BUDGET_EPSILON` / `RWA_DP_BUDGET_DELTA` | `10` / `1e-4` |
    /// | `RWA_DP_BUDGET_WINDOW_SECS` | `86400` |
    /// | `RWA_DP_API_KEYS` (comma-separated) | keys of `RWA_API_KEY_TIERS` |
    ///
    /// A `*_DELTA` of `0` configures a pure-`ε` endpoint.
    pub fn from_env() -> Result<Self, PrivacyError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, PrivacyError> {
        let num = |name: &str, default: f64| {
            get(name)
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or(default)
        };
        let params = |eps: &str, delta: &str, default_eps: f64| {
            let epsilon = num(eps, default_eps);
            let delta = num(delta, DEFAULT_DELTA);
            if delta == 0.0 {
                PrivacyParams::new(epsilon)
            } else {
                PrivacyParams::with_delta(epsilon, delta)
            }
        };

        let enabled = get("RWA_DP_ENABLED")
            .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off"))
            .unwrap_or(true);
        let bounds = SensitivityBounds {
            tvl_clip_cents: num("RWA_DP_TVL_CLIP_CENTS", DEFAULT_TVL_CLIP_CENTS),
            transfer_volume_clip: num("RWA_DP_TRANSFER_VOLUME_CLIP", DEFAULT_TRANSFER_VOLUME_CLIP),
        };
        validate_sensitivity(bounds.tvl_clip_cents)?;
        validate_sensitivity(bounds.transfer_volume_clip)?;

        let budget_epsilon = num("RWA_DP_BUDGET_EPSILON", DEFAULT_BUDGET_EPSILON);
        PrivacyParams::validate_epsilon(budget_epsilon)?;
        let budget_delta = num("RWA_DP_BUDGET_DELTA", DEFAULT_BUDGET_DELTA);
        if !(0.0..1.0).contains(&budget_delta) {
            return Err(PrivacyError::InvalidDelta(budget_delta));
        }
        let window_secs = get("RWA_DP_BUDGET_WINDOW_SECS")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_BUDGET_WINDOW_SECS);

        let api_keys = match get("RWA_DP_API_KEYS") {
            Some(keys) => keys
                .split(',')
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(str::to_owned)
                .collect(),
            None => get("RWA_API_KEY_TIERS")
                .unwrap_or_default()
                .split(',')
                .filter_map(|entry| entry.split_once('=').map(|(k, _)| k.trim()))
                .filter(|k| !k.is_empty())
                .map(str::to_owned)
                .collect(),
        };

        Ok(Self {
            enabled,
            stats: params("RWA_DP_STATS_EPSILON", "RWA_DP_STATS_DELTA", DEFAULT_STATS_EPSILON)?,
            holders: params(
                "RWA_DP_HOLDERS_EPSILON",
                "RWA_DP_HOLDERS_DELTA",
                DEFAULT_HOLDERS_EPSILON,
            )?,
            bounds,
            budget_epsilon,
            budget_delta,
            budget_window: Duration::from_secs(window_secs),
            api_keys,
        })
    }

    pub fn params(&self, endpoint: PrivateEndpoint) -> PrivacyParams {
        match endpoint {
            PrivateEndpoint::Stats => self.stats,
            PrivateEndpoint::Holders => self.holders,
        }
    }
}

// ─── Budget store ───────────────────────────────────────────────────────────

/// Result of charging a query against a key's budget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BudgetDecision {
    Allowed {
        spent_epsilon: f64,
        /// Seconds until the key's budget refreshes.
        refresh_in_secs: u64,
    },
    Exhausted {
        spent_epsilon: f64,
        refresh_in_secs: u64,
    },
}

/// One key's consumption, as reported by `GET /v1/admin/privacy/budget`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BudgetUsage {
    /// Stable, non-reversible id of the API key (or `anonymous`).
    pub key_id: String,
    pub spent_epsilon: f64,
    pub spent_delta: f64,
    pub queries: u64,
    pub refresh_in_secs: u64,
}

#[async_trait]
pub trait BudgetStore: Send + Sync {
    /// Atomically add `(epsilon, delta)` to `key_id`'s spend unless that
    /// would exceed `(limit_epsilon, limit_delta)`. The first charge in a
    /// window starts its `window`-long refresh timer.
    async fn charge(
        &self,
        key_id: &str,
        epsilon: f64,
        delta: f64,
        limit_epsilon: f64,
        limit_delta: f64,
        window: Duration,
    ) -> Result<BudgetDecision, String>;

    /// Current consumption of every key with spend in its window.
    async fn usage(&self) -> Result<Vec<BudgetUsage>, String>;
}

/// Tolerance for float accumulation when comparing spend against a limit.
const SPEND_EPSILON: f64 = 1e-9;

#[derive(Debug)]
struct MemoryEntry {
    epsilon: f64,
    delta: f64,
    queries: u64,
    expires: Instant,
}

/// Single-node budget store, used when `RWA_DP_REDIS_URL` is not set.
#[derive(Debug, Default)]
pub struct MemoryBudgetStore {
    entries: Mutex<HashMap<String, MemoryEntry>>,
}

#[async_trait]
impl BudgetStore for MemoryBudgetStore {
    async fn charge(
        &self,
        key_id: &str,
        epsilon: f64,
        delta: f64,
        limit_epsilon: f64,
        limit_delta: f64,
        window: Duration,
    ) -> Result<BudgetDecision, String> {
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, entry| entry.expires > now);
        let entry = entries.entry(key_id.to_owned()).or_insert(MemoryEntry {
            epsilon: 0.0,
            delta: 0.0,
            queries: 0,
            expires: now + window,
        });
        let refresh_in_secs = entry.expires.saturating_duration_since(now).as_secs().max(1);
        let next_epsilon = entry.epsilon + epsilon;
        let next_delta = entry.delta + delta;
        if next_epsilon > limit_epsilon + SPEND_EPSILON || next_delta > limit_delta + SPEND_EPSILON
        {
            return Ok(BudgetDecision::Exhausted {
                spent_epsilon: entry.epsilon,
                refresh_in_secs,
            });
        }
        entry.epsilon = next_epsilon;
        entry.delta = next_delta;
        entry.queries += 1;
        Ok(BudgetDecision::Allowed {
            spent_epsilon: next_epsilon,
            refresh_in_secs,
        })
    }

    async fn usage(&self) -> Result<Vec<BudgetUsage>, String> {
        let now = Instant::now();
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut usage: Vec<BudgetUsage> = entries
            .iter()
            .filter(|(_, entry)| entry.expires > now)
            .map(|(key_id, entry)| BudgetUsage {
                key_id: key_id.clone(),
                spent_epsilon: entry.epsilon,
                spent_delta: entry.delta,
                queries: entry.queries,
                refresh_in_secs: entry.expires.saturating_duration_since(now).as_secs(),
            })
            .collect();
        usage.sort_by(|a, b| a.key_id.cmp(&b.key_id));
        Ok(usage)
    }
}

/// Redis-backed budget store shared by every API node.
///
/// Each key's spend is a hash `tessera:dp:budget:<key_id>` with fields
/// `epsilon`, `delta` and `queries`, expiring at the end of its refresh
/// window. The set `tessera:dp:budget-keys` indexes the hashes for the admin
/// endpoint; members whose hash has expired are pruned when listed.
pub struct RedisBudgetStore {
    client: redis::Client,
}

const REDIS_BUDGET_PREFIX: &str = "tessera:dp:budget:";
const REDIS_BUDGET_INDEX: &str = "tessera:dp:budget-keys";

/// Atomic check-and-charge. Returns `{allowed, spent_epsilon, spent_delta,
/// ttl_seconds}`; spend values are strings to keep float precision.
const CHARGE_SCRIPT: &str = r#"
local spent_e = tonumber(redis.call('HGET', KEYS[1], 'epsilon') or '0')
local spent_d = tonumber(redis.call('HGET', KEYS[1], 'delta') or '0')
local next_e = spent_e + tonumber(ARGV[1])
local next_d = spent_d + tonumber(ARGV[2])
local window = tonumber(ARGV[5])
local ttl = redis.call('TTL', KEYS[1])
if ttl < 0 then ttl = window end
if next_e > tonumber(ARGV[3]) + 1e-9 or next_d > tonumber(ARGV[4]) + 1e-9 then
    return {0, tostring(spent_e), tostring(spent_d), ttl}
end
redis.call('HSET', KEYS[1], 'epsilon', tostring(next_e), 'delta', tostring(next_d))
redis.call('HINCRBY', KEYS[1], 'queries', 1)
if redis.call('TTL', KEYS[1]) < 0 then
    redis.call('EXPIRE', KEYS[1], window)
end
redis.call('SADD', KEYS[2], ARGV[6])
return {1, tostring(next_e), tostring(next_d), ttl}
"#;

impl RedisBudgetStore {
    pub fn new(redis_url: &str) -> Result<Self, String> {
        let client = redis::Client::open(redis_url).map_err(|e| e.to_string())?;
        Ok(Self { client })
    }

    async fn connection(&self) -> Result<redis::aio::MultiplexedConnection, String> {
        self.client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| e.to_string())
    }
}

fn parse_spend(value: &str) -> f64 {
    value.parse().unwrap_or(0.0)
}

#[async_trait]
impl BudgetStore for RedisBudgetStore {
    async fn charge(
        &self,
        key_id: &str,
        epsilon: f64,
        delta: f64,
        limit_epsilon: f64,
        limit_delta: f64,
        window: Duration,
    ) -> Result<BudgetDecision, String> {
        let mut con = self.connection().await?;
        let (allowed, spent_epsilon, _spent_delta, ttl): (i64, String, String, i64) =
            redis::Script::new(CHARGE_SCRIPT)
                .key(format!("{REDIS_BUDGET_PREFIX}{key_id}"))
                .key(REDIS_BUDGET_INDEX)
                .arg(epsilon.to_string())
                .arg(delta.to_string())
                .arg(limit_epsilon.to_string())
                .arg(limit_delta.to_string())
                .arg(window.as_secs().max(1))
                .arg(key_id)
                .invoke_async(&mut con)
                .await
                .map_err(|e| e.to_string())?;
        let refresh_in_secs = ttl.max(1) as u64;
        let spent_epsilon = parse_spend(&spent_epsilon);
        Ok(if allowed == 1 {
            BudgetDecision::Allowed {
                spent_epsilon,
                refresh_in_secs,
            }
        } else {
            BudgetDecision::Exhausted {
                spent_epsilon,
                refresh_in_secs,
            }
        })
    }

    async fn usage(&self) -> Result<Vec<BudgetUsage>, String> {
        let mut con = self.connection().await?;
        let key_ids: Vec<String> = redis::cmd("SMEMBERS")
            .arg(REDIS_BUDGET_INDEX)
            .query_async(&mut con)
            .await
            .map_err(|e| e.to_string())?;
        let mut usage = Vec::with_capacity(key_ids.len());
        for key_id in key_ids {
            let key = format!("{REDIS_BUDGET_PREFIX}{key_id}");
            let fields: HashMap<String, String> = redis::cmd("HGETALL")
                .arg(&key)
                .query_async(&mut con)
                .await
                .map_err(|e| e.to_string())?;
            if fields.is_empty() {
                // The window expired: drop the stale index entry.
                let _: i64 = redis::cmd("SREM")
                    .arg(REDIS_BUDGET_INDEX)
                    .arg(&key_id)
                    .query_async(&mut con)
                    .await
                    .map_err(|e| e.to_string())?;
                continue;
            }
            let ttl: i64 = redis::cmd("TTL")
                .arg(&key)
                .query_async(&mut con)
                .await
                .map_err(|e| e.to_string())?;
            usage.push(BudgetUsage {
                key_id,
                spent_epsilon: fields.get("epsilon").map_or(0.0, |v| parse_spend(v)),
                spent_delta: fields.get("delta").map_or(0.0, |v| parse_spend(v)),
                queries: fields
                    .get("queries")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                refresh_in_secs: ttl.max(0) as u64,
            });
        }
        usage.sort_by(|a, b| a.key_id.cmp(&b.key_id));
        Ok(usage)
    }
}

// ─── Engine ─────────────────────────────────────────────────────────────────

/// Why a private query was not answered.
#[derive(Debug, Clone, PartialEq)]
pub enum PrivacyRejection {
    /// The caller's budget is spent; retry after `retry_after_secs`.
    BudgetExhausted {
        spent_epsilon: f64,
        limit_epsilon: f64,
        retry_after_secs: u64,
    },
    /// The budget store could not be reached. Fails closed: answering
    /// without charging would let a store outage bypass the budget.
    StoreUnavailable(String),
}

/// A charged query: the noise parameters to use for each aggregate and the
/// headers describing the spend.
#[derive(Debug, Clone, PartialEq)]
pub struct PrivateQuery {
    pub params: PrivacyParams,
    /// `ε` per released aggregate (`params.epsilon / aggregates`).
    pub per_aggregate_epsilon: f64,
    pub spent_epsilon: f64,
    pub limit_epsilon: f64,
    pub refresh_in_secs: u64,
    bounds: SensitivityBounds,
}

impl PrivateQuery {
    /// Release `value` for `aggregate` under the Laplace mechanism, then
    /// clamp to `>= 0` (post-processing, so the guarantee is unchanged).
    pub fn release(&self, aggregate: Aggregate, value: f64, rng: &mut impl Rng) -> f64 {
        let params = PrivacyParams {
            epsilon: self.per_aggregate_epsilon,
            delta: None,
        };
        add_laplace_noise(value, aggregate.sensitivity(&self.bounds), &params, rng)
            .map(|noisy| noisy.max(0.0))
            .unwrap_or(0.0)
    }

    /// Release a count, rounded to an integer.
    pub fn release_count(&self, aggregate: Aggregate, value: usize, rng: &mut impl Rng) -> usize {
        self.release(aggregate, value as f64, rng).round() as usize
    }

    pub fn remaining_epsilon(&self) -> f64 {
        (self.limit_epsilon - self.spent_epsilon).max(0.0)
    }
}

/// Stable, non-reversible budget id for an API key, so raw keys never reach
/// Redis or the admin endpoint.
pub fn key_id_for(api_key: &str) -> String {
    let digest = Sha256::digest(api_key.as_bytes());
    format!("key_{}", &hex::encode(digest)[..16])
}

pub struct PrivacyEngine {
    pub config: PrivacyConfig,
    store: Box<dyn BudgetStore>,
}

impl PrivacyEngine {
    pub fn new(config: PrivacyConfig, store: Box<dyn BudgetStore>) -> Self {
        Self { config, store }
    }

    /// Build from the environment; Redis-backed when `RWA_DP_REDIS_URL` (or,
    /// failing that, `RWA_RATE_LIMIT_REDIS_URL`) is set.
    pub fn from_env() -> Result<Self, String> {
        let config = PrivacyConfig::from_env().map_err(|e| e.to_string())?;
        let redis_url = std::env::var("RWA_DP_REDIS_URL")
            .or_else(|_| std::env::var("RWA_RATE_LIMIT_REDIS_URL"))
            .ok();
        let store: Box<dyn BudgetStore> = match redis_url {
            Some(url) => Box::new(RedisBudgetStore::new(&url)?),
            None => Box::<MemoryBudgetStore>::default(),
        };
        Ok(Self::new(config, store))
    }

    /// An engine that releases exact values and charges nothing.
    pub fn disabled() -> Self {
        Self::new(
            PrivacyConfig {
                enabled: false,
                ..PrivacyConfig::default()
            },
            Box::<MemoryBudgetStore>::default(),
        )
    }

    /// Budget id for the caller: a configured API key gets its own budget;
    /// everyone else shares [`ANONYMOUS`].
    pub fn key_id(&self, api_key: Option<&str>) -> String {
        match api_key {
            Some(key) if self.config.api_keys.contains(key) => key_id_for(key),
            _ => ANONYMOUS.to_owned(),
        }
    }

    /// Charge one query that releases `aggregates` values. `Ok(None)` when
    /// differential privacy is disabled.
    pub async fn begin(
        &self,
        endpoint: PrivateEndpoint,
        api_key: Option<&str>,
        aggregates: usize,
    ) -> Result<Option<PrivateQuery>, PrivacyRejection> {
        if !self.config.enabled {
            return Ok(None);
        }
        let params = self.config.params(endpoint);
        let key_id = self.key_id(api_key);
        let decision = self
            .store
            .charge(
                &key_id,
                params.epsilon,
                params.delta_or_zero(),
                self.config.budget_epsilon,
                self.config.budget_delta,
                self.config.budget_window,
            )
            .await
            .map_err(PrivacyRejection::StoreUnavailable)?;
        match decision {
            BudgetDecision::Allowed {
                spent_epsilon,
                refresh_in_secs,
                ..
            } => {
                metrics::counter!("tessera_privacy_queries_total", "endpoint" => endpoint.name(), "outcome" => "allowed").increment(1);
                Ok(Some(PrivateQuery {
                    params,
                    per_aggregate_epsilon: params.epsilon / aggregates.max(1) as f64,
                    spent_epsilon,
                    limit_epsilon: self.config.budget_epsilon,
                    refresh_in_secs,
                    bounds: self.config.bounds,
                }))
            }
            BudgetDecision::Exhausted {
                spent_epsilon,
                refresh_in_secs,
                ..
            } => {
                metrics::counter!("tessera_privacy_queries_total", "endpoint" => endpoint.name(), "outcome" => "exhausted").increment(1);
                Err(PrivacyRejection::BudgetExhausted {
                    spent_epsilon,
                    limit_epsilon: self.config.budget_epsilon,
                    retry_after_secs: refresh_in_secs,
                })
            }
        }
    }

    pub async fn usage(&self) -> Result<Vec<BudgetUsage>, String> {
        self.store.usage().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use serde_json::json;

    fn rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    #[test]
    fn rejects_non_positive_epsilon() {
        assert!(PrivacyParams::new(0.0).is_err());
        assert!(PrivacyParams::new(-1.0).is_err());
        assert!(PrivacyParams::new(f64::NAN).is_err());
    }

    #[test]
    fn rejects_delta_outside_open_unit_interval() {
        assert!(PrivacyParams::with_delta(1.0, 0.0).is_err());
        assert!(PrivacyParams::with_delta(1.0, 1.0).is_err());
        assert!(PrivacyParams::with_delta(1.0, 1.5).is_err());
        assert!(PrivacyParams::with_delta(1.0, 0.5).is_ok());
    }

    #[test]
    fn gaussian_mechanism_requires_delta() {
        let params = PrivacyParams::new(1.0).unwrap();
        let mut r = rng();
        assert_eq!(
            add_gaussian_noise(100.0, 1.0, &params, &mut r),
            Err(PrivacyError::MissingDelta)
        );
    }

    #[test]
    fn laplace_noise_has_zero_mean_and_scale_matches_theory() {
        // Lap(0, b) has mean 0 and variance 2b².
        let params = PrivacyParams::new(0.5).unwrap();
        let sensitivity = 1.0;
        let scale = sensitivity / params.epsilon;
        let mut r = rng();

        let n = 200_000;
        let (mut sum, mut sum_sq) = (0.0, 0.0);
        for _ in 0..n {
            let noisy = add_laplace_noise(0.0, sensitivity, &params, &mut r).unwrap();
            sum += noisy;
            sum_sq += noisy * noisy;
        }
        let mean = sum / n as f64;
        let variance = sum_sq / n as f64 - mean * mean;
        let expected_variance = 2.0 * scale * scale;

        assert!(mean.abs() < 0.05, "mean {mean} too far from 0");
        assert!(
            (variance - expected_variance).abs() / expected_variance < 0.1,
            "variance {variance} too far from expected {expected_variance}"
        );
    }

    #[test]
    fn gaussian_noise_std_dev_matches_theory() {
        let params = PrivacyParams::with_delta(0.5, 1e-5).unwrap();
        let expected = (2.0 * (1.25_f64 / 1e-5).ln()).sqrt() / params.epsilon;
        let mut r = rng();
        let n = 200_000;
        let (mut sum, mut sum_sq) = (0.0, 0.0);
        for _ in 0..n {
            let noisy = add_gaussian_noise(0.0, 1.0, &params, &mut r).unwrap();
            sum += noisy;
            sum_sq += noisy * noisy;
        }
        let mean = sum / n as f64;
        let std_dev = (sum_sq / n as f64 - mean * mean).sqrt();
        assert!((std_dev - expected).abs() / expected < 0.1);
    }

    #[test]
    fn smaller_epsilon_means_more_noise() {
        let tight = PrivacyParams::new(0.1).unwrap();
        let loose = PrivacyParams::new(10.0).unwrap();
        let mut r = rng();
        let mut abs_mean = |params: &PrivacyParams| {
            (0..50_000)
                .map(|_| add_laplace_noise(0.0, 1.0, params, &mut r).unwrap().abs())
                .sum::<f64>()
                / 50_000.0
        };
        let tight_noise = abs_mean(&tight);
        let loose_noise = abs_mean(&loose);
        assert!(tight_noise > loose_noise);
    }

    #[test]
    fn rejects_non_positive_sensitivity() {
        let params = PrivacyParams::new(1.0).unwrap();
        let mut r = rng();
        assert!(add_laplace_noise(10.0, 0.0, &params, &mut r).is_err());
        assert!(add_laplace_noise(10.0, -1.0, &params, &mut r).is_err());
    }

    #[test]
    fn privacy_budget_tracks_composition_and_rejects_overspend() {
        let mut budget = PrivacyBudget::new(1.0).unwrap();
        budget.charge(0.4).unwrap();
        budget.charge(0.4).unwrap();
        assert!((budget.remaining() - 0.2).abs() < 1e-9);
        assert!(matches!(
            budget.charge(0.4),
            Err(PrivacyError::BudgetExceeded { .. })
        ));
        assert!((budget.remaining() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn sensitivities_follow_the_clip_bounds() {
        let bounds = SensitivityBounds {
            tvl_clip_cents: 500.0,
            transfer_volume_clip: 70.0,
        };
        assert_eq!(Aggregate::HolderCount.sensitivity(&bounds), 1.0);
        assert_eq!(Aggregate::TvlSum.sensitivity(&bounds), 500.0);
        assert_eq!(Aggregate::TransferVolume.sensitivity(&bounds), 70.0);
    }

    fn asset(id: u64, valuation_cents: &str, contract: &str) -> Asset {
        Asset {
            id,
            token_contract: contract.to_string(),
            issuer: "GISSUER".to_string(),
            name: "Asset".to_string(),
            symbol: "AST".to_string(),
            asset_type: "real_estate".to_string(),
            description: String::new(),
            valuation_cents: valuation_cents.to_string(),
            valuation_usd: 0.0,
            decimals: 7,
            total_supply: "0".to_string(),
            holders: 0,
            active: true,
            paused: false,
            compliance_contract: "CCOMP".to_string(),
            created_at_ledger: 0,
            indexed_at_ledger: 0,
            index_error: None,
        }
    }

    fn holder(address: &str, share: f64) -> Holder {
        Holder {
            address: address.to_string(),
            balance: "1".to_string(),
            share_percent: share,
        }
    }

    #[test]
    fn tvl_clips_each_holder_position() {
        // Valuation $10,000 split 90/10. With a $1,000 clip the whale's
        // $9,000 position counts as $1,000 and the minnow's $1,000 in full.
        let assets = vec![asset(1, "1000000", "C1")];
        let holders = HashMap::from([(1, vec![holder("GA", 90.0), holder("GB", 10.0)])]);
        assert_eq!(clipped_tvl_cents(&assets, &holders, 100_000.0), 200_000.0);
        // With no clip in effect the full valuation is recovered.
        assert_eq!(clipped_tvl_cents(&assets, &holders, f64::MAX), 1_000_000.0);
    }

    #[test]
    fn removing_one_holder_moves_each_aggregate_by_at_most_its_sensitivity() {
        let bounds = SensitivityBounds {
            tvl_clip_cents: 100_000.0,
            transfer_volume_clip: 50.0,
        };
        let assets = vec![asset(1, "1000000", "C1")];
        let with = HashMap::from([(1, vec![holder("GA", 90.0), holder("GB", 10.0)])]);
        let without = HashMap::from([(1, vec![holder("GB", 10.0)])]);

        let tvl_diff = clipped_tvl_cents(&assets, &with, bounds.tvl_clip_cents)
            - clipped_tvl_cents(&assets, &without, bounds.tvl_clip_cents);
        assert!(tvl_diff.abs() <= Aggregate::TvlSum.sensitivity(&bounds));
        let count_diff = distinct_holders(&with) as f64 - distinct_holders(&without) as f64;
        assert!(count_diff.abs() <= Aggregate::HolderCount.sensitivity(&bounds));

        let now = DateTime::from_timestamp(100_000, 0).unwrap();
        let ts = DateTime::from_timestamp(99_000, 0).unwrap().to_rfc3339();
        let transfer = |id, from: &str, amount: &str| Event {
            id,
            contract: "C1".to_string(),
            event_type: "transfer".to_string(),
            ledger: id as u32,
            timestamp: Some(ts.clone()),
            data: json!({ "from": from, "to": "GZ", "amount": amount }),
        };
        let all = vec![
            transfer(1, "GA", "400"),
            transfer(2, "GA", "400"),
            transfer(3, "GB", "30"),
        ];
        let without_ga: Vec<Event> = all
            .iter()
            .filter(|e| e.data["from"] != "GA")
            .cloned()
            .collect();
        let v_all = clipped_transfer_volume_24h(&all, "C1", now, bounds.transfer_volume_clip);
        let v_without = clipped_transfer_volume_24h(&without_ga, "C1", now, bounds.transfer_volume_clip);
        assert_eq!(v_all, 80.0, "GA clipped to 50, GB counted in full");
        assert!((v_all - v_without).abs() <= Aggregate::TransferVolume.sensitivity(&bounds));
    }

    #[test]
    fn config_reads_per_endpoint_parameters() {
        let env = HashMap::from([
            ("RWA_DP_STATS_EPSILON", "0.25"),
            ("RWA_DP_STATS_DELTA", "0"),
            ("RWA_DP_HOLDERS_EPSILON", "2"),
            ("RWA_DP_HOLDERS_DELTA", "1e-7"),
            ("RWA_DP_BUDGET_EPSILON", "3"),
            ("RWA_DP_BUDGET_WINDOW_SECS", "60"),
            ("RWA_API_KEY_TIERS", "alpha=10:1,beta=20:2"),
        ]);
        let config =
            PrivacyConfig::from_lookup(|k| env.get(k).map(|v| v.to_string())).unwrap();
        assert!(config.enabled);
        assert_eq!(config.stats, PrivacyParams::new(0.25).unwrap());
        assert_eq!(config.holders, PrivacyParams::with_delta(2.0, 1e-7).unwrap());
        assert_eq!(config.budget_epsilon, 3.0);
        assert_eq!(config.budget_window, Duration::from_secs(60));
        assert!(config.api_keys.contains("alpha") && config.api_keys.contains("beta"));

        let bad = HashMap::from([("RWA_DP_STATS_EPSILON", "-1")]);
        assert!(PrivacyConfig::from_lookup(|k| bad.get(k).map(|v| v.to_string())).is_err());
    }

    fn engine(budget_epsilon: f64) -> PrivacyEngine {
        PrivacyEngine::new(
            PrivacyConfig {
                stats: PrivacyParams::with_delta(1.0, 1e-6).unwrap(),
                budget_epsilon,
                budget_window: Duration::from_secs(3600),
                api_keys: HashSet::from(["known".to_string()]),
                ..PrivacyConfig::default()
            },
            Box::<MemoryBudgetStore>::default(),
        )
    }

    #[tokio::test]
    async fn each_query_consumes_budget_until_exhausted() {
        let engine = engine(2.5);
        for expected in [1.0, 2.0] {
            let query = engine
                .begin(PrivateEndpoint::Stats, Some("known"), 2)
                .await
                .unwrap()
                .expect("enabled");
            assert!((query.spent_epsilon - expected).abs() < 1e-9);
            assert!((query.per_aggregate_epsilon - 0.5).abs() < 1e-9);
        }
        match engine.begin(PrivateEndpoint::Stats, Some("known"), 2).await {
            Err(PrivacyRejection::BudgetExhausted {
                retry_after_secs, ..
            }) => assert!(retry_after_secs > 0 && retry_after_secs <= 3600),
            other => panic!("expected exhaustion, got {other:?}"),
        }
        // A different (anonymous) caller still has its own budget.
        assert!(engine
            .begin(PrivateEndpoint::Stats, None, 2)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn unknown_api_keys_share_the_anonymous_budget() {
        let engine = engine(1.0);
        engine
            .begin(PrivateEndpoint::Stats, Some("made-up-1"), 1)
            .await
            .unwrap();
        // Inventing another key must not reset the budget.
        assert!(matches!(
            engine.begin(PrivateEndpoint::Stats, Some("made-up-2"), 1).await,
            Err(PrivacyRejection::BudgetExhausted { .. })
        ));
        let usage = engine.usage().await.unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].key_id, ANONYMOUS);
        assert_eq!(usage[0].queries, 1);
    }

    #[tokio::test]
    async fn disabled_engine_releases_without_charging() {
        let engine = PrivacyEngine::disabled();
        assert_eq!(
            engine.begin(PrivateEndpoint::Stats, None, 3).await.unwrap(),
            None
        );
        assert!(engine.usage().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn released_values_are_noisy_but_unbiased() {
        let engine = engine(f64::MAX / 2.0);
        let query = engine
            .begin(PrivateEndpoint::Stats, None, 1)
            .await
            .unwrap()
            .unwrap();
        let mut r = rng();
        let n = 20_000;
        let mut sum = 0.0;
        let mut exact = 0;
        for _ in 0..n {
            let v = query.release(Aggregate::HolderCount, 1_000.0, &mut r);
            if v == 1_000.0 {
                exact += 1;
            }
            sum += v;
        }
        assert!(exact < n / 100, "noise must actually be applied");
        assert!((sum / n as f64 - 1_000.0).abs() < 0.1);
    }

    #[test]
    fn key_ids_do_not_contain_the_raw_key() {
        let id = key_id_for("super-secret");
        assert!(id.starts_with("key_") && !id.contains("super-secret"));
        assert_eq!(id, key_id_for("super-secret"));
    }
}
