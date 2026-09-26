//! Real-time fraud & anomaly detection (issue #101).
//!
//! The detector is fed [`Observation`]s (a transfer of some volume, or an
//! allowlist/compliance change) keyed by account. For every account and metric
//! it keeps a sliding window of fixed-width time buckets and, on each new
//! observation, compares the *current* bucket against the mean and standard
//! deviation of the *preceding* buckets in the window (the baseline):
//!
//! ```text
//!     z = (current - mean(baseline)) / max(std(baseline), floor)
//! ```
//!
//! Three metrics are tracked: transfer `Volume` (sum of amounts per bucket),
//! transfer `Velocity` (count per bucket, which catches rapid micro-transfers)
//! and `AllowlistChanges` (count per bucket, which catches flash allowlist
//! churn). When `z > 3.5` ([`Z_THRESHOLD`]) a risk flag is raised: it is
//! stored as an *active flag* (served by `GET /v1/security/anomalies`),
//! logged, counted in Prometheus, and pushed to subscribers of
//! [`AnomalyDetector::subscribe`] as a real-time alert.
//!
//! Notes on the statistics:
//! * Empty buckets inside the window count as zeros, so a quiet account that
//!   suddenly becomes busy scores high.
//! * The standard deviation is floored (see [`DetectorConfig`]) so a perfectly
//!   flat baseline does not produce infinite scores from a single extra unit.
//! * No flag is raised until an account has at least
//!   `min_baseline_buckets` buckets of history, to avoid alerting on brand
//!   new accounts.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;

use serde::Serialize;
use tokio::sync::broadcast;

use crate::models::Event;

/// Z-score above which an observation is flagged (issue #101: Z > 3.5).
pub const Z_THRESHOLD: f64 = 3.5;

/// Tunables for the detector.
#[derive(Debug, Clone)]
pub struct DetectorConfig {
    /// Width of one time bucket, in seconds.
    pub bucket_secs: u64,
    /// Number of buckets in the sliding window (current bucket included).
    pub window_buckets: usize,
    /// Minimum number of baseline buckets required before scoring.
    pub min_baseline_buckets: usize,
    /// Flag threshold for the Z-score.
    pub z_threshold: f64,
    /// A flag not re-triggered for this long is no longer "active".
    pub flag_ttl_secs: u64,
    /// Lower bound on the standard deviation for counts (events per bucket).
    pub count_std_floor: f64,
    /// Lower bound on the standard deviation for volume, as a fraction of the
    /// baseline mean (with an absolute minimum of 1.0).
    pub volume_std_floor_ratio: f64,
    /// Upper bound on tracked accounts; stale accounts are pruned first.
    pub max_accounts: usize,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        DetectorConfig {
            bucket_secs: 60,
            window_buckets: 60,
            min_baseline_buckets: 5,
            z_threshold: Z_THRESHOLD,
            flag_ttl_secs: 3600,
            count_std_floor: 1.0,
            volume_std_floor_ratio: 0.1,
            max_accounts: 100_000,
        }
    }
}

/// What kind of activity an observation describes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ObservationKind {
    /// A token transfer of `amount` units.
    Transfer { amount: f64 },
    /// An allowlist / compliance status change for the account.
    AllowlistChange,
}

/// One unit of account activity at a unix timestamp (seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub account: String,
    pub timestamp: u64,
    pub kind: ObservationKind,
}

/// The statistic a flag was raised on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Volume,
    Velocity,
    AllowlistChanges,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AnomalyAlert {
    pub account: String,
    pub metric: Metric,
    pub z_score: f64,
    pub observed: f64,
    pub baseline_mean: f64,
    pub baseline_std: f64,
    pub timestamp: u64,
}

/// An active risk flag as exposed to maintainers.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RiskFlag {
    pub account: String,
    pub metric: Metric,
    /// Highest Z-score seen while the flag has been active.
    pub peak_z_score: f64,
    /// Z-score of the most recent trigger.
    pub latest_z_score: f64,
    pub observed: f64,
    pub baseline_mean: f64,
    pub baseline_std: f64,
    pub first_seen: u64,
    pub last_seen: u64,
    pub trigger_count: u64,
}

#[derive(Debug, Default)]
struct Bucket {
    volume: f64,
    transfers: f64,
    allowlist: f64,
}

#[derive(Debug, Default)]
struct AccountSeries {
    first_bucket: u64,
    last_bucket: u64,
    buckets: VecDeque<(u64, Bucket)>,
}

#[derive(Default)]
struct Inner {
    accounts: HashMap<String, AccountSeries>,
    flags: BTreeMap<(String, Metric), RiskFlag>,
}

pub struct AnomalyDetector {
    cfg: DetectorConfig,
    inner: Mutex<Inner>,
    tx: broadcast::Sender<AnomalyAlert>,
}

impl Default for AnomalyDetector {
    fn default() -> Self {
        Self::new(DetectorConfig::default())
    }
}

fn mean_std(values: &[f64]) -> (f64, f64) {
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    (mean, var.sqrt())
}

impl AnomalyDetector {
    pub fn new(cfg: DetectorConfig) -> Self {
        let (tx, _) = broadcast::channel(256);
        AnomalyDetector {
            cfg,
            inner: Mutex::new(Inner::default()),
            tx,
        }
    }

    /// Subscribe to real-time alerts. Slow receivers lag (and skip) rather
    /// than blocking the indexer.
    pub fn subscribe(&self) -> broadcast::Receiver<AnomalyAlert> {
        self.tx.subscribe()
    }

    /// Record an observation and return any alerts it triggered.
    pub fn observe(&self, obs: &Observation) -> Vec<AnomalyAlert> {
        let cfg = &self.cfg;
        let window = cfg.window_buckets.max(2) as u64;
        let bucket_idx = obs.timestamp / cfg.bucket_secs.max(1);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        if inner.accounts.len() >= cfg.max_accounts && !inner.accounts.contains_key(&obs.account) {
            let horizon = bucket_idx.saturating_sub(window);
            inner.accounts.retain(|_, s| s.last_bucket >= horizon);
            if inner.accounts.len() >= cfg.max_accounts {
                // Still full of live accounts: drop the observation rather
                // than grow without bound.
                return Vec::new();
            }
        }

        let series = inner
            .accounts
            .entry(obs.account.clone())
            .or_insert_with(|| AccountSeries {
                first_bucket: bucket_idx,
                last_bucket: bucket_idx,
                buckets: VecDeque::new(),
            });
        // Late/out-of-order observations older than the window are ignored.
        if series.last_bucket > bucket_idx && series.last_bucket - bucket_idx >= window {
            return Vec::new();
        }
        series.first_bucket = series.first_bucket.min(bucket_idx);
        series.last_bucket = series.last_bucket.max(bucket_idx);
        let oldest = series.last_bucket.saturating_sub(window - 1);
        while series.buckets.front().is_some_and(|(i, _)| *i < oldest) {
            series.buckets.pop_front();
        }
        let pos = match series.buckets.binary_search_by_key(&bucket_idx, |(i, _)| *i) {
            Ok(p) => p,
            Err(p) => {
                series.buckets.insert(p, (bucket_idx, Bucket::default()));
                p
            }
        };
        {
            let b = &mut series.buckets[pos].1;
            match obs.kind {
                ObservationKind::Transfer { amount } => {
                    b.volume += amount.abs();
                    b.transfers += 1.0;
                }
                ObservationKind::AllowlistChange => b.allowlist += 1.0,
            }
        }

        // Dense baseline: buckets [bucket_idx - len, bucket_idx) with zero fill.
        let history = bucket_idx.saturating_sub(series.first_bucket);
        let baseline_len = history.min(window - 1) as usize;
        if baseline_len < cfg.min_baseline_buckets.max(1) {
            return Vec::new();
        }
        let start = bucket_idx - baseline_len as u64;
        let mut vol = vec![0.0; baseline_len];
        let mut cnt = vec![0.0; baseline_len];
        let mut alw = vec![0.0; baseline_len];
        for (i, b) in series.buckets.iter() {
            if *i >= start && *i < bucket_idx {
                let k = (*i - start) as usize;
                vol[k] = b.volume;
                cnt[k] = b.transfers;
                alw[k] = b.allowlist;
            }
        }
        let cur = &series.buckets[pos].1;
        let candidates = [
            (Metric::Volume, cur.volume, vol, true),
            (Metric::Velocity, cur.transfers, cnt, false),
            (Metric::AllowlistChanges, cur.allowlist, alw, false),
        ];

        let mut alerts = Vec::new();
        for (metric, observed, base, is_volume) in candidates {
            let touched = matches!(
                (metric, obs.kind),
                (Metric::AllowlistChanges, ObservationKind::AllowlistChange)
                    | (Metric::Volume | Metric::Velocity, ObservationKind::Transfer { .. })
            );
            if !touched {
                continue;
            }
            let (mean, std) = mean_std(&base);
            let floor = if is_volume {
                (mean * cfg.volume_std_floor_ratio).max(1.0)
            } else {
                cfg.count_std_floor
            };
            let z = (observed - mean) / std.max(floor);
            if z > cfg.z_threshold {
                alerts.push(AnomalyAlert {
                    account: obs.account.clone(),
                    metric,
                    z_score: z,
                    observed,
                    baseline_mean: mean,
                    baseline_std: std,
                    timestamp: obs.timestamp,
                });
            }
        }

        for a in &alerts {
            let flag = inner
                .flags
                .entry((a.account.clone(), a.metric))
                .and_modify(|f| {
                    if obs.timestamp.saturating_sub(f.last_seen) > cfg.flag_ttl_secs {
                        // Previous flag had expired: start a fresh one.
                        f.first_seen = obs.timestamp;
                        f.peak_z_score = a.z_score;
                        f.trigger_count = 0;
                    }
                })
                .or_insert_with(|| RiskFlag {
                    account: a.account.clone(),
                    metric: a.metric,
                    peak_z_score: a.z_score,
                    latest_z_score: a.z_score,
                    observed: a.observed,
                    baseline_mean: a.baseline_mean,
                    baseline_std: a.baseline_std,
                    first_seen: obs.timestamp,
                    last_seen: obs.timestamp,
                    trigger_count: 0,
                });
            flag.peak_z_score = flag.peak_z_score.max(a.z_score);
            flag.latest_z_score = a.z_score;
            flag.observed = a.observed;
            flag.baseline_mean = a.baseline_mean;
            flag.baseline_std = a.baseline_std;
            flag.last_seen = obs.timestamp;
            flag.trigger_count += 1;
        }
        drop(inner);

        for a in &alerts {
            tracing::warn!(
                account = %a.account, metric = ?a.metric, z = a.z_score,
                observed = a.observed, "anomaly detected: z-score above threshold"
            );
            metrics::counter!("rwa_anomaly_alerts_total", "metric" => format!("{:?}", a.metric))
                .increment(1);
            let _ = self.tx.send(a.clone());
        }
        alerts
    }

    /// Flags triggered within the TTL of `now`, highest peak Z-score first.
    pub fn active_flags(&self, now: u64) -> Vec<RiskFlag> {
        let ttl = self.cfg.flag_ttl_secs;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.flags.retain(|_, f| now.saturating_sub(f.last_seen) <= ttl);
        let mut v: Vec<RiskFlag> = inner.flags.values().cloned().collect();
        v.sort_by(|a, b| {
            b.peak_z_score
                .partial_cmp(&a.peak_z_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// Feed indexed contract events. Returns the number of events that mapped
    /// to an observation.
    pub fn observe_events(&self, events: &[Event]) -> usize {
        let mut used = 0;
        for e in events {
            if let Some(o) = observation_from_event(e) {
                used += 1;
                self.observe(&o);
            }
        }
        used
    }
}

/// Best-effort mapping of an indexed [`Event`] to an [`Observation`].
///
/// Transfers are recognised by `event_type == "transfer"` with a `from`
/// (or `account`/`address`) field and an `amount`; events whose type mentions
/// `allowlist`/`whitelist` become allowlist changes. Anything else is ignored.
pub fn observation_from_event(e: &Event) -> Option<Observation> {
    let ty = e.event_type.to_ascii_lowercase();
    let account = ["from", "account", "address", "holder"]
        .iter()
        .find_map(|n| e.data.get(*n))
        .and_then(|v| v.as_str().map(str::to_owned))?;
    let timestamp = e
        .timestamp
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.timestamp().max(0) as u64)
        // ~5s per ledger when no wall-clock timestamp is available.
        .unwrap_or(e.ledger as u64 * 5);
    let kind = if ty == "transfer" {
        let amount = e.data.get("amount").and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| v.as_f64())
        })?;
        ObservationKind::Transfer { amount }
    } else if ty.contains("allowlist") || ty.contains("whitelist") {
        ObservationKind::AllowlistChange
    } else {
        return None;
    };
    Some(Observation {
        account,
        timestamp,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer(acct: &str, ts: u64, amount: f64) -> Observation {
        Observation {
            account: acct.into(),
            timestamp: ts,
            kind: ObservationKind::Transfer { amount },
        }
    }

    /// Steady baseline: one transfer of ~100 (+-noise) per minute for 20 min.
    fn warm(d: &AnomalyDetector, acct: &str) {
        for m in 0..20u64 {
            let amt = 100.0 + (m % 3) as f64 * 5.0;
            assert!(d.observe(&transfer(acct, m * 60, amt)).is_empty(), "minute {m}");
        }
    }

    #[test]
    fn steady_activity_never_flags() {
        let d = AnomalyDetector::default();
        warm(&d, "GA");
        assert!(d.active_flags(20 * 60).is_empty());
    }

    #[test]
    fn volume_spike_flags_above_threshold() {
        let d = AnomalyDetector::default();
        warm(&d, "GA");
        let alerts = d.observe(&transfer("GA", 20 * 60, 100_000.0));
        let v = alerts
            .iter()
            .find(|a| a.metric == Metric::Volume)
            .expect("volume alert");
        assert!(v.z_score > Z_THRESHOLD, "z = {}", v.z_score);
        assert_eq!(d.active_flags(20 * 60 + 1)[0].account, "GA");
    }

    #[test]
    fn rapid_micro_transfers_flag_velocity_but_not_volume() {
        let d = AnomalyDetector::default();
        warm(&d, "GA");
        let mut velocity = None;
        for i in 0..30 {
            for a in d.observe(&transfer("GA", 20 * 60 + i, 0.01)) {
                if a.metric == Metric::Velocity {
                    velocity.get_or_insert(a);
                }
            }
        }
        let v = velocity.expect("velocity alert from micro-transfers");
        assert!(v.z_score > Z_THRESHOLD);
        assert!(d
            .active_flags(20 * 60 + 40)
            .iter()
            .all(|f| f.metric == Metric::Velocity));
    }

    #[test]
    fn flash_allowlist_changes_flag() {
        let d = AnomalyDetector::default();
        warm(&d, "GA");
        let mut hit = false;
        for i in 0..10 {
            let o = Observation {
                account: "GA".into(),
                timestamp: 20 * 60 + i,
                kind: ObservationKind::AllowlistChange,
            };
            hit |= d
                .observe(&o)
                .iter()
                .any(|a| a.metric == Metric::AllowlistChanges);
        }
        assert!(hit);
    }

    #[test]
    fn no_alert_without_enough_history() {
        let d = AnomalyDetector::default();
        for i in 0..200 {
            assert!(d.observe(&transfer("NEW", 10 + i, 1e9)).is_empty());
        }
    }

    #[test]
    fn accounts_are_scored_independently() {
        let d = AnomalyDetector::default();
        warm(&d, "GA");
        warm(&d, "GB");
        d.observe(&transfer("GA", 20 * 60, 1e6));
        let flags = d.active_flags(20 * 60);
        assert!(!flags.is_empty());
        assert!(flags.iter().all(|f| f.account == "GA"));
    }

    #[test]
    fn flags_expire_after_ttl() {
        let d = AnomalyDetector::default();
        warm(&d, "GA");
        d.observe(&transfer("GA", 20 * 60, 1e6));
        assert!(!d.active_flags(20 * 60 + 3600).is_empty());
        assert!(d.active_flags(20 * 60 + 3601).is_empty());
    }

    #[tokio::test]
    async fn alerts_are_broadcast_in_real_time() {
        let d = AnomalyDetector::default();
        let mut rx = d.subscribe();
        warm(&d, "GA");
        d.observe(&transfer("GA", 20 * 60, 1e6));
        let a = rx.try_recv().expect("alert delivered");
        assert_eq!(a.account, "GA");
        assert!(a.z_score > Z_THRESHOLD);
    }

    #[test]
    fn maps_events_to_observations() {
        let e = Event {
            id: 1,
            contract: "C".into(),
            event_type: "transfer".into(),
            ledger: 10,
            timestamp: None,
            data: serde_json::json!({"from": "GA", "amount": "42.5"}),
        };
        let o = observation_from_event(&e).unwrap();
        assert_eq!(o.timestamp, 50);
        assert_eq!(o.kind, ObservationKind::Transfer { amount: 42.5 });
        let other = Event {
            event_type: "mint".into(),
            ..e
        };
        assert!(observation_from_event(&other).is_none());
    }
}
