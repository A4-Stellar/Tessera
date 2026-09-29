//! Automated ledger gap detection and healing (issue #156).
//!
//! Event history enters the store through full replay runs
//! (`tessera-api replay`) and the admin DLQ retry. Both record the ledger
//! ranges they covered in a persistent *coverage* file. This module reads
//! that coverage, finds holes — windows no successful run ever covered
//! (network outage, RPC retention window expiring, crashed replay) — and
//! backfills them from archive RPC endpoints without pausing real-time
//! indexing.
//!
//! # Coverage model
//!
//! Coverage is a set of half-open `[start, end)` windows per contract scope
//! (sorted, deduplicated contract IDs), persisted as JSON at
//! `RWA_COVERAGE_STORE` (default [`DEFAULT_COVERAGE_STORE`]) with the same
//! temp-write + fsync + rename + lock-file layout as the event store. A
//! window is only recorded *after* its events are merged, so coverage never
//! claims ledgers the store does not hold.
//!
//! # What counts as a gap
//!
//! * **Interior holes** — `[end of one covered window, start of the next)`
//!   — are true outage artifacts and are always healed.
//! * **Before the first covered window** is not a gap: that is history from
//!   before replay coverage began, and healing it from ledger 0 would be an
//!   unbounded multi-day backfill.
//! * **After the last covered window** (the leading edge up to the chain
//!   head) is not healed by default — a future live event pipeline owns it.
//!   Opt in per deployment with `RWA_GAP_HEAL_HEAD=1` to have the healer
//!   trail the head in bounded windows.
//!
//! # Healing
//!
//! [`GapHealer::run_once`] backfills gaps oldest-first, split into
//! [`MAX_BACKFILL_WINDOW`]-ledger chunks and capped at
//! [`MAX_HEALED_PER_CYCLE`] ledgers per cycle, so one huge hole is healed
//! across cycles instead of monopolising one. Fetches retry transient
//! errors exactly like replay ([`super::replay::fetch_with_retry`]); a
//! window is recorded as covered once fetched — even when it legitimately
//! contained no events — so quiet ranges are never re-fetched forever.
//!
//! Decoding errors quarantine to the DLQ inside `RpcEventSource::fetch_window`
//! exactly as in a real replay, and merges are atomic: a failed backfill
//! leaves the event store and the coverage file untouched.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::replay::{fetch_with_retry, EventSource, FileEventStore, ShadowState};
use crate::cluster::leader_election::NodeRole;

/// Where coverage state lives when `RWA_COVERAGE_STORE` is unset.
pub const DEFAULT_COVERAGE_STORE: &str = "./data/coverage.json";

/// Upper bound on ledgers fetched in a single backfill window.
const MAX_BACKFILL_WINDOW: u32 = 5_000;

/// Upper bound on ledgers healed per `run_once` cycle, so one huge hole is
/// spread across cycles and the healer never monopolises the archive RPC.
const MAX_HEALED_PER_CYCLE: u64 = u64::from(MAX_BACKFILL_WINDOW);

/// Upper bound on coverage entries kept per scope, so a pathological client
/// cannot grow the state file without bound. Windows are normalised before
/// the cap applies, so legitimate history stays well under it.
const MAX_WINDOWS_PER_SCOPE: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum GapHealerError {
    #[error("store error: {0}")]
    Store(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// Coverage state
// ---------------------------------------------------------------------------

/// One half-open covered range `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Window {
    start: u32,
    /// Exclusive end.
    end: u32,
}

impl Window {
    fn new(start: u32, end: u32) -> Self {
        Window { start, end }
    }

    pub fn contains(&self, ledger: u32) -> bool {
        self.start <= ledger && ledger < self.end
    }
}

/// Contract scope key: sorted, deduplicated contract IDs.
fn scope_key(mut contracts: Vec<String>) -> String {
    contracts.sort();
    contracts.dedup();
    contracts.join(",")
}

/// Coverage for every scope, keyed by the sorted contract list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LedgerCoverage {
    scopes: BTreeMap<String, Vec<Window>>,
}

impl LedgerCoverage {
    /// Record that `[start, end)` was fully fetched-and-merged for
    /// `contracts`. Zero-width windows are ignored (nothing was covered).
    pub fn cover(&mut self, contracts: &[String], start: u32, end: u32) {
        if start >= end {
            return;
        }
        let windows = self.scopes.entry(scope_key(contracts.to_vec())).or_default();
        windows.push(Window::new(start, end));
        windows.sort_unstable();
        self.normalize(windows);
    }

    /// Merge overlapping/adjacent windows ([a,b) touching [b,c) is
    /// contiguous); cap the entry count, keeping the widest windows.
    fn normalize(&self, windows: &mut Vec<Window>) {
        let mut merged: Vec<Window> = Vec::with_capacity(windows.len());
        for &w in windows.iter() {
            match merged.last_mut() {
                Some(last) if w.start <= last.end => {
                    last.end = last.end.max(w.end);
                }
                _ => merged.push(w),
            }
        }
        if merged.len() > MAX_WINDOWS_PER_SCOPE {
            merged.sort_unstable_by_key(|w| std::cmp::Reverse(w.end - w.start));
            merged.truncate(MAX_WINDOWS_PER_SCOPE);
            merged.sort_unstable();
        }
        *windows = merged;
    }

    /// The contract scopes that have any coverage, split back into
    /// contract lists. An empty scope key (never produced by `replay`,
    /// which requires at least one contract) is skipped.
    pub fn scope_keys(&self) -> Vec<Vec<String>> {
        self.scopes
            .keys()
            .filter(|k| !k.is_empty())
            .map(|k| k.split(',').map(String::from).collect())
            .collect()
    }

    /// Uncovered ledgers strictly below `head` (exclusive), oldest first.
    ///
    /// With `include_head` cleared, only *interior* holes between two
    /// covered windows are returned: everything before the first window is
    /// pre-coverage history, and everything after the last window is the
    /// leading edge owned by live ingestion (see the module docs). With
    /// `include_head` set, the leading edge is reported too.
    pub fn gaps_below(&self, contracts: &[String], head: u32, include_head: bool) -> Vec<Window> {
        let Some(windows) = self.scopes.get(&scope_key(contracts.to_vec())) else {
            return Vec::new();
        };
        let mut gaps = Vec::new();
        // Windows are normalised (sorted, non-overlapping, non-adjacent), so
        // consecutive pairs are exactly where interior holes can live.
        for pair in windows.windows(2) {
            let (prev, next) = (pair[0], pair[1]);
            if prev.end < next.start && prev.end < head {
                gaps.push(Window::new(prev.end, next.start.min(head)));
            }
        }
        if include_head {
            if let Some(last) = windows.last() {
                if last.end < head {
                    gaps.push(Window::new(last.end, head));
                }
            }
        }
        gaps
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "exercised by the coverage round-trip test; the healer checks gaps wholesale"))]
    pub fn is_covered(&self, contracts: &[String], ledger: u32) -> bool {
        self.scopes
            .get(&scope_key(contracts.to_vec()))
            .is_some_and(|ws| ws.iter().any(|w| w.contains(ledger)))
    }
}

/// Deletes the lock file when dropped (same RAII layout as `dlq::Unlock`).
struct Unlock {
    path: PathBuf,
    _file: fs::File,
}

impl Unlock {
    fn acquire(path: &Path) -> Result<Self, GapHealerError> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| GapHealerError::Store(format!("could not take coverage lock ({e})")))?;
        Ok(Unlock {
            path: path.to_path_buf(),
            _file: file,
        })
    }
}

impl Drop for Unlock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Load coverage from disk. A missing file is empty coverage; a corrupt file
/// is moved to `<path>.corrupt` for triage and treated as empty — a wedged
/// state file must not stop healing.
pub fn load_coverage(path: &Path) -> Result<LedgerCoverage, GapHealerError> {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(coverage) => Ok(coverage),
            Err(e) => {
                let backup = path.with_extension("corrupt");
                match fs::rename(path, &backup) {
                    Ok(()) => {
                        tracing::warn!(
                            error = %e,
                            backup = %backup.display(),
                            "coverage store corrupt; moved aside, continuing empty"
                        );
                        Ok(LedgerCoverage::default())
                    }
                    Err(io) => Err(GapHealerError::Store(format!(
                        "corrupt coverage store {}: {e} (backup failed: {io})",
                        path.display()
                    ))),
                }
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LedgerCoverage::default()),
        Err(e) => Err(e.into()),
    }
}

/// Atomically replace the coverage file (temp, fsync, rename) under a lock
/// file, so a replay in another process and the healer cannot lose each
/// other's updates.
pub fn save_coverage(path: &Path, coverage: &LedgerCoverage) -> Result<(), GapHealerError> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let _lock = Unlock::acquire(&path.with_extension("lock"))?;
    let bytes = serde_json::to_vec_pretty(coverage)?;
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Record a merged window in the env-configured coverage store. Best-effort:
/// a failure to persist coverage is logged, never fatal — the worst case is
/// that the healer re-fetches a window, which is idempotent.
pub fn record_covered(contracts: &[String], start: u32, end: u32) {
    let path = coverage_path();
    let record = || -> Result<(), GapHealerError> {
        let mut coverage = load_coverage(&path)?;
        coverage.cover(contracts, start, end);
        save_coverage(&path, &coverage)
    };
    if let Err(e) = record() {
        tracing::warn!(error = %e, "failed to record replay coverage");
    }
}

fn coverage_path() -> PathBuf {
    PathBuf::from(
        std::env::var("RWA_COVERAGE_STORE").unwrap_or_else(|_| DEFAULT_COVERAGE_STORE.into()),
    )
}

// ---------------------------------------------------------------------------
// Healer
// ---------------------------------------------------------------------------

pub struct GapHealer {
    src: Arc<dyn EventSource>,
    store: FileEventStore,
    coverage_path: PathBuf,
    /// Base delay for transient-error backoff between backfill attempts.
    base_backoff: Duration,
    /// Also heal the leading edge up to the chain head (opt-in; see the
    /// module docs).
    heal_head: bool,
}

impl GapHealer {
    pub fn new(src: Arc<dyn EventSource>, store: FileEventStore) -> Self {
        GapHealer {
            src,
            store,
            coverage_path: coverage_path(),
            base_backoff: Duration::from_millis(500),
            heal_head: false,
        }
    }

    /// Override the coverage path (tests).
    #[cfg_attr(not(test), expect(dead_code, reason = "test-only seam; production uses the env-configured path"))]
    fn with_coverage_path(mut self, path: PathBuf) -> Self {
        self.coverage_path = path;
        self
    }

    /// Opt in to healing the leading edge up to the chain head.
    pub fn with_heal_head(mut self, heal_head: bool) -> Self {
        self.heal_head = heal_head;
        self
    }

    /// Override the transient-error backoff base (tests).
    #[cfg_attr(not(test), expect(dead_code, reason = "test-only seam; production retries with the replay default"))]
    fn with_base_backoff(mut self, base_backoff: Duration) -> Self {
        self.base_backoff = base_backoff;
        self
    }

    /// One healing cycle across every covered scope: find gaps, backfill the
    /// oldest ones (bounded), record coverage for what was fetched. Returns
    /// the number of healed ledgers.
    pub async fn run_once(&self, head: u32) -> Result<u64, GapHealerError> {
        let coverage = load_coverage(&self.coverage_path)?;
        let mut healed = 0u64;

        for contracts in coverage.scope_keys() {
            for gap in coverage.gaps_below(&contracts, head, self.heal_head) {
                let mut cursor = gap.start;
                while cursor < gap.end {
                    let end = cursor.saturating_add(MAX_BACKFILL_WINDOW).min(gap.end);
                    self.backfill(&contracts, cursor, end).await?;
                    healed += u64::from(end - cursor);
                    if healed >= MAX_HEALED_PER_CYCLE {
                        return Ok(healed);
                    }
                    cursor = end;
                }
            }
        }
        Ok(healed)
    }

    /// Fetch and merge one `[start, end)` window. Coverage is recorded even
    /// when the window contained no events: a clean fetch proves those
    /// ledgers have no contract activity, and skipping the record would
    /// re-fetch the same quiet window every cycle forever.
    async fn backfill(
        &self,
        contracts: &[String],
        start: u32,
        end: u32,
    ) -> Result<(), GapHealerError> {
        // `fetch_window` is inclusive of end; coverage windows are not.
        debug_assert!(end > start);
        let events =
            fetch_with_retry(self.src.as_ref(), start, end - 1, contracts, self.base_backoff)
                .await
                .map_err(|e| GapHealerError::Store(e.to_string()))?;

        if !events.is_empty() {
            let shadow = ShadowState {
                start_ledger: start,
                end_ledger: end - 1,
                contracts: contracts.to_vec(),
                next_ledger: end,
                events,
            };
            let report = self
                .store
                .merge(&shadow)
                .map_err(|e| GapHealerError::Store(e.to_string()))?;
            tracing::info!(
                start,
                end,
                inserted = report.inserted,
                replaced = report.replaced,
                "ledger gap healed"
            );
        } else {
            tracing::debug!(start, end, "gap window contained no contract events");
        }

        record_covered_at(&self.coverage_path, contracts, start, end)?;
        metrics::counter!("tessera_healed_ledgers_total").increment(u64::from(end - start));
        Ok(())
    }

    /// Healing loop: every `interval`, if this node is still the leader,
    /// read the chain head and run one cycle. Runs until `shutdown` flips;
    /// a demotion mid-cycle lets the cycle finish (merges are atomic and
    /// idempotent) and the next cycle is skipped.
    pub async fn run_forever(
        self,
        head_urls: Vec<String>,
        interval: Duration,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        role: tokio::sync::watch::Receiver<NodeRole>,
    ) {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        loop {
            if *shutdown.borrow() {
                tracing::info!("shutdown signal received; stopping gap healer");
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("shutdown signal received; stopping gap healer");
                        return;
                    }
                }
            }
            if *role.borrow() != NodeRole::Leader {
                continue;
            }
            let Some(head) = latest_chain_ledger(&http, &head_urls).await else {
                tracing::warn!("could not read chain head; skipping healing cycle");
                continue;
            };
            match self.run_once(head).await {
                Ok(0) => {}
                Ok(healed) => tracing::info!(healed, head, "gap healing cycle complete"),
                Err(e) => tracing::warn!(error = %e, head, "gap healing cycle failed"),
            }
        }
    }
}

/// Record coverage at a specific path ([`record_covered`] uses the
/// env-configured one; the healer pins its own for testability).
fn record_covered_at(
    path: &Path,
    contracts: &[String],
    start: u32,
    end: u32,
) -> Result<(), GapHealerError> {
    let mut coverage = load_coverage(path)?;
    coverage.cover(contracts, start, end);
    save_coverage(path, &coverage)
}

/// Chain head via the JSON-RPC `getLatestLedger` call, failing over across
/// endpoints. Thin and side-effect free; `None` only when every endpoint
/// fails, in which case the cycle is skipped.
async fn latest_chain_ledger(http: &reqwest::Client, urls: &[String]) -> Option<u32> {
    for url in urls {
        let parsed = http
            .post(url)
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getLatestLedger"}))
            .send()
            .await
            .ok()
            .and_then(|resp| resp.error_for_status().ok());
        let body = match parsed {
            Some(resp) => resp.json::<serde_json::Value>().await.ok(),
            None => continue,
        };
        if let Some(seq) = body
            .as_ref()
            .and_then(|b| b.get("result"))
            .and_then(|r| r.get("sequence"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|seq| u32::try_from(seq).ok())
        {
            return Some(seq);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tessera-gap-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ev(id: u64, ledger: u32, contract: &str) -> crate::models::Event {
        crate::models::Event {
            id,
            contract: contract.to_string(),
            event_type: "test".into(),
            ledger,
            timestamp: None,
            data: serde_json::json!({}),
        }
    }

    const CA: &str = "CA";

    /// Fetches `start..=end` one event per ledger; optionally fails the
    /// first `fail_first` calls with a transient RPC error, or always.
    struct FakeSource {
        calls: AtomicU32,
        fail_first: u32,
    }

    #[async_trait]
    impl EventSource for FakeSource {
        async fn fetch_window(
            &self,
            start: u32,
            end: u32,
            contracts: &[String],
        ) -> Result<Vec<crate::models::Event>, super::super::replay::ReplayError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_first {
                return Err(super::super::replay::ReplayError::Rpc {
                    message: "boom".into(),
                    transient: true,
                });
            }
            Ok((start..=end)
                .map(|l| ev(u64::from(l), l, &contracts[0]))
                .collect::<Vec<crate::models::Event>>())
        }
    }

    fn coverage_with(windows: &[(u32, u32)]) -> LedgerCoverage {
        let mut coverage = LedgerCoverage::default();
        for (start, end) in windows {
            coverage.cover(&[CA.to_string()], *start, *end);
        }
        coverage
    }

    #[test]
    fn cover_merges_adjacent_and_overlapping_windows() {
        let mut coverage = coverage_with(&[(10, 20), (20, 30)]);
        assert_eq!(coverage.scopes["CA"].len(), 1);
        assert_eq!(coverage.scopes["CA"][0], Window::new(10, 30));

        coverage.cover(&[CA.to_string()], 25, 40);
        assert_eq!(coverage.scopes["CA"][0], Window::new(10, 40));

        // Disjoint windows stay separate.
        coverage.cover(&[CA.to_string()], 50, 60);
        assert_eq!(coverage.scopes["CA"].len(), 2);

        // Zero-width windows are ignored.
        coverage.cover(&[CA.to_string()], 70, 70);
        assert_eq!(coverage.scopes["CA"].len(), 2);
    }

    #[test]
    fn only_interior_holes_are_gaps_by_default() {
        // [0,100) [300,400): hole [100,300) is interior; the leading edge
        // before 0 does not exist here; [400,head) is the leading edge.
        let coverage = coverage_with(&[(0, 100), (300, 400)]);

        assert_eq!(coverage.gaps_below(&[CA.into()], 500, false), [Window::new(100, 300)]);
        assert_eq!(
            coverage.gaps_below(&[CA.into()], 500, true),
            [Window::new(100, 300), Window::new(400, 500)]
        );
        // The head clips the leading edge: last.end=400 > head=350, so no
        // leading-edge gap — only the interior hole is returned.
        assert_eq!(coverage.gaps_below(&[CA.into()], 350, true), [Window::new(100, 300)]);
    }

    #[test]
    fn history_before_first_window_is_not_a_gap() {
        // Coverage starts at 1000: [0,1000) is pre-coverage history.
        let coverage = coverage_with(&[(1000, 1100), (1200, 1300)]);
        assert_eq!(coverage.gaps_below(&[CA.into()], 2000, false), [Window::new(1100, 1200)]);
        assert!(coverage.gaps_below(&[CA.into()], 1000, true).is_empty());
    }

    #[test]
    fn empty_coverage_yields_no_gaps() {
        let coverage = LedgerCoverage::default();
        // Seeding is replay's job; the healer must not backfill from 0.
        assert!(coverage.gaps_below(&[CA.into()], 999_999, true).is_empty());
    }

    #[tokio::test]
    async fn run_once_backfills_gaps_and_records_coverage() {
        let dir = tmpdir("heal");
        let coverage_path = dir.join("coverage.json");
        save_coverage(&coverage_path, &coverage_with(&[(10, 20), (30, 40)])).unwrap();
        let store_path = dir.join("events.json");

        let healer = GapHealer::new(
            Arc::new(FakeSource {
                calls: AtomicU32::new(0),
                fail_first: 0,
            }),
            FileEventStore::new(&store_path),
        )
        .with_coverage_path(coverage_path.clone());

        let healed = healer.run_once(45).await.unwrap();
        assert_eq!(healed, 10, "interior gap [20,30) healed");

        // Coverage now spans the healed hole (adjacent windows merged).
        let coverage = load_coverage(&coverage_path).unwrap();
        assert_eq!(coverage.scopes["CA"], vec![Window::new(10, 40)]);

        // The event store holds the backfilled ledgers.
        let events = FileEventStore::new(&store_path).load().unwrap();
        assert_eq!(events.len(), 10);
        assert!(events.iter().all(|e| (20..30).contains(&e.ledger)));

        // Nothing left to heal below the head.
        assert_eq!(healer.run_once(45).await.unwrap(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_once_records_coverage_for_empty_windows() {
        let dir = tmpdir("quiet");
        let coverage_path = dir.join("coverage.json");
        save_coverage(&coverage_path, &coverage_with(&[(10, 20), (30, 40)])).unwrap();

        // A source that always returns zero events (quiet ledgers).
        struct QuietSource;
        #[async_trait]
        impl EventSource for QuietSource {
            async fn fetch_window(
                &self,
                _start: u32,
                _end: u32,
                _contracts: &[String],
            ) -> Result<Vec<crate::models::Event>, super::super::replay::ReplayError> {
                Ok(Vec::new())
            }
        }

        let healer = GapHealer::new(Arc::new(QuietSource), FileEventStore::new(dir.join("e.json")))
            .with_coverage_path(coverage_path.clone());

        assert_eq!(healer.run_once(45).await.unwrap(), 10);
        let coverage = load_coverage(&coverage_path).unwrap();
        assert_eq!(
            coverage.scopes["CA"],
            vec![Window::new(10, 40)],
            "quiet windows must still be covered, or they refetch forever"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_once_leading_edge_only_when_opted_in() {
        let dir = tmpdir("head");
        let coverage_path = dir.join("coverage.json");
        save_coverage(&coverage_path, &coverage_with(&[(10, 20)])).unwrap();
        let store_path = dir.join("events.json");

        let make = || {
            GapHealer::new(
                Arc::new(FakeSource {
                    calls: AtomicU32::new(0),
                    fail_first: 0,
                }),
                FileEventStore::new(&store_path),
            )
            .with_coverage_path(coverage_path.clone())
        };

        // Default: leading edge untouched.
        assert_eq!(make().run_once(30).await.unwrap(), 0);
        assert!(FileEventStore::new(&store_path).load().unwrap().is_empty());

        // Opt-in: [20,30) healed.
        assert_eq!(make().with_heal_head(true).run_once(30).await.unwrap(), 10);
        let coverage = load_coverage(&coverage_path).unwrap();
        assert_eq!(coverage.scopes["CA"], vec![Window::new(10, 30)]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_once_fails_closed_on_persistent_fetch_errors() {
        let dir = tmpdir("fail");
        let coverage_path = dir.join("coverage.json");
        save_coverage(&coverage_path, &coverage_with(&[(10, 20), (30, 40)])).unwrap();

        let healer = GapHealer::new(
            Arc::new(FakeSource {
                calls: AtomicU32::new(0),
                fail_first: u32::MAX,
            }),
            FileEventStore::new(dir.join("e.json")),
        )
        .with_coverage_path(coverage_path.clone())
        .with_base_backoff(Duration::ZERO);

        let result = healer.run_once(45).await;
        assert!(matches!(result, Err(GapHealerError::Store(_))));

        // The store and coverage are untouched by the failed cycle.
        let coverage = load_coverage(&coverage_path).unwrap();
        assert_eq!(coverage.scopes["CA"].len(), 2);
        assert!(FileEventStore::new(dir.join("e.json")).load().unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn corrupt_coverage_file_is_moved_aside() {
        let dir = tmpdir("corrupt");
        let coverage_path = dir.join("coverage.json");
        fs::write(&coverage_path, b"{not json").unwrap();

        let healer = GapHealer::new(
            Arc::new(FakeSource {
                calls: AtomicU32::new(0),
                fail_first: 0,
            }),
            FileEventStore::new(dir.join("e.json")),
        )
        .with_coverage_path(coverage_path.clone());

        // Empty coverage → no gaps → clean no-op cycle.
        assert_eq!(healer.run_once(100).await.unwrap(), 0);
        assert!(coverage_path.with_extension("corrupt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn coverage_file_roundtrips_and_releases_its_lock() {
        let dir = tmpdir("roundtrip");
        let path = dir.join("coverage.json");
        let coverage = coverage_with(&[(5, 10), (20, 30)]);

        save_coverage(&path, &coverage).unwrap();
        assert!(
            !path.with_extension("lock").exists(),
            "coverage lock must be released after save"
        );
        // A second save immediately succeeds (cross-process safety).
        save_coverage(&path, &coverage).unwrap();

        let loaded = load_coverage(&path).unwrap();
        assert_eq!(loaded.scopes["CA"], coverage.scopes["CA"]);
        assert!(loaded.is_covered(&[CA.into()], 7));
        assert!(!loaded.is_covered(&[CA.into()], 15));
        let _ = fs::remove_dir_all(&dir);
    }
}
