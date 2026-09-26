//! High-availability event replay & backfill (issue #104).
//!
//! ```text
//! tessera-api replay --start-ledger 1000 --end-ledger 5000 --contracts CABC...,CDEF...
//! ```
//!
//! Flow:
//!
//! 1. The range is split into windows. Each window is fetched from Soroban RPC
//!    `getEvents` (with paging, retry/backoff and endpoint failover) through
//!    the [`EventSource`] trait.
//! 2. Fetched events accumulate in an isolated [`ShadowState`]. Nothing the
//!    running API serves or stores is touched while replay is in flight. After
//!    every window the shadow is checkpointed to disk (atomic rename), so an
//!    interrupted replay resumes where it stopped instead of starting over.
//! 3. Only when the *whole* range succeeded is the shadow merged into the main
//!    event store in a single atomic step ([`FileEventStore::merge`]: write a
//!    temp file, fsync, rename over the store, guarded by a lock file). A
//!    failed replay leaves the store byte-for-byte unchanged.
//! 4. The merge semantic is *replace within scope*: existing events for the
//!    replayed contracts inside `[start, end]` are replaced by the shadow's
//!    events, everything else is kept, so re-running a replay is idempotent.
//! 5. Progress is logged after each window with rate and ETA ([`Progress`]).
//!
//! Persistence note: the indexer's state is an in-memory snapshot rebuilt from
//! RPC reads; there is no database in this codebase yet. The "main persistence
//! layer" for replayed events is therefore the JSON event store at
//! `RWA_EVENT_STORE` (default `./data/events.json`), which the running server
//! reads on each refresh (see [`stored_events`]). [`AppState::merge_events`]
//! applies the same merge to the live snapshot atomically via `ArcSwap` for
//! in-process callers. When a database lands, implement the same merge there.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use stellar_xdr::curr as xdr;
use stellar_xdr::curr::{Limits, ReadXdr};

use super::{scval_to_json, AppState, Config};
use crate::models::Event;

pub const DEFAULT_WINDOW_LEDGERS: u32 = 500;
const PAGE_LIMIT: u32 = 1000;
const MAX_ATTEMPTS: u32 = 5;
const BASE_BACKOFF: Duration = Duration::from_millis(500);
/// Soroban RPC accepts at most 5 filters of 5 contract IDs each.
const MAX_CONTRACTS: usize = 25;

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("invalid arguments: {0}")]
    Args(String),
    #[error("rpc error (transient={transient}): {message}")]
    Rpc { message: String, transient: bool },
    #[error("event decode error: {0}")]
    Decode(String),
    #[error("store error: {0}")]
    Store(String),
}

impl ReplayError {
    fn is_transient(&self) -> bool {
        matches!(self, ReplayError::Rpc { transient: true, .. })
    }
}

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayArgs {
    pub start_ledger: u32,
    pub end_ledger: u32,
    pub contracts: Vec<String>,
    pub window: u32,
    pub dry_run: bool,
}

/// Parse the arguments following `replay`:
/// `--start-ledger X --end-ledger Y --contracts A,B [--window N] [--dry-run]`.
/// `--contracts` accepts a comma separated list and/or several values.
pub fn parse_args(args: &[String]) -> Result<ReplayArgs, ReplayError> {
    let (mut start, mut end, mut window) = (None, None, DEFAULT_WINDOW_LEDGERS);
    let mut contracts: Vec<String> = Vec::new();
    let mut dry_run = false;
    let mut i = 0;
    let num = |name: &str, v: Option<&String>| -> Result<u32, ReplayError> {
        v.ok_or_else(|| ReplayError::Args(format!("{name} needs a value")))?
            .parse::<u32>()
            .map_err(|_| ReplayError::Args(format!("{name} must be a non-negative integer")))
    };
    while i < args.len() {
        match args[i].as_str() {
            "--start-ledger" => {
                start = Some(num("--start-ledger", args.get(i + 1))?);
                i += 2;
            }
            "--end-ledger" => {
                end = Some(num("--end-ledger", args.get(i + 1))?);
                i += 2;
            }
            "--window" => {
                window = num("--window", args.get(i + 1))?;
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--contracts" => {
                i += 1;
                while i < args.len() && !args[i].starts_with("--") {
                    contracts.extend(
                        args[i]
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned),
                    );
                    i += 1;
                }
            }
            other => return Err(ReplayError::Args(format!("unknown argument `{other}`"))),
        }
    }
    let start_ledger = start.ok_or_else(|| ReplayError::Args("--start-ledger is required".into()))?;
    let end_ledger = end.ok_or_else(|| ReplayError::Args("--end-ledger is required".into()))?;
    if start_ledger == 0 || end_ledger < start_ledger {
        return Err(ReplayError::Args(
            "ledger range must satisfy 1 <= start <= end".into(),
        ));
    }
    if window == 0 {
        return Err(ReplayError::Args("--window must be > 0".into()));
    }
    if contracts.is_empty() {
        return Err(ReplayError::Args("--contracts needs at least one contract ID".into()));
    }
    if contracts.len() > MAX_CONTRACTS {
        return Err(ReplayError::Args(format!(
            "at most {MAX_CONTRACTS} contracts per replay (Soroban RPC filter limit)"
        )));
    }
    contracts.sort();
    contracts.dedup();
    for c in &contracts {
        stellar_strkey::Contract::from_string(c)
            .map_err(|e| ReplayError::Args(format!("invalid contract ID {c}: {e}")))?;
    }
    Ok(ReplayArgs {
        start_ledger,
        end_ledger,
        contracts,
        window,
        dry_run,
    })
}

// ---------------------------------------------------------------------------
// Progress / ETA
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Progress {
    total: u64,
    done: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProgressReport {
    pub done: u64,
    pub total: u64,
    pub percent: f64,
    pub ledgers_per_sec: f64,
    pub eta: Option<Duration>,
}

impl Progress {
    pub fn new(total: u64) -> Self {
        Progress { total, done: 0 }
    }

    pub fn advance(&mut self, ledgers: u64) {
        self.done = (self.done + ledgers).min(self.total);
    }

    /// `elapsed` is the time spent on ledgers processed in *this* run, and
    /// `done_this_run` how many of them there were, so resumed work does not
    /// inflate the rate.
    pub fn report(&self, elapsed: Duration, done_this_run: u64) -> ProgressReport {
        let secs = elapsed.as_secs_f64();
        let rate = if secs > 0.0 { done_this_run as f64 / secs } else { 0.0 };
        let remaining = self.total.saturating_sub(self.done);
        let eta = if remaining == 0 {
            Some(Duration::ZERO)
        } else if rate > 0.0 {
            Some(Duration::from_secs_f64(remaining as f64 / rate))
        } else {
            None
        };
        ProgressReport {
            done: self.done,
            total: self.total,
            percent: if self.total == 0 { 100.0 } else { self.done as f64 * 100.0 / self.total as f64 },
            ledgers_per_sec: rate,
            eta,
        }
    }
}

impl ProgressReport {
    pub fn line(&self, events: usize) -> String {
        let eta = self
            .eta
            .map(|d| format!("{}s", d.as_secs()))
            .unwrap_or_else(|| "unknown".into());
        format!(
            "replay {}/{} ledgers ({:.1}%) {:.1} ledgers/s ETA {} events={}",
            self.done, self.total, self.percent, self.ledgers_per_sec, eta, events
        )
    }
}

// ---------------------------------------------------------------------------
// Shadow state & merge
// ---------------------------------------------------------------------------

/// Isolated accumulator for a replay run; also the on-disk checkpoint format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShadowState {
    pub start_ledger: u32,
    pub end_ledger: u32,
    pub contracts: Vec<String>,
    /// First ledger not yet replayed.
    pub next_ledger: u32,
    pub events: Vec<Event>,
}

impl ShadowState {
    pub fn new(args: &ReplayArgs) -> Self {
        ShadowState {
            start_ledger: args.start_ledger,
            end_ledger: args.end_ledger,
            contracts: args.contracts.clone(),
            next_ledger: args.start_ledger,
            events: Vec::new(),
        }
    }

    fn matches(&self, args: &ReplayArgs) -> bool {
        self.start_ledger == args.start_ledger
            && self.end_ledger == args.end_ledger
            && self.contracts == args.contracts
    }

    pub fn is_complete(&self) -> bool {
        self.next_ledger > self.end_ledger
    }
}

/// Pure merge: replace `existing` events within `[start, end]` for the given
/// contracts with `shadow`, keep all others, dedupe by id, sort by
/// `(ledger, id)`. Idempotent for a fixed shadow.
pub fn merge_events(
    existing: Vec<Event>,
    shadow: &[Event],
    start: u32,
    end: u32,
    contracts: &[String],
) -> Vec<Event> {
    let in_scope = |e: &Event| {
        e.ledger >= start && e.ledger <= end && contracts.iter().any(|c| c == &e.contract)
    };
    let mut by_key: BTreeMap<(u32, u64), Event> = BTreeMap::new();
    for e in existing.into_iter().filter(|e| !in_scope(e)) {
        by_key.insert((e.ledger, e.id), e);
    }
    for e in shadow {
        by_key.insert((e.ledger, e.id), e.clone());
    }
    let mut seen = HashSet::new();
    by_key
        .into_values()
        .filter(|e| seen.insert(e.id))
        .collect()
}

impl AppState {
    /// Atomically merge replayed events into the live snapshot (single
    /// `ArcSwap` store: readers see the old or the new event list, never a mix).
    pub fn merge_events(&self, shadow: &ShadowState) {
        let mut next = self.snapshot();
        next.events = merge_events(
            std::mem::take(&mut next.events),
            &shadow.events,
            shadow.start_ledger,
            shadow.end_ledger,
            &shadow.contracts,
        );
        self.replace(next);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeReport {
    pub replaced: usize,
    pub inserted: usize,
    pub total_after: usize,
}

/// JSON event store with atomic, lock-guarded merges.
pub struct FileEventStore {
    path: PathBuf,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

impl FileEventStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileEventStore { path: path.into() }
    }

    pub fn from_env() -> Self {
        Self::new(store_path())
    }

    pub fn load(&self) -> Result<Vec<Event>, ReplayError> {
        match fs::read(&self.path) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| ReplayError::Store(format!("corrupt store {}: {e}", self.path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(ReplayError::Store(e.to_string())),
        }
    }

    /// Merge `shadow` into the store atomically. On any error the store file
    /// is untouched.
    pub fn merge(&self, shadow: &ShadowState) -> Result<MergeReport, ReplayError> {
        let store_err = |e: std::io::Error| ReplayError::Store(e.to_string());
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(store_err)?;
        }
        let lock_path = self.path.with_extension("lock");
        let _lock = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|e| {
                ReplayError::Store(format!(
                    "could not take store lock {} ({e}); is another replay running?",
                    lock_path.display()
                ))
            })?;
        struct Unlock(PathBuf);
        impl Drop for Unlock {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let _unlock = Unlock(lock_path);

        let existing = self.load()?;
        let in_scope_before = existing
            .iter()
            .filter(|e| {
                e.ledger >= shadow.start_ledger
                    && e.ledger <= shadow.end_ledger
                    && shadow.contracts.contains(&e.contract)
            })
            .count();
        let merged = merge_events(
            existing,
            &shadow.events,
            shadow.start_ledger,
            shadow.end_ledger,
            &shadow.contracts,
        );
        let bytes = serde_json::to_vec(&merged).map_err(|e| ReplayError::Store(e.to_string()))?;
        write_atomic(&self.path, &bytes).map_err(store_err)?;
        Ok(MergeReport {
            replaced: in_scope_before,
            inserted: shadow.events.len(),
            total_after: merged.len(),
        })
    }
}

fn store_path() -> PathBuf {
    PathBuf::from(std::env::var("RWA_EVENT_STORE").unwrap_or_else(|_| "./data/events.json".into()))
}

/// Events persisted by replay, for the indexer to serve. Cached by file
/// modification time so the 10s refresh loop only re-parses after a merge.
pub fn stored_events() -> Option<Vec<Event>> {
    static CACHE: OnceLock<Mutex<Option<(SystemTime, Vec<Event>)>>> = OnceLock::new();
    let path = store_path();
    let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    let mut cache = CACHE.get_or_init(|| Mutex::new(None)).lock().ok()?;
    if let Some((t, ev)) = cache.as_ref() {
        if *t == mtime {
            return Some(ev.clone());
        }
    }
    match FileEventStore::new(path).load() {
        Ok(ev) => {
            *cache = Some((mtime, ev.clone()));
            Some(ev)
        }
        Err(e) => {
            tracing::warn!(error = %e, "ignoring unreadable event store");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Event sources
// ---------------------------------------------------------------------------

#[async_trait]
pub trait EventSource: Send + Sync {
    /// All events for `contracts` in ledgers `start..=end` (paging handled
    /// internally).
    async fn fetch_window(
        &self,
        start: u32,
        end: u32,
        contracts: &[String],
    ) -> Result<Vec<Event>, ReplayError>;
}

/// Soroban RPC `getEvents` client with endpoint failover.
pub struct RpcEventSource {
    http: reqwest::Client,
    urls: Vec<String>,
}

#[derive(Deserialize)]
struct GetEventsResponse {
    result: Option<GetEventsResult>,
    error: Option<RpcErr>,
}
#[derive(Deserialize)]
struct RpcErr {
    message: String,
}
#[derive(Deserialize)]
struct GetEventsResult {
    #[serde(default)]
    events: Vec<RawEvent>,
    #[serde(default)]
    cursor: Option<String>,
}
#[derive(Deserialize)]
pub struct RawEvent {
    pub id: String,
    pub ledger: u32,
    #[serde(rename = "ledgerClosedAt", default)]
    pub ledger_closed_at: Option<String>,
    #[serde(rename = "contractId", default)]
    pub contract_id: Option<String>,
    #[serde(default)]
    pub topic: Vec<String>,
    #[serde(default)]
    pub value: String,
}

impl RpcEventSource {
    pub fn new(urls: Vec<String>) -> Self {
        RpcEventSource {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            urls,
        }
    }

    async fn call(&self, url: &str, params: serde_json::Value) -> Result<GetEventsResult, ReplayError> {
        let transient = |m: String| ReplayError::Rpc { message: m, transient: true };
        let resp = self
            .http
            .post(url)
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getEvents","params":params}))
            .send()
            .await
            .map_err(|e| transient(e.to_string()))?;
        let status = resp.status();
        if status.as_u16() == 429 || status.is_server_error() {
            return Err(transient(format!("http {status}")));
        }
        let body: GetEventsResponse = resp
            .json()
            .await
            .map_err(|e| ReplayError::Rpc { message: e.to_string(), transient: false })?;
        if let Some(e) = body.error {
            // Ledger outside the RPC retention window etc.: retrying won't help.
            return Err(ReplayError::Rpc { message: e.message, transient: false });
        }
        body.result.ok_or(ReplayError::Rpc {
            message: "empty rpc result".into(),
            transient: false,
        })
    }
}

#[async_trait]
impl EventSource for RpcEventSource {
    async fn fetch_window(
        &self,
        start: u32,
        end: u32,
        contracts: &[String],
    ) -> Result<Vec<Event>, ReplayError> {
        let filters: Vec<_> = contracts
            .chunks(5)
            .map(|c| serde_json::json!({"type":"contract","contractIds":c}))
            .collect();
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        let mut url_idx = 0usize;
        loop {
            let params = match &cursor {
                None => serde_json::json!({
                    "startLedger": start,
                    // RPC's endLedger is exclusive.
                    "endLedger": end.saturating_add(1),
                    "filters": filters,
                    "pagination": {"limit": PAGE_LIMIT},
                }),
                Some(c) => serde_json::json!({
                    "filters": filters,
                    "pagination": {"limit": PAGE_LIMIT, "cursor": c},
                }),
            };
            // Fail over across endpoints on transient errors.
            let mut last_err = None;
            let mut page = None;
            for _ in 0..self.urls.len().max(1) {
                let url = &self.urls[url_idx % self.urls.len()];
                match self.call(url, params.clone()).await {
                    Ok(p) => {
                        page = Some(p);
                        break;
                    }
                    Err(e) if e.is_transient() => {
                        url_idx += 1;
                        last_err = Some(e);
                    }
                    Err(e) => return Err(e),
                }
            }
            let Some(page) = page else {
                return Err(last_err.unwrap_or(ReplayError::Rpc {
                    message: "no rpc endpoints".into(),
                    transient: true,
                }));
            };
            let n = page.events.len();
            let mut past_end = false;
            for raw in page.events {
                if raw.ledger > end {
                    past_end = true;
                    continue;
                }
                out.push(decode_event(&raw)?);
            }
            match page.cursor {
                Some(c) if n as u32 >= PAGE_LIMIT && !past_end && cursor.as_deref() != Some(&c) => {
                    cursor = Some(c)
                }
                _ => break,
            }
        }
        Ok(out)
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Decode one RPC event into the API's [`Event`]. The `id` is a stable FNV-1a
/// hash of the RPC event id, so replays are idempotent.
pub fn decode_event(raw: &RawEvent) -> Result<Event, ReplayError> {
    let dec = |b64: &str| -> Result<serde_json::Value, ReplayError> {
        let v = xdr::ScVal::from_xdr_base64(b64, Limits::none())
            .map_err(|e| ReplayError::Decode(e.to_string()))?;
        scval_to_json(&v).map_err(|e| ReplayError::Decode(e.to_string()))
    };
    let topics = raw.topic.iter().map(|t| dec(t)).collect::<Result<Vec<_>, _>>()?;
    let value = if raw.value.is_empty() { serde_json::Value::Null } else { dec(&raw.value)? };
    let event_type = topics
        .first()
        .and_then(|t| t.as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut data = serde_json::json!({ "topics": topics, "value": value });
    if event_type == "transfer" && topics.len() >= 3 {
        data["from"] = topics[1].clone();
        data["to"] = topics[2].clone();
        data["amount"] = value.clone();
    }
    Ok(Event {
        id: fnv1a(&raw.id),
        contract: raw.contract_id.clone().unwrap_or_default(),
        event_type,
        ledger: raw.ledger,
        timestamp: raw.ledger_closed_at.clone(),
        data,
    })
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

async fn fetch_with_retry(
    src: &dyn EventSource,
    start: u32,
    end: u32,
    contracts: &[String],
    base_backoff: Duration,
) -> Result<Vec<Event>, ReplayError> {
    let mut attempt = 0;
    loop {
        match src.fetch_window(start, end, contracts).await {
            Ok(v) => return Ok(v),
            Err(e) if e.is_transient() && attempt + 1 < MAX_ATTEMPTS => {
                let delay = base_backoff * 2u32.pow(attempt);
                tracing::warn!(error = %e, attempt = attempt + 1, ?delay, "replay window failed; retrying");
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

fn checkpoint_path(store: &Path, args: &ReplayArgs) -> PathBuf {
    store.with_file_name(format!(
        "replay-{}-{}.checkpoint.json",
        args.start_ledger, args.end_ledger
    ))
}

/// Run a replay: fetch into a shadow (resuming a checkpoint if one matches),
/// then merge atomically. Returns the final shadow and, unless `dry_run`, the
/// merge report.
pub async fn run_replay(
    args: &ReplayArgs,
    src: &dyn EventSource,
    store: &FileEventStore,
    base_backoff: Duration,
) -> Result<(ShadowState, Option<MergeReport>), ReplayError> {
    let ckpt = checkpoint_path(&store.path, args);
    let mut shadow = fs::read(&ckpt)
        .ok()
        .and_then(|b| serde_json::from_slice::<ShadowState>(&b).ok())
        .filter(|s| s.matches(args) && s.next_ledger >= args.start_ledger)
        .map(|s| {
            tracing::info!(next_ledger = s.next_ledger, "resuming replay from checkpoint");
            s
        })
        .unwrap_or_else(|| ShadowState::new(args));

    let total = (args.end_ledger - args.start_ledger) as u64 + 1;
    let mut progress = Progress::new(total);
    progress.advance((shadow.next_ledger - args.start_ledger) as u64);
    let resumed = progress.done;
    let started = Instant::now();
    tracing::info!(
        start = args.start_ledger, end = args.end_ledger, contracts = args.contracts.len(),
        "replay started (shadow state; live data untouched until merge)"
    );

    while !shadow.is_complete() {
        let w_start = shadow.next_ledger;
        let w_end = w_start.saturating_add(args.window - 1).min(args.end_ledger);
        let events = fetch_with_retry(src, w_start, w_end, &args.contracts, base_backoff).await?;
        shadow.events.extend(events);
        shadow.next_ledger = w_end.saturating_add(1);
        progress.advance((w_end - w_start) as u64 + 1);
        let bytes = serde_json::to_vec(&shadow).map_err(|e| ReplayError::Store(e.to_string()))?;
        write_atomic(&ckpt, &bytes).map_err(|e| ReplayError::Store(e.to_string()))?;
        tracing::info!("{}", progress.report(started.elapsed(), progress.done - resumed).line(shadow.events.len()));
    }

    let report = if args.dry_run {
        tracing::info!(events = shadow.events.len(), "dry run: shadow not merged");
        None
    } else {
        let r = store.merge(&shadow)?;
        tracing::info!(replaced = r.replaced, inserted = r.inserted, total = r.total_after, "shadow merged atomically");
        let _ = fs::remove_file(&ckpt);
        Some(r)
    };
    Ok((shadow, report))
}

/// Entry point for `tessera-api replay ...`; returns the process exit code.
pub async fn run_cli(args: &[String], config: &Config) -> i32 {
    let parsed = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\nusage: tessera-api replay --start-ledger X --end-ledger Y --contracts C1,C2 [--window N] [--dry-run]");
            return 2;
        }
    };
    let src = RpcEventSource::new(config.rpc_urls.clone());
    let store = FileEventStore::from_env();
    match run_replay(&parsed, &src, &store, BASE_BACKOFF).await {
        Ok((shadow, report)) => {
            println!(
                "replay complete: {} events{}",
                shadow.events.len(),
                report.map(|r| format!(", merged (replaced {}, store now {})", r.replaced, r.total_after)).unwrap_or_else(|| " (dry run, not merged)".into())
            );
            0
        }
        Err(e) => {
            eprintln!("replay failed: {e}. The event store was not modified; re-run to resume from the checkpoint.");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const C1: &str = "CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3";
    const C2: &str = "CAR4XY3CEBQWFOL27JEWFW34KXSIZA7RFKDQMEIV7ZU723RWY37I2SYX";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn ev(id: u64, ledger: u32, contract: &str) -> Event {
        Event {
            id,
            contract: contract.into(),
            event_type: "transfer".into(),
            ledger,
            timestamp: None,
            data: serde_json::json!({}),
        }
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tessera-replay-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn parses_cli_arguments() {
        let a = parse_args(&s(&["--start-ledger", "10", "--end-ledger", "20", "--contracts", C2, C1])).unwrap();
        assert_eq!((a.start_ledger, a.end_ledger, a.window), (10, 20, DEFAULT_WINDOW_LEDGERS));
        assert_eq!(a.contracts.len(), 2);
        let b = parse_args(&s(&["--contracts", &format!("{C1},{C1}"), "--start-ledger", "1", "--end-ledger", "1", "--dry-run"])).unwrap();
        assert_eq!(b.contracts, vec![C1.to_string()]);
        assert!(b.dry_run);
    }

    #[test]
    fn rejects_bad_arguments() {
        for bad in [
            s(&["--end-ledger", "5", "--contracts", C1]),
            s(&["--start-ledger", "9", "--end-ledger", "5", "--contracts", C1]),
            s(&["--start-ledger", "0", "--end-ledger", "5", "--contracts", C1]),
            s(&["--start-ledger", "1", "--end-ledger", "5"]),
            s(&["--start-ledger", "1", "--end-ledger", "5", "--contracts", "nope"]),
            s(&["--start-ledger", "x", "--end-ledger", "5", "--contracts", C1]),
            s(&["--start-ledger", "1", "--end-ledger", "5", "--contracts", C1, "--bogus"]),
        ] {
            assert!(parse_args(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn merge_replaces_in_scope_and_keeps_the_rest() {
        let existing = vec![ev(1, 5, C1), ev(2, 10, C1), ev(3, 10, C2), ev(4, 30, C1)];
        let shadow = vec![ev(20, 10, C1), ev(21, 11, C1)];
        let merged = merge_events(existing.clone(), &shadow, 6, 20, &[C1.to_string()]);
        let ids: Vec<u64> = merged.iter().map(|e| e.id).collect();
        // id 2 (in scope) replaced; C2 event, out-of-range events kept.
        assert_eq!(ids, vec![1, 20, 3, 21, 4]);
        // Idempotent.
        assert_eq!(merge_events(merged.clone(), &shadow, 6, 20, &[C1.to_string()]).len(), merged.len());
    }

    #[test]
    fn progress_reports_rate_and_eta() {
        let mut p = Progress::new(1000);
        p.advance(250);
        let r = p.report(Duration::from_secs(10), 250);
        assert_eq!(r.percent, 25.0);
        assert_eq!(r.ledgers_per_sec, 25.0);
        assert_eq!(r.eta, Some(Duration::from_secs(30)));
        assert!(r.line(7).contains("ETA 30s") && r.line(7).contains("25.0%"));
        assert_eq!(Progress::new(10).report(Duration::ZERO, 0).eta, None);
        p.advance(10_000);
        assert_eq!(p.report(Duration::from_secs(1), 1).eta, Some(Duration::ZERO));
    }

    struct FakeSource {
        calls: AtomicU32,
        fail_first: u32,
        fail_after_ledger: Option<u32>,
    }

    #[async_trait]
    impl EventSource for FakeSource {
        async fn fetch_window(&self, start: u32, end: u32, contracts: &[String]) -> Result<Vec<Event>, ReplayError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_first {
                return Err(ReplayError::Rpc { message: "boom".into(), transient: true });
            }
            if self.fail_after_ledger.is_some_and(|l| start > l) {
                return Err(ReplayError::Rpc { message: "gone".into(), transient: false });
            }
            Ok((start..=end)
                .map(|l| ev(l as u64, l, &contracts[0]))
                .collect())
        }
    }

    fn args(start: u32, end: u32, window: u32) -> ReplayArgs {
        ReplayArgs { start_ledger: start, end_ledger: end, contracts: vec![C1.to_string()], window, dry_run: false }
    }

    #[tokio::test]
    async fn replays_in_windows_and_merges_atomically() {
        let dir = tmpdir("ok");
        let store = FileEventStore::new(dir.join("events.json"));
        // Pre-existing data: one in-scope stale event, one outside the range.
        store.merge(&ShadowState { start_ledger: 1, end_ledger: 200, contracts: vec![C1.into()], next_ledger: 201, events: vec![ev(9_999, 105, C1), ev(8_888, 150, C1)] }).unwrap();

        let src = FakeSource { calls: AtomicU32::new(0), fail_first: 0, fail_after_ledger: None };
        let (shadow, report) = run_replay(&args(100, 109, 4), &src, &store, Duration::ZERO).await.unwrap();
        assert_eq!(src.calls.load(Ordering::SeqCst), 3, "10 ledgers / window 4");
        assert_eq!(shadow.events.len(), 10);
        let report = report.unwrap();
        assert_eq!(report.replaced, 1);
        let stored = store.load().unwrap();
        assert_eq!(stored.len(), 11);
        assert!(stored.iter().all(|e| e.id != 9_999), "stale in-range event replaced");
        assert!(stored.iter().any(|e| e.id == 8_888), "out-of-range event kept");
        assert!(!checkpoint_path(&dir.join("events.json"), &args(100, 109, 4)).exists());
        assert!(!dir.join("events.lock").exists());
    }

    #[tokio::test]
    async fn transient_errors_are_retried() {
        let dir = tmpdir("retry");
        let store = FileEventStore::new(dir.join("events.json"));
        let src = FakeSource { calls: AtomicU32::new(0), fail_first: 2, fail_after_ledger: None };
        let (shadow, _) = run_replay(&args(1, 3, 10), &src, &store, Duration::ZERO).await.unwrap();
        assert_eq!(shadow.events.len(), 3);
        assert_eq!(src.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn failure_leaves_store_untouched_and_resumes_from_checkpoint() {
        let dir = tmpdir("resume");
        let path = dir.join("events.json");
        let store = FileEventStore::new(&path);
        store.merge(&ShadowState { start_ledger: 1, end_ledger: 1, contracts: vec![C1.into()], next_ledger: 2, events: vec![ev(1, 1, C1)] }).unwrap();
        let before = fs::read(&path).unwrap();

        // Windows 1-4 and 5-8 succeed, window 9-10 fails permanently.
        let bad = FakeSource { calls: AtomicU32::new(0), fail_first: 0, fail_after_ledger: Some(8) };
        let a = args(100, 109, 5);
        let mut a = a;
        a.start_ledger = 1; a.end_ledger = 10; a.window = 4;
        let err = run_replay(&a, &bad, &store, Duration::ZERO).await.unwrap_err();
        assert!(matches!(err, ReplayError::Rpc { transient: false, .. }));
        assert_eq!(fs::read(&path).unwrap(), before, "store must be untouched after failed replay");

        // Second run with a healthy source resumes at ledger 9.
        let good = FakeSource { calls: AtomicU32::new(0), fail_first: 0, fail_after_ledger: None };
        let (shadow, _) = run_replay(&a, &good, &store, Duration::ZERO).await.unwrap();
        assert_eq!(good.calls.load(Ordering::SeqCst), 1, "only the failed window is refetched");
        assert_eq!(shadow.events.len(), 10);
        assert_eq!(store.load().unwrap().len(), 10);
    }

    #[tokio::test]
    async fn dry_run_does_not_merge() {
        let dir = tmpdir("dry");
        let store = FileEventStore::new(dir.join("events.json"));
        let mut a = args(1, 4, 2);
        a.dry_run = true;
        let src = FakeSource { calls: AtomicU32::new(0), fail_first: 0, fail_after_ledger: None };
        let (shadow, report) = run_replay(&a, &src, &store, Duration::ZERO).await.unwrap();
        assert!(report.is_none());
        assert_eq!(shadow.events.len(), 4);
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn concurrent_merge_is_refused_by_lock() {
        let dir = tmpdir("lock");
        let store = FileEventStore::new(dir.join("events.json"));
        fs::write(dir.join("events.lock"), b"").unwrap();
        let sh = ShadowState { start_ledger: 1, end_ledger: 1, contracts: vec![C1.into()], next_ledger: 2, events: vec![] };
        assert!(matches!(store.merge(&sh), Err(ReplayError::Store(_))));
    }

    #[test]
    fn live_snapshot_merge_is_atomic_swap() {
        let state = AppState::for_test_empty();
        let sh = ShadowState { start_ledger: 1, end_ledger: 9, contracts: vec![C1.into()], next_ledger: 10, events: vec![ev(1, 3, C1)] };
        state.merge_events(&sh);
        assert_eq!(state.snapshot().events.len(), 1);
        state.merge_events(&sh);
        assert_eq!(state.snapshot().events.len(), 1, "idempotent");
    }

    #[test]
    fn decodes_rpc_events_into_api_events() {
        use stellar_xdr::curr::WriteXdr;
        let sym = |x: &str| xdr::ScVal::Symbol(xdr::ScSymbol(x.try_into().unwrap()));
        let acct = |b: u8| xdr::ScVal::Address(xdr::ScAddress::Account(xdr::AccountId(xdr::PublicKey::PublicKeyTypeEd25519(xdr::Uint256([b; 32])))));
        let b64 = |v: &xdr::ScVal| v.to_xdr_base64(Limits::none()).unwrap();
        let raw = RawEvent {
            id: "0000000012345-0000000001".into(),
            ledger: 42,
            ledger_closed_at: Some("2026-01-01T00:00:00Z".into()),
            contract_id: Some(C1.into()),
            topic: vec![b64(&sym("transfer")), b64(&acct(1)), b64(&acct(2))],
            value: b64(&xdr::ScVal::I128(xdr::Int128Parts { hi: 0, lo: 500 })),
        };
        let e = decode_event(&raw).unwrap();
        assert_eq!(e.event_type, "transfer");
        assert_eq!(e.data["amount"], "500");
        assert!(e.data["from"].as_str().unwrap().starts_with('G'));
        assert_eq!(e.id, decode_event(&raw).unwrap().id, "stable id");
        // It feeds the anomaly detector.
        assert!(crate::services::anomaly_detector::observation_from_event(&e).is_some());
    }
}
