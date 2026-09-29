//! Holder concentration and transaction velocity analytics (issue #160).
//!
//! Backs `GET /v1/assets/:id/analytics` with three regulatory metrics:
//!
//! * **Gini coefficient** of holder balances sorted ascending,
//!   `G = Σ(2i − n − 1)·xᵢ / (n·Σxᵢ)` with 1-indexed `i`. `0` is perfect
//!   equality; the maximum for `n` holders is `(n − 1)/n`.
//! * **Herfindahl–Hirschman Index**, `Σsᵢ²` where `sᵢ` is each holder's
//!   percentage share of the held supply, on the 0–10 000 scale used by the
//!   U.S. DOJ/FTC merger guidelines.
//! * **24-hour transaction velocity**, `24h transfer volume / total supply`.
//!
//! # Complexity
//!
//! * Concentration metrics are computed per request from the snapshot's
//!   holder list in O(n log n) time (linear when the list is already ordered,
//!   which the indexer guarantees) and O(n) space for `n` holders.
//! * Velocity keeps, per token contract, a sliding window of
//!   `(ledger close time, amount)` pairs plus a running volume sum. Each
//!   transfer is inserted once (amortised O(1) for in-order ledgers,
//!   O(log k + k) for an out-of-order backfill) and evicted once when it
//!   leaves the window, so reading the volume is amortised O(1). Space is
//!   O(k) for the `k` transfers inside the window.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::models::{Asset, Event, Holder};

/// Length of the velocity window in seconds.
pub const WINDOW_SECS: i64 = 24 * 60 * 60;

/// Response body of `GET /v1/assets/:id/analytics`.
#[derive(Debug, Clone, Serialize)]
pub struct AssetAnalytics {
    pub asset_id: u64,
    /// Holders with a positive balance, i.e. the population of the
    /// concentration metrics.
    pub holder_count: usize,
    /// `null` when the asset has no holders.
    pub gini_coefficient: Option<f64>,
    /// 0–10 000; `null` when the asset has no holders.
    pub herfindahl_hirschman_index: Option<f64>,
    /// Transfer volume inside the window, in base units (raw i128, as a string).
    pub volume_24h: String,
    /// Total supply in base units (raw i128, as a string).
    pub total_supply: String,
    /// `volume_24h / total_supply`; `null` when the total supply is zero.
    pub velocity_24h: Option<f64>,
    /// RFC 3339 bounds of the velocity window `(window_start, window_end]`.
    pub window_start: String,
    pub window_end: String,
}

impl AssetAnalytics {
    pub fn compute(
        asset: &Asset,
        holders: &[Holder],
        velocity: &VelocityTracker,
        now: DateTime<Utc>,
    ) -> Self {
        let mut balances: Vec<i128> = holders
            .iter()
            .filter_map(|h| h.balance.parse::<i128>().ok())
            .filter(|&balance| balance > 0)
            .collect();
        balances.sort_unstable();

        let total_supply = asset.total_supply.parse::<i128>().unwrap_or(0);
        let volume = velocity.volume_24h(&asset.token_contract, now);

        AssetAnalytics {
            asset_id: asset.id,
            holder_count: balances.len(),
            gini_coefficient: gini(&balances),
            herfindahl_hirschman_index: hhi(&balances),
            volume_24h: volume.to_string(),
            total_supply: total_supply.to_string(),
            velocity_24h: (total_supply > 0).then(|| volume as f64 / total_supply as f64),
            window_start: (now - chrono::Duration::seconds(WINDOW_SECS)).to_rfc3339(),
            window_end: now.to_rfc3339(),
        }
    }
}

/// Gini coefficient of positive balances sorted ascending.
///
/// Evaluated exactly in `i128`: `|2i − n − 1| < n`, so every partial sum of
/// the numerator is bounded in magnitude by the denominator `n·Σx`, and one
/// overflow check on the denominator covers the whole computation. Only
/// holdings beyond `i128::MAX / n` fall back to `f64`.
fn gini(sorted: &[i128]) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len() as i128;
    let exact_denominator = sorted
        .iter()
        .try_fold(0i128, |sum, &x| sum.checked_add(x))
        .and_then(|total| total.checked_mul(n));

    Some(match exact_denominator {
        Some(denominator) => {
            let numerator: i128 = sorted
                .iter()
                .zip(1i128..)
                .map(|(&x, i)| (2 * i - n - 1) * x)
                .sum();
            numerator as f64 / denominator as f64
        }
        None => {
            let n = n as f64;
            let (numerator, total) =
                sorted
                    .iter()
                    .zip(1u64..)
                    .fold((0.0, 0.0), |(numerator, total), (&x, i)| {
                        let x = x as f64;
                        (numerator + (2.0 * i as f64 - n - 1.0) * x, total + x)
                    });
            numerator / (n * total)
        }
    })
}

/// Herfindahl–Hirschman Index of positive balances as percentage shares of
/// their sum (0–10 000).
fn hhi(balances: &[i128]) -> Option<f64> {
    let total: f64 = balances.iter().map(|&x| x as f64).sum();
    (total > 0.0).then(|| {
        balances
            .iter()
            .map(|&x| {
                let share = 100.0 * x as f64 / total;
                share * share
            })
            .sum()
    })
}

/// Sliding 24-hour transfer-volume windows, one per token contract.
#[derive(Debug, Default)]
pub struct VelocityTracker {
    windows: Mutex<HashMap<String, TransferWindow>>,
}

#[derive(Debug, Default)]
struct TransferWindow {
    /// `(ledger close unix seconds, amount)`, ordered by time.
    transfers: VecDeque<(i64, i128)>,
    /// Running sum of the amounts in `transfers`.
    volume: i128,
}

impl TransferWindow {
    fn insert(&mut self, at: i64, amount: i128) {
        // Ledgers arrive in order, so this is the back of the deque except
        // when a replay backfills older events.
        let index = self.transfers.partition_point(|&(t, _)| t <= at);
        self.transfers.insert(index, (at, amount));
        self.volume = self.volume.saturating_add(amount);
    }

    /// Drop transfers at or before `cutoff`; the window is `(cutoff, now]`.
    fn evict(&mut self, cutoff: i64) {
        while let Some(&(at, amount)) = self.transfers.front() {
            if at > cutoff {
                break;
            }
            self.volume = self.volume.saturating_sub(amount);
            self.transfers.pop_front();
        }
    }
}

impl VelocityTracker {
    /// Record newly indexed events. Each event must be passed once: the
    /// indexer feeds only events that were absent from the previous snapshot.
    pub fn observe(&self, events: &[Event], now: DateTime<Utc>) {
        let cutoff = now.timestamp() - WINDOW_SECS;
        let mut windows = self.windows.lock().unwrap_or_else(PoisonError::into_inner);
        for (contract, at, amount) in events.iter().filter_map(transfer) {
            if at > cutoff {
                let window = windows.entry(contract.to_owned()).or_default();
                window.insert(at, amount);
                window.evict(cutoff);
            }
        }
    }

    /// Transfer volume of `contract` in the window ending at `now`.
    pub fn volume_24h(&self, contract: &str, now: DateTime<Utc>) -> i128 {
        let mut windows = self.windows.lock().unwrap_or_else(PoisonError::into_inner);
        windows.get_mut(contract).map_or(0, |window| {
            window.evict(now.timestamp() - WINDOW_SECS);
            window.volume
        })
    }
}

/// `(token contract, ledger close unix seconds, amount)` of a transfer event.
///
/// SEP-41 amounts are `i128`s, which the indexer decodes to decimal strings.
/// Since CAP-67, a transfer that carries muxed destination information has
/// map data `{ amount, to_muxed_id }` instead of a bare amount, so both forms
/// are accepted. Events without a ledger close time cannot be placed in the
/// window and are skipped.
fn transfer(event: &Event) -> Option<(&str, i64, i128)> {
    if event.event_type != "transfer" {
        return None;
    }
    let amount = match event.data.get("amount")? {
        serde_json::Value::Object(fields) => fields.get("amount")?,
        amount => amount,
    }
    .as_str()?
    .parse::<i128>()
    .ok()
    .filter(|&amount| amount >= 0)?;
    let at = DateTime::parse_from_rfc3339(event.timestamp.as_deref()?)
        .ok()?
        .timestamp();
    Some((&event.contract, at, amount))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOKEN: &str = "CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3";

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn transfer_event(id: u64, secs: i64, amount: serde_json::Value) -> Event {
        Event {
            id,
            contract: TOKEN.to_string(),
            event_type: "transfer".to_string(),
            ledger: id as u32,
            timestamp: Some(at(secs).to_rfc3339()),
            data: json!({ "from": "GA", "to": "GB", "amount": amount }),
        }
    }

    fn asset(total_supply: &str) -> Asset {
        Asset {
            id: 1,
            token_contract: TOKEN.to_string(),
            issuer: "GISSUER".to_string(),
            name: "Asset".to_string(),
            symbol: "AST".to_string(),
            asset_type: "real_estate".to_string(),
            description: String::new(),
            valuation_cents: "0".to_string(),
            valuation_usd: 0.0,
            decimals: 7,
            total_supply: total_supply.to_string(),
            holders: 0,
            active: true,
            paused: false,
            compliance_contract: "CCOMPLIANCE".to_string(),
            created_at_ledger: 0,
            indexed_at_ledger: 0,
            index_error: None,
        }
    }

    fn holders(balances: &[&str]) -> Vec<Holder> {
        balances
            .iter()
            .map(|balance| Holder {
                address: format!("G{balance}"),
                balance: balance.to_string(),
                share_percent: 0.0,
            })
            .collect()
    }

    #[test]
    fn gini_matches_closed_form() {
        // Σ(2i − n − 1)·xᵢ = −3·1 − 1·2 + 1·3 + 3·4 = 10; n·Σx = 40.
        assert_eq!(gini(&[1, 2, 3, 4]), Some(0.25));
        assert_eq!(gini(&[5, 5, 5, 5]), Some(0.0));
        assert_eq!(gini(&[42]), Some(0.0));
        // One holder owning everything among n = 4 reaches (n − 1)/n.
        assert!((gini(&[1, 1, 1, 1_000_000_000_000]).unwrap() - 0.75).abs() < 1e-9);
        assert_eq!(gini(&[]), None);
    }

    #[test]
    fn gini_falls_back_to_f64_when_i128_would_overflow() {
        let big = i128::MAX / 2;
        assert_eq!(gini(&[big, big, big]), Some(0.0));
        let g = gini(&[1, big, big]).unwrap();
        assert!((g - 1.0 / 3.0).abs() < 1e-9, "got {g}");
    }

    #[test]
    fn hhi_uses_percentage_shares() {
        // DOJ example: shares of 30, 30, 20 and 20 percent give 2 600.
        assert_eq!(hhi(&[20, 20, 30, 30]), Some(2_600.0));
        assert_eq!(hhi(&[7]), Some(10_000.0));
        assert_eq!(hhi(&[]), None);
    }

    #[test]
    fn velocity_window_slides_and_evicts() {
        let tracker = VelocityTracker::default();
        let now = 1_000_000;
        tracker.observe(
            &[
                transfer_event(1, now - WINDOW_SECS, json!("999")), // on the boundary: excluded
                transfer_event(2, now - 3_600, json!("100")),
                transfer_event(3, now - 60, json!("50")),
            ],
            at(now),
        );
        assert_eq!(tracker.volume_24h(TOKEN, at(now)), 150);

        // An hour later the first in-window transfer has slid out.
        assert_eq!(tracker.volume_24h(TOKEN, at(now + WINDOW_SECS - 3_600)), 50);
        assert_eq!(tracker.volume_24h("CUNKNOWN", at(now)), 0);
    }

    #[test]
    fn velocity_accepts_out_of_order_and_muxed_transfers() {
        let tracker = VelocityTracker::default();
        let now = 2_000_000;
        tracker.observe(&[transfer_event(2, now - 10, json!("30"))], at(now));
        tracker.observe(
            &[
                // Backfilled older transfer, CAP-67 muxed map data.
                transfer_event(1, now - 20, json!({ "amount": "20", "to_muxed_id": 7 })),
                Event {
                    event_type: "mint".to_string(),
                    ..transfer_event(3, now - 5, json!("1000"))
                },
                Event {
                    timestamp: None,
                    ..transfer_event(4, now - 5, json!("1000"))
                },
            ],
            at(now),
        );
        assert_eq!(tracker.volume_24h(TOKEN, at(now)), 50);
        // Eviction order follows ledger time, not arrival order.
        assert_eq!(tracker.volume_24h(TOKEN, at(now - 15 + WINDOW_SECS)), 30);
    }

    #[test]
    fn compute_reports_all_metrics() {
        let tracker = VelocityTracker::default();
        let now = 3_000_000;
        tracker.observe(&[transfer_event(1, now - 100, json!("250"))], at(now));

        let analytics = AssetAnalytics::compute(
            &asset("1000"),
            &holders(&["400", "300", "200", "100", "0"]),
            &tracker,
            at(now),
        );

        assert_eq!(analytics.holder_count, 4);
        assert_eq!(analytics.gini_coefficient, Some(0.25));
        assert_eq!(analytics.herfindahl_hirschman_index, Some(3_000.0));
        assert_eq!(analytics.volume_24h, "250");
        assert_eq!(analytics.velocity_24h, Some(0.25));
        assert_eq!(analytics.window_end, at(now).to_rfc3339());
    }

    #[test]
    fn compute_handles_empty_asset() {
        let analytics =
            AssetAnalytics::compute(&asset("0"), &[], &VelocityTracker::default(), at(0));
        assert_eq!(analytics.holder_count, 0);
        assert_eq!(analytics.gini_coefficient, None);
        assert_eq!(analytics.herfindahl_hirschman_index, None);
        assert_eq!(analytics.velocity_24h, None);
    }
}
