//! Ledger-gap detection and auto-healing (issue #156).
//!
//! During a network outage, an RPC failover or a stalled node the indexer can
//! skip ledgers: refresh `N` reports head `1_000` and refresh `N+1` reports
//! head `1_040`, leaving `1_001..=1_039` unreconciled. This module watches the
//! head ledger reported by every refresh cycle, flags any missing sequence
//! range, and heals it in a detached Tokio task that pages the range back from
//! the configured archive/RPC nodes via `getLedgers`.
//!
//! Two properties matter for production:
//!
//! * **Real-time indexing never pauses.** [`GapHealer::observe_ledger`] does no
//!   I/O: it records the observation, emits metrics and *spawns* the backfill.
//!   The refresh loop continues immediately.
//! * **Reordered observations are not gaps.** RPC responses can arrive out of
//!   order and a node can report a slightly older `latestLedger`; only a head
//!   strictly greater than the last one seen can open a gap.
//!
//! Recovered ledgers are counted in Prometheus as
//! `tessera_healed_ledgers_total{source="rpc"}`.

use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;

use super::rpc_client::RpcClient;
use super::IndexError;

/// A contiguous range of missing ledger sequence numbers, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerGap {
    pub start: u32,
    pub end: u32,
}

impl LedgerGap {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Number of ledgers in the gap.
    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start).saturating_add(1)
    }

    /// Whether the range carries no ledgers (`end < start`).
    pub fn is_empty(&self) -> bool {
        self.end < self.start
    }
}

/// Tunables for the gap healer. `from_env` reads them from
/// `RWA_GAP_HEALER_PAGE_LIMIT`, `RWA_GAP_HEALER_MAX_CONCURRENT` and
/// `RWA_GAP_FULL_REBUILD_THRESHOLD`, so operators can widen the backfill
/// without a rebuild; the `Default` values are used when they are unset.
#[derive(Debug, Clone, Copy)]
pub struct GapHealerConfig {
    /// Ledgers requested per `getLedgers` page.
    pub page_limit: u32,
    /// Cap on backfill tasks running at once. Real-time indexing is unaffected
    /// regardless; this only bounds how hard we lean on the archive nodes.
    pub max_concurrent_heals: usize,
    /// A gap at least this wide is additionally reported as outage-sized
    /// (`tessera_ledger_gap_outage_total`, plus a warning log) so an operator
    /// or alert can trigger a snapshot rebuild. The healer still attempts the
    /// incremental backfill first.
    pub full_rebuild_threshold: u32,
}

impl Default for GapHealerConfig {
    fn default() -> Self {
        Self {
            page_limit: 200,
            max_concurrent_heals: 4,
            full_rebuild_threshold: 50,
        }
    }
}

impl GapHealerConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            page_limit: env_u32("RWA_GAP_HEALER_PAGE_LIMIT", defaults.page_limit).clamp(1, 1_000),
            max_concurrent_heals: env_u32(
                "RWA_GAP_HEALER_MAX_CONCURRENT",
                defaults.max_concurrent_heals as u32,
            )
            .clamp(1, 64) as usize,
            full_rebuild_threshold: env_u32(
                "RWA_GAP_FULL_REBUILD_THRESHOLD",
                defaults.full_rebuild_threshold,
            ),
        }
    }
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Highest ledger seen so far plus the gaps still awaiting backfill.
///
/// Deliberately free of I/O and of Tokio so the detection rules can be tested
/// as plain functions.
#[derive(Debug, Default)]
pub struct GapTracker {
    last_seen: Option<u32>,
    pending: Vec<LedgerGap>,
}

impl GapTracker {
    pub fn pending(&self) -> &[LedgerGap] {
        &self.pending
    }

    /// Record an observed head ledger.
    ///
    /// Returns the gap that opened, if any. An observation at or below the
    /// current head is a reordering/duplicate and never opens a gap; only a
    /// strictly newer head advances the tracker, and only when it skips at
    /// least one sequence number.
    pub fn observe(&mut self, ledger: u32) -> Option<LedgerGap> {
        match self.last_seen {
            None => {
                self.last_seen = Some(ledger);
                None
            }
            Some(last) if ledger <= last => None,
            Some(last) => {
                self.last_seen = Some(ledger);
                if ledger == last + 1 {
                    return None;
                }
                let gap = LedgerGap::new(last + 1, ledger - 1);
                self.pending.push(gap);
                Some(gap)
            }
        }
    }

    /// Mark a gap as healed so `pending` reflects only outstanding work.
    pub fn complete(&mut self, gap: LedgerGap) {
        self.pending.retain(|g| *g != gap);
    }
}

/// Split a gap into `getLedgers`-sized pages, in ascending order.
pub fn plan_backfill_pages(gap: LedgerGap, page_limit: u32) -> Vec<LedgerGap> {
    if gap.is_empty() || page_limit == 0 {
        return Vec::new();
    }
    let mut pages = Vec::new();
    let mut start = gap.start;
    loop {
        let end = start.saturating_add(page_limit - 1).min(gap.end);
        pages.push(LedgerGap::new(start, end));
        if end >= gap.end {
            break;
        }
        start = end + 1;
    }
    pages
}

/// Detect and heal ledger gaps reported by the refresh loop.
pub struct GapHealer {
    rpc: Arc<RpcClient>,
    config: GapHealerConfig,
    tracker: Arc<Mutex<GapTracker>>,
    permits: Arc<Semaphore>,
}

impl GapHealer {
    pub fn new(rpc: Arc<RpcClient>, config: GapHealerConfig) -> Self {
        let permits = Arc::new(Semaphore::new(config.max_concurrent_heals.max(1)));
        Self {
            rpc,
            config,
            tracker: Arc::new(Mutex::new(GapTracker::default())),
            permits,
        }
    }

    /// Observe the head ledger of a completed refresh cycle.
    ///
    /// Non-blocking: it flags the gap, emits the detection metrics and hands
    /// the backfill to a detached task, so the caller's real-time indexing
    /// loop is never paused. Returns the newly flagged gap, if one opened.
    pub fn observe_ledger(&self, ledger: u32) -> Option<LedgerGap> {
        let (gap, outstanding) = {
            let mut tracker = self.tracker.lock().unwrap();
            let gap = tracker.observe(ledger);
            (gap, tracker.pending().len())
        };
        let gap = gap?;

        metrics::counter!("tessera_ledger_gaps_detected_total").increment(1);
        tracing::warn!(
            start = gap.start,
            end = gap.end,
            missing = gap.len(),
            head = ledger,
            outstanding,
            "ledger sequence gap detected; scheduling background backfill"
        );

        if gap.len() >= self.config.full_rebuild_threshold {
            metrics::counter!("tessera_ledger_gap_outage_total").increment(1);
            tracing::warn!(
                missing = gap.len(),
                threshold = self.config.full_rebuild_threshold,
                "ledger gap is outage-sized; a full snapshot rebuild may be warranted"
            );
        }

        self.spawn_backfill(gap);
        Some(gap)
    }

    /// Spawn the background backfill for `gap`. Falls back to leaving the gap
    /// pending when called outside a Tokio runtime (e.g. from a plain unit
    /// test), where there is nothing to spawn onto.
    fn spawn_backfill(&self, gap: LedgerGap) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::debug!(
                start = gap.start,
                end = gap.end,
                "no tokio runtime available; leaving ledger gap pending"
            );
            return;
        };

        let rpc = Arc::clone(&self.rpc);
        let permits = Arc::clone(&self.permits);
        let tracker = Arc::clone(&self.tracker);
        let page_limit = self.config.page_limit;

        let _heal_task = handle.spawn(async move {
            let _permit = permits.acquire_owned().await;
            match backfill_gap(&rpc, gap, page_limit).await {
                Ok(healed) => {
                    record_healed("rpc", healed);
                    tracker.lock().unwrap().complete(gap);
                    tracing::info!(
                        start = gap.start,
                        end = gap.end,
                        healed,
                        "ledger gap healed"
                    );
                }
                Err(e) => {
                    metrics::counter!("tessera_heal_failures_total", "source" => "rpc")
                        .increment(1);
                    tracing::warn!(
                        start = gap.start,
                        end = gap.end,
                        error = %e,
                        "ledger gap backfill failed; gap remains pending"
                    );
                }
            }
        });
    }
}

/// Fetch `gap` from the archive/RPC nodes page by page, returning how many
/// ledger headers were recovered.
pub async fn backfill_gap(
    rpc: &RpcClient,
    gap: LedgerGap,
    page_limit: u32,
) -> Result<u32, IndexError> {
    if gap.is_empty() {
        return Ok(0);
    }
    let mut healed: u32 = 0;
    for page in plan_backfill_pages(gap, page_limit) {
        let want = page.len();
        let fetched = rpc.ledgers(page.start, want).await?;
        if fetched.sequences.is_empty() {
            // The node has no further headers in this window; nothing left to
            // pull without retrying from a different endpoint on a later pass.
            break;
        }
        healed = healed.saturating_add(fetched.sequences.len() as u32);
        if (fetched.sequences.len() as u32) < want {
            break;
        }
    }

    Ok(healed)
}

/// Prometheus counter for recovered ledgers. Split out so the unit test can
/// drive the real metric name without a live RPC.
fn record_healed(source: &'static str, ledgers: u32) {
    if ledgers > 0 {
        metrics::counter!("tessera_healed_ledgers_total", "source" => source)
            .increment(ledgers as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc_stub() -> Arc<RpcClient> {
        Arc::new(RpcClient::new(
            vec!["https://soroban-testnet.stellar.org".to_string()],
            "GA".to_string(),
        ))
    }

    #[test]
    fn first_observation_never_opens_a_gap() {
        let mut tracker = GapTracker::default();
        assert_eq!(tracker.observe(100), None);
        assert_eq!(tracker.last_seen, Some(100));
        assert!(tracker.pending().is_empty());
    }

    #[test]
    fn contiguous_observations_do_not_open_gaps() {
        let mut tracker = GapTracker::default();
        tracker.observe(100);
        assert_eq!(tracker.observe(101), None);
        assert_eq!(tracker.observe(102), None);
        assert_eq!(tracker.last_seen, Some(102));
        assert!(tracker.pending().is_empty());
    }

    #[test]
    fn a_jump_flags_the_missing_sequence_range() {
        let mut tracker = GapTracker::default();
        tracker.observe(1_000);
        let gap = tracker.observe(1_040).expect("a 39-ledger jump is a gap");
        assert_eq!(gap, LedgerGap::new(1_001, 1_039));
        assert_eq!(gap.len(), 39);
        assert_eq!(tracker.pending(), &[LedgerGap::new(1_001, 1_039)]);
        assert_eq!(tracker.last_seen, Some(1_040));
    }

    #[test]
    fn reordered_and_duplicate_heads_do_not_open_gaps() {
        let mut tracker = GapTracker::default();
        tracker.observe(500);
        assert_eq!(tracker.observe(500), None, "duplicate head");
        assert_eq!(tracker.observe(499), None, "out-of-order older head");
        assert_eq!(tracker.observe(120), None, "stale failover node");
        assert_eq!(tracker.last_seen, Some(500));
        assert!(tracker.pending().is_empty());
    }

    #[test]
    fn consecutive_gaps_track_independently_and_complete() {
        let mut tracker = GapTracker::default();
        tracker.observe(10);
        let first = tracker.observe(20).unwrap();
        let second = tracker.observe(30).unwrap();
        assert_eq!(first, LedgerGap::new(11, 19));
        assert_eq!(second, LedgerGap::new(21, 29));
        assert_eq!(tracker.pending().len(), 2);

        tracker.complete(first);
        assert_eq!(tracker.pending(), &[second]);
        tracker.complete(second);
        assert!(tracker.pending().is_empty());
    }

    #[test]
    fn gap_length_is_inclusive() {
        assert_eq!(LedgerGap::new(7, 7).len(), 1);
        assert_eq!(LedgerGap::new(7, 9).len(), 3);
        assert!(LedgerGap::new(9, 7).is_empty());
    }

    #[test]
    fn pages_are_bounded_and_contiguous() {
        let pages = plan_backfill_pages(LedgerGap::new(100, 349), 100);
        assert_eq!(
            pages,
            vec![
                LedgerGap::new(100, 199),
                LedgerGap::new(200, 299),
                LedgerGap::new(300, 349),
            ]
        );
    }

    #[test]
    fn single_ledger_gap_and_degenerate_limits() {
        assert_eq!(
            plan_backfill_pages(LedgerGap::new(5, 5), 100),
            vec![LedgerGap::new(5, 5)]
        );
        assert!(plan_backfill_pages(LedgerGap::new(5, 9), 0).is_empty());
        assert!(plan_backfill_pages(LedgerGap::new(9, 5), 10).is_empty());
    }

    #[test]
    fn healed_metric_uses_the_documented_name() {
        // Mirror the indexer's own metric tests: capture the emission through
        // a local recorder so we assert the exact Prometheus series name.
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            record_healed("rpc", 7);
            record_healed("rpc", 0);
        });
        let rendered = handle.render();
        assert!(
            rendered.contains("tessera_healed_ledgers_total{source=\"rpc\"} 7"),
            "healed ledger metric must be named tessera_healed_ledgers_total; got:\n{rendered}"
        );
    }

    #[test]
    fn observe_ledger_flags_and_queues_without_a_runtime() {
        let healer = GapHealer::new(rpc_stub(), GapHealerConfig::default());
        assert_eq!(healer.observe_ledger(1_000), None);
        let gap = healer.observe_ledger(1_010).expect("gap flagged");
        assert_eq!(gap, LedgerGap::new(1_001, 1_009));
        // No Tokio runtime in a plain #[test], so the backfill stays pending
        // instead of spawning — detection still happened.
        let tracker = healer.tracker.lock().unwrap();
        assert_eq!(tracker.pending(), &[LedgerGap::new(1_001, 1_009)]);
        assert_eq!(tracker.last_seen, Some(1_010));
    }
}
