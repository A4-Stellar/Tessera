//! Dead-Letter Queue for unparseable contract events (issue #163).
//!
//! When an event payload received from Soroban RPC cannot be decoded —
//! corrupt base64, an unexpected XDR shape, a decode bug — the indexing
//! pipeline must keep going instead of aborting the whole window. This module
//! quarantines the event so a maintainer can inspect it and re-process it
//! later once the parsing bug is fixed.
//!
//! # Flow
//!
//! 1. [`super::replay`]'s `getEvents` loop hits an undecodable event and
//!    calls [`quarantine_failed_decode`], which records it and moves on to
//!    the next event; the window keeps processing (error isolation).
//! 2. The quarantine record is logged at `error` level with the contract id,
//!    ledger sequence and raw payload bytes (acceptance criterion 2), the
//!    `rwa_dlq_events_total` metric is incremented, and the record is
//!    persisted to the JSON DLQ store at `RWA_DLQ_STORE` (default
//!    [`DEFAULT_DLQ_STORE`]) using the same temp-write + fsync + atomic-rename
//!    + lock-file layout as [`super::replay::FileEventStore`] — plus, when a
//!    database is configured via [`init_db_pool`], the `failed_events`
//!    quarantine table (migration `0003`, acceptance criterion 1).
//! 3. `POST /v1/admin/dlq/retry` re-decodes quarantined payloads with the
//!    current (fixed) parser and merges the recovered events into the live
//!    event store (acceptance criterion 3); `GET /v1/admin/dlq` lists the
//!    queue. Both routes are bearer-token gated (see `routes::dlq`).
//!
//! Quarantine is *best effort* by design: a failure to persist a quarantined
//! payload is logged but never propagates, because the whole point of the DLQ
//! is that error isolation must not take the pipeline down.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use stellar_xdr::curr as xdr;
use stellar_xdr::curr::{Limits, ReadXdr};

use super::replay::{self, RawEvent, ReplayError};
use crate::models::Event;

/// Where the DLQ store lives when `RWA_DLQ_STORE` is unset.
pub const DEFAULT_DLQ_STORE: &str = "./data/dlq.json";

/// Largest verbatim payload (`raw_payload`, and each string inside
/// `raw_event`) kept in a quarantine record, in bytes. A hostile or buggy
/// event can carry megabytes of base64; without a cap every re-observation
/// would balloon the JSON store. Truncated base64/XDR cannot decode, so a
/// capped payload safely stays in the queue on retry instead of "recovering"
/// a half-event.
const MAX_PAYLOAD_BYTES: usize = 2048;

/// Marker appended to any payload or raw-event string that was capped.
const TRUNCATED_SUFFIX: &str = "[truncated]";

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// A quarantined event payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuarantinedEvent {
    /// Stable id: FNV-1a of the RPC event id, matching
    /// [`replay::decode_event`], so a retried event lands on the same id in
    /// the event store.
    pub id: u64,
    /// The RPC-level event id (e.g. `"0000000012345-0000000001"`).
    pub source_id: String,
    /// Contract that emitted the event, when known.
    #[serde(default)]
    pub contract_id: String,
    /// Ledger sequence the event was observed at.
    pub ledger: u32,
    /// The complete raw RPC event as received, so retry can re-decode it in
    /// full (topics + value), not just the part that failed.
    pub raw_event: serde_json::Value,
    /// Verbatim base64 XDR bytes of the payload that failed to decode,
    /// capped at [`MAX_PAYLOAD_BYTES`].
    pub raw_payload: String,
    /// Which decoder rejected the payload (e.g. `"topic[1]"`, `"value"`,
    /// `"projection"`).
    pub decode_stage: String,
    /// Error message from the failed decode.
    pub error: String,
    /// Unix timestamp (seconds) of quarantine, for triage ordering.
    pub quarantined_at: u64,
    /// Set once the event decodes successfully on retry.
    #[serde(default)]
    pub resolved: bool,
    /// How many times retry has been attempted.
    #[serde(default)]
    pub retry_count: u32,
}

impl QuarantinedEvent {
    /// Build a quarantine record; `quarantined_at` is captured here.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        source_id: String,
        contract_id: String,
        ledger: u32,
        raw_event: serde_json::Value,
        raw_payload: String,
        decode_stage: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        QuarantinedEvent {
            id,
            source_id,
            contract_id,
            ledger,
            raw_event,
            raw_payload,
            decode_stage: decode_stage.into(),
            error: error.into(),
            quarantined_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            resolved: false,
            retry_count: 0,
        }
    }
}

/// Cap an oversized payload string on a char boundary, marking it truncated.
fn truncate_payload(payload: &str) -> String {
    if payload.len() <= MAX_PAYLOAD_BYTES {
        return payload.to_string();
    }
    let mut end = MAX_PAYLOAD_BYTES;
    while end > 0 && !payload.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED_SUFFIX}", &payload[..end])
}

/// Cap every oversized string inside a quarantine record's raw-event JSON.
/// See [`MAX_PAYLOAD_BYTES`] for why truncated (and therefore undecodable) is
/// the intended outcome for a hostile payload.
fn truncate_json_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) if s.len() > MAX_PAYLOAD_BYTES => {
            *s = truncate_payload(s);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(truncate_json_strings),
        serde_json::Value::Object(map) => map.values_mut().for_each(truncate_json_strings),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// File store (atomic, lock-guarded — mirrors replay::FileEventStore)
// ---------------------------------------------------------------------------

/// DLQ persistence failure. Never propagated into the indexing pipeline; the
/// admin routes map it to `503 Service Unavailable`.
#[derive(Debug, thiserror::Error)]
#[error("dlq store error: {0}")]
pub struct DlqError(pub String);

/// JSON DLQ store with atomic, lock-guarded writes.
pub struct DlqStore {
    path: PathBuf,
}

fn store_path() -> PathBuf {
    PathBuf::from(std::env::var("RWA_DLQ_STORE").unwrap_or_else(|_| DEFAULT_DLQ_STORE.into()))
}

/// Deletes the lock file when dropped, so a panic or early return inside
/// [`DlqStore::save`] cannot leave a stale lock wedging every later retry.
struct Unlock {
    path: PathBuf,
    // Keeping the handle alive is deliberate: a second `create_new` on the
    // same path fails while this file is open, which is exactly the
    // mutual exclusion `save` needs.
    _file: fs::File,
}

impl Unlock {
    fn acquire(path: &Path) -> Result<Self, DlqError> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| {
                DlqError(format!(
                    "could not take DLQ lock {} ({e}); is another retry running?",
                    path.display()
                ))
            })?;
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

impl DlqStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        DlqStore { path: path.into() }
    }

    pub fn from_env() -> Self {
        Self::new(store_path())
    }

    /// Load the full queue. A missing file is an empty queue.
    pub fn load(&self) -> Result<Vec<QuarantinedEvent>, DlqError> {
        match fs::read(&self.path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| {
                DlqError(format!("corrupt DLQ store {}: {e}", self.path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(DlqError(e.to_string())),
        }
    }

    /// Load the queue for a retry run. If the store is corrupt (e.g. a crash
    /// mid-write, or a hand edit), rename it aside to `<path>.corrupt` and
    /// start from an empty queue instead of wedging the retry endpoint
    /// forever; the corrupt file is kept for manual triage.
    fn load_for_retry(&self) -> Result<Vec<QuarantinedEvent>, DlqError> {
        match self.load() {
            Ok(events) => Ok(events),
            Err(e) => {
                let backup = self.path.with_extension("corrupt");
                match fs::rename(&self.path, &backup) {
                    Ok(()) => {
                        tracing::warn!(
                            error = %e,
                            backup = %backup.display(),
                            "DLQ store corrupt; retry proceeds with an empty queue"
                        );
                        Ok(Vec::new())
                    }
                    Err(io) => Err(DlqError(format!("{e} (backup failed: {io})"))),
                }
            }
        }
    }

    /// Replace the queue atomically (temp file, fsync, rename) under a lock
    /// file so concurrent retries cannot lose updates.
    pub fn save(&self, events: &[QuarantinedEvent]) -> Result<(), DlqError> {
        let io_err = |e: std::io::Error| DlqError(e.to_string());
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(io_err)?;
        }
        let lock_path = self.path.with_extension("lock");
        let _lock = Unlock::acquire(&lock_path)?;

        let bytes = serde_json::to_vec_pretty(events).map_err(|e| DlqError(e.to_string()))?;
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp).map_err(io_err)?;
            f.write_all(&bytes).map_err(io_err)?;
            f.sync_all().map_err(io_err)?;
        }
        fs::rename(&tmp, &self.path).map_err(io_err)
    }

    /// Append a quarantine record, flushing to disk immediately. Quarantines
    /// must survive a crash of the indexer, since the payloads are otherwise
    /// unrecoverable.
    pub fn push(&self, event: QuarantinedEvent) -> Result<(), DlqError> {
        let mut all = self.load()?;
        // Dedup: the same RPC event re-observed (e.g. a replay resumed from a
        // checkpoint) should not pile up duplicate rows.
        all.retain(|e| e.id != event.id);
        all.push(event);
        self.save(&all)
    }
}

// ---------------------------------------------------------------------------
// Database mirror
// ---------------------------------------------------------------------------

/// Pool for the `failed_events` quarantine table, installed once at startup
/// by [`init_db_pool`]. `None` when no database is configured: the DLQ then
/// runs on the JSON store alone.
static DB_POOL: OnceLock<sqlx::PgPool> = OnceLock::new();

/// Give the DLQ the database pool used to mirror quarantine records into the
/// `failed_events` table (migration `0003`). Called from `main` when a
/// database is configured; without it the DLQ is JSON-store-only.
pub fn init_db_pool(pool: sqlx::PgPool) {
    let _ = DB_POOL.set(pool);
}

fn db_pool() -> Option<&'static sqlx::PgPool> {
    DB_POOL.get()
}

// ---------------------------------------------------------------------------
// Quarantine + alerting
// ---------------------------------------------------------------------------

/// Identify which stage (topic index, value, or the JSON projection) failed so
/// the quarantine record carries the offending bytes verbatim for triage.
fn quarantine_record(raw: &RawEvent, error: ReplayError) -> QuarantinedEvent {
    let failing = |b64: &str| xdr::ScVal::from_xdr_base64(b64, Limits::none()).is_err();

    let (stage, payload) = if raw.value.is_empty() {
        // `decode_event` maps an empty value to `Null`, so the rejection came
        // from a topic (or the JSON projection below).
        match raw.topic.iter().enumerate().find(|(_, t)| failing(t)) {
            Some((i, topic)) => (format!("topic[{i}]"), truncate_payload(topic)),
            None => (String::from("projection"), String::new()),
        }
    } else if failing(&raw.value) {
        (String::from("value"), truncate_payload(&raw.value))
    } else {
        match raw.topic.iter().enumerate().find(|(_, t)| failing(t)) {
            Some((i, topic)) => (format!("topic[{i}]"), truncate_payload(topic)),
            // Every field decodes as XDR, so the failure was in the JSON
            // projection (`scval_to_json`) or a future decode rule; keep the
            // value bytes as the triage payload.
            None => (String::from("projection"), truncate_payload(&raw.value)),
        }
    };

    let mut record = QuarantinedEvent::new(
        replay::fnv1a(&raw.id),
        raw.id.clone(),
        raw.contract_id.clone().unwrap_or_default(),
        raw.ledger,
        serde_json::to_value(raw).unwrap_or(serde_json::Value::Null),
        payload,
        stage,
        error.to_string(),
    );
    truncate_json_strings(&mut record.raw_event);
    record
}

/// Quarantine an event that [`replay::decode_event`] already rejected
/// (issue #163).
///
/// This is the integration point used by the replay `getEvents` loop: the
/// loop decodes once itself, so quarantining must not decode a second time —
/// good events would pay double.
///
/// Emits a high-priority alert log with the contract id, ledger sequence and
/// raw payload bytes, increments the `rwa_dlq_events_total` metric, and
/// persists the record to the DLQ store and — when a database is configured —
/// the `failed_events` quarantine table. Persistence failures are logged but
/// swallowed: error isolation must never halt the pipeline.
pub async fn quarantine_failed_decode(raw: &RawEvent, error: ReplayError) {
    quarantine(quarantine_record(raw, error)).await;
}

/// Quarantine an event payload (issue #163).
///
/// See [`quarantine_failed_decode`] for the alerting and persistence
/// guarantees.
async fn quarantine(event: QuarantinedEvent) {
    metrics::counter!("rwa_dlq_events_total").increment(1);

    // Acceptance criterion 2: high-priority alert with contract id, ledger
    // sequence and raw payload bytes.
    tracing::error!(
        target: "tessera::dlq",
        event_id = event.id,
        source_id = %event.source_id,
        contract_id = %event.contract_id,
        ledger_sequence = event.ledger,
        raw_payload = %event.raw_payload,
        decode_stage = %event.decode_stage,
        error = %event.error,
        "unparseable event quarantined to DLQ"
    );

    if let Err(e) = DlqStore::from_env().push(event.clone()) {
        tracing::warn!(error = %e, event_id = event.id, "failed to persist DLQ entry");
    }

    record_failed_event(db_pool(), &event).await;
}

/// Best-effort write into the `failed_events` quarantine table (migration
/// `0003`). Called with `None` when no database is configured; errors are
/// logged, never propagated.
async fn record_failed_event(db: Option<&sqlx::PgPool>, event: &QuarantinedEvent) {
    let Some(pool) = db else { return };
    let result = sqlx::query(
        "INSERT INTO failed_events \
             (event_id, source_id, contract_id, ledger_sequence, raw_payload, \
              decode_stage, error, quarantined_at, resolved, retry_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, to_timestamp($8), FALSE, $9) \
         ON CONFLICT (event_id) DO UPDATE SET \
             error = EXCLUDED.error, \
             retry_count = EXCLUDED.retry_count, \
             quarantined_at = EXCLUDED.quarantined_at",
    )
    .bind(event.id as i64)
    .bind(&event.source_id)
    .bind(&event.contract_id)
    .bind(event.ledger as i64)
    .bind(&event.raw_payload)
    .bind(&event.decode_stage)
    .bind(&event.error)
    .bind(event.quarantined_at as f64)
    .bind(event.retry_count as i32)
    .execute(pool)
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, event_id = event.id, "failed to record failed_events row");
    }
}

// ---------------------------------------------------------------------------
// Retry
// ---------------------------------------------------------------------------

/// What the retry endpoint did.
#[derive(Debug, Serialize)]
pub struct RetryReport {
    /// Entries that decoded successfully with the current parser.
    pub recovered: usize,
    /// Entries that still fail to decode (left in the queue).
    pub still_failing: usize,
    /// Whether the recovered events were merged into the event store.
    pub merged: bool,
    /// Number of entries currently left in the DLQ.
    pub remaining: usize,
}

/// Attempt to re-decode quarantined payloads with the current parser
/// (`replay::decode_event`). Successfully recovered events are merged into
/// the live event store via the replay merge machinery; payloads that still
/// fail are kept, with `retry_count` bumped and the latest error recorded.
///
/// When `id` is `Some`, only that entry is retried; otherwise the whole queue.
pub async fn retry(store: &DlqStore, id: Option<u64>) -> Result<RetryReport, DlqError> {
    let queue = store.load_for_retry()?;
    let mut recovered: Vec<Event> = Vec::new();
    let mut still_failing = 0usize;
    let mut remaining = Vec::with_capacity(queue.len());

    for mut entry in queue {
        if id.is_some_and(|id| id != entry.id) {
            remaining.push(entry);
            continue;
        }
        entry.retry_count += 1;
        match serde_json::from_value::<RawEvent>(entry.raw_event.clone()) {
            Ok(raw) => match replay::decode_event(&raw) {
                Ok(event) => {
                    entry.resolved = true;
                    entry.error = String::new();
                    recovered.push(event);
                    mark_resolved(db_pool(), entry.id).await;
                    continue; // resolved entries leave the queue
                }
                Err(e) => {
                    entry.error = e.to_string();
                    still_failing += 1;
                }
            },
            Err(e) => {
                entry.error = format!("quarantine record unreadable: {e}");
                still_failing += 1;
            }
        }
        entry.resolved = false;
        remaining.push(entry);
    }

    // Merge recovered events into the event store *before* saving the queue:
    // if the merge fails we return an error with the queue untouched, so no
    // recovered payload is ever lost.
    let merged = !recovered.is_empty();
    if merged {
        merge_recovered(&recovered)?;
    }
    store.save(&remaining)?;

    Ok(RetryReport {
        recovered: recovered.len(),
        still_failing,
        merged,
        remaining: remaining.len(),
    })
}

/// Merge recovered events into the replay event store so the running API
/// serves them after the next refresh.
///
/// Uses [`replay::FileEventStore::merge`] with an empty contract scope: the
/// scope filter then keeps *every* existing event, and the recovered events
/// are upserted by their `(ledger, id)` key — the same idempotent
/// replace-within-key semantics replay uses for real windows.
fn merge_recovered(recovered: &[Event]) -> Result<(), DlqError> {
    let store = replay::FileEventStore::from_env();
    let shadow = replay::ShadowState {
        start_ledger: 0,
        end_ledger: 0,
        contracts: Vec::new(),
        next_ledger: 0,
        events: recovered.to_vec(),
    };
    store.merge(&shadow).map(|_| ()).map_err(|e| DlqError(e.to_string()))
}

/// Mark a `failed_events` row resolved after a successful retry. Best effort.
async fn mark_resolved(db: Option<&sqlx::PgPool>, event_id: u64) {
    let Some(pool) = db else { return };
    if let Err(e) = sqlx::query("UPDATE failed_events SET resolved = TRUE WHERE event_id = $1")
        .bind(event_id as i64)
        .execute(pool)
        .await
    {
        tracing::warn!(error = %e, event_id, "failed to mark failed_events row resolved");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use stellar_xdr::curr::WriteXdr;

    const CONTRACT: &str = "CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3";

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tessera-dlq-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(id: u64) -> QuarantinedEvent {
        QuarantinedEvent::new(
            id,
            format!("0000000012345-{id:010}"),
            CONTRACT.into(),
            12345,
            serde_json::json!({"id": format!("0000000012345-{id:010}"), "ledger": 12345}),
            "bm90LXhkcg==".into(),
            "value",
            "xdr error: unexpected end of XDR input",
        )
    }

    fn raw_event(id: &str, ledger: u32, topic: Vec<String>, value: String) -> RawEvent {
        RawEvent {
            id: id.into(),
            ledger,
            ledger_closed_at: None,
            contract_id: Some(CONTRACT.into()),
            topic,
            value,
        }
    }

    /// Serialize tests that mutate process-global env vars (RWA_DLQ_STORE,
    /// RWA_EVENT_STORE): cargo runs a binary's tests in parallel threads.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn push_dedups_by_id_and_persists() {
        let dir = tmpdir("push");
        let store = DlqStore::new(dir.join("dlq.json"));

        store.push(sample(1)).unwrap();
        // Same id again: replaces, not duplicates.
        store.push(sample(1)).unwrap();
        store.push(sample(2)).unwrap();

        let all = store.load().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, 1);
        assert_eq!(all[1].id, 2);
    }

    #[test]
    fn missing_store_is_empty_queue() {
        let dir = tmpdir("empty");
        let store = DlqStore::new(dir.join("nonexistent.json"));
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn quarantine_record_captures_triage_fields() {
        let e = sample(7);
        assert_eq!(e.retry_count, 0);
        assert!(!e.resolved);
        assert!(e.quarantined_at > 0);
        assert!(e.source_id.starts_with("0000000012345-"));
        assert_eq!(e.raw_payload, "bm90LXhkcg==");
    }

    #[test]
    fn quarantine_record_captures_failing_topic() {
        let good_topic = xdr::ScVal::Symbol("transfer".try_into().unwrap())
            .to_xdr_base64(Limits::none())
            .unwrap();
        let raw = raw_event(
            "0000000012345-0000000003",
            12345,
            vec![good_topic, "%%%not-base64".into()],
            xdr::ScVal::U32(1).to_xdr_base64(Limits::none()).unwrap(),
        );
        let record = quarantine_record(&raw, ReplayError::Decode("bad topic".into()));
        assert_eq!(record.decode_stage, "topic[1]");
        assert_eq!(record.raw_payload, "%%%not-base64");
        assert_eq!(record.id, replay::fnv1a(&raw.id));
    }

    #[test]
    fn quarantine_record_with_empty_value_checks_topics() {
        let raw = raw_event("0000000012345-0000000004", 1, vec!["%%%".into()], String::new());
        let record = quarantine_record(&raw, ReplayError::Decode("bad".into()));
        assert_eq!(record.decode_stage, "topic[0]");
        assert_eq!(record.raw_payload, "%%%");
    }

    #[test]
    fn quarantine_record_labels_projection_failures() {
        // Every field decodes as XDR (so no topic/value stage matches) yet
        // `decode_event` rejected the event — e.g. a JSON-projection failure.
        // The record must fall back to the `projection` stage and keep the
        // value bytes for triage.
        let raw = raw_event(
            "0000000012345-0000000005",
            1,
            vec![],
            xdr::ScVal::U32(9).to_xdr_base64(Limits::none()).unwrap(),
        );
        let record = quarantine_record(&raw, ReplayError::Decode("projection failed".into()));
        assert_eq!(record.decode_stage, "projection");
        assert_eq!(record.raw_payload, raw.value);
        assert_eq!(record.raw_event["id"].as_str(), Some(raw.id.as_str()));
    }

    #[test]
    fn quarantine_record_caps_hostile_payload_sizes() {
        // '%' is outside the base64 alphabet, so the payload fails to decode
        // deterministically regardless of XDR shape.
        let big = format!("%{}", "A".repeat(MAX_PAYLOAD_BYTES * 8));
        let raw = raw_event("0000000012345-0000000006", 1, vec![big.clone()], big);
        let record = quarantine_record(&raw, ReplayError::Decode("big".into()));

        // Both the triage payload and the embedded raw event are capped, and
        // the caps are marked so operators can tell truncation from data.
        assert!(record.raw_payload.len() <= MAX_PAYLOAD_BYTES + TRUNCATED_SUFFIX.len());
        assert!(record.raw_payload.ends_with(TRUNCATED_SUFFIX));
        let raw_event_value = record.raw_event["value"].as_str().unwrap();
        assert!(raw_event_value.len() <= MAX_PAYLOAD_BYTES + TRUNCATED_SUFFIX.len());
        assert!(raw_event_value.ends_with(TRUNCATED_SUFFIX));
    }

    #[test]
    fn lock_is_released_after_save() {
        let dir = tmpdir("unlock");
        let store = DlqStore::new(dir.join("dlq.json"));
        store.push(sample(1)).unwrap();
        assert!(
            !dir.join("dlq.lock").exists(),
            "lock file must be removed after a successful save"
        );
        // So a second save immediately succeeds.
        store.push(sample(2)).unwrap();
        assert_eq!(store.load().unwrap().len(), 2);
    }

    #[test]
    fn concurrent_save_is_refused_by_lock() {
        let dir = tmpdir("lock");
        let store = DlqStore::new(dir.join("dlq.json"));
        store.push(sample(1)).unwrap();
        // Take the lock ourselves; the next save must refuse.
        fs::File::create(dir.join("dlq.lock")).unwrap();
        assert!(store.push(sample(2)).is_err());
        // Original entry untouched.
        assert_eq!(store.load().unwrap()[0].id, 1);
    }

    #[tokio::test]
    async fn quarantine_survives_unwritable_store() {
        // Point RWA_DLQ_STORE at a path whose parent is a file, so pushes
        // fail; quarantine must still return without panicking (error
        // isolation is the whole point of the DLQ).
        let dir = tmpdir("unwritable");
        let blocker = dir.join("blocker");
        fs::write(&blocker, b"x").unwrap();
        // SAFETY: test-only env mutation, serialized by ENV_LOCK.
        let _env = ENV_LOCK.lock().unwrap();
        std::env::set_var("RWA_DLQ_STORE", blocker.join("dlq.json"));
        quarantine(sample(3)).await;
        std::env::remove_var("RWA_DLQ_STORE");
    }

    #[tokio::test]
    async fn retry_recovers_events_once_parser_supports_them() {
        let _env = ENV_LOCK.lock().unwrap();
        let dir = tmpdir("retry");
        let store = DlqStore::new(dir.join("dlq.json"));
        std::env::set_var("RWA_DLQ_STORE", dir.join("dlq.json"));
        std::env::set_var("RWA_EVENT_STORE", dir.join("events.json"));

        // A genuinely valid ScVal payload — quarantineable only by a fake
        // "stage" label; retry must decode it and merge it into the store.
        let value = xdr::ScVal::I128(xdr::Int128Parts { hi: 0, lo: 500 });
        let b64 = value.to_xdr_base64(Limits::none()).unwrap();
        let raw = raw_event("0000000012345-0000000042", 12345, Vec::new(), b64.clone());
        let record = quarantine_record(&raw, ReplayError::Decode("forced".into()));
        // The payload is valid XDR, so the fake label falls through to the
        // projection stage — exactly the "parser regression" shape.
        assert_eq!(record.decode_stage, "projection");
        assert_eq!(record.raw_payload, b64);
        store.push(record).unwrap();

        let report = retry(&store, None).await.unwrap();
        assert_eq!(report.recovered, 1);
        assert_eq!(report.remaining, 0);
        assert!(report.merged);

        let events = replay::FileEventStore::from_env().load().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].ledger, 12345);
        assert!(store.load().unwrap().is_empty());

        std::env::remove_var("RWA_DLQ_STORE");
        std::env::remove_var("RWA_EVENT_STORE");
    }

    #[tokio::test]
    async fn retry_keeps_still_failing_entries_and_bumps_count() {
        let _env = ENV_LOCK.lock().unwrap();
        let dir = tmpdir("retry-fail");
        let store = DlqStore::new(dir.join("dlq.json"));
        std::env::set_var("RWA_DLQ_STORE", dir.join("dlq.json"));
        std::env::set_var("RWA_EVENT_STORE", dir.join("events.json"));

        let mut record = sample(9);
        record.raw_event = serde_json::json!({
            "id": "0000000012345-0000000009",
            "ledger": 12345,
            "contractId": CONTRACT,
            "topic": [],
            "value": "bm90LXhkcg==", // "not-xdr"
        });
        store.push(record).unwrap();

        let report = retry(&store, None).await.unwrap();
        assert_eq!(report.recovered, 0);
        assert_eq!(report.still_failing, 1);
        assert_eq!(report.remaining, 1);
        assert!(!report.merged);

        let left = store.load().unwrap();
        assert_eq!(left[0].retry_count, 1);
        assert!(!left[0].resolved);

        std::env::remove_var("RWA_DLQ_STORE");
        std::env::remove_var("RWA_EVENT_STORE");
    }

    #[tokio::test]
    async fn retry_recovers_from_a_corrupt_store() {
        let dir = tmpdir("corrupt");
        let store = DlqStore::new(dir.join("dlq.json"));
        fs::write(dir.join("dlq.json"), b"{not json").unwrap();

        let report = retry(&store, None).await.unwrap();
        assert_eq!(report.recovered, 0);
        assert_eq!(report.remaining, 0);
        assert!(
            dir.join("dlq.corrupt").exists(),
            "corrupt store kept aside for triage"
        );
        assert!(store.load().unwrap().is_empty());
    }

    #[tokio::test]
    async fn quarantine_failed_decode_isolates_bad_events() {
        let _env = ENV_LOCK.lock().unwrap();
        let dir = tmpdir("isolate");
        std::env::set_var("RWA_DLQ_STORE", dir.join("dlq.json"));
        std::env::set_var("RWA_EVENT_STORE", dir.join("events.json"));

        let good = raw_event(
            "0000000012345-0000000001",
            100,
            Vec::new(),
            xdr::ScVal::U32(7).to_xdr_base64(Limits::none()).unwrap(),
        );
        let mut bad = raw_event(
            "0000000012345-0000000002",
            100,
            Vec::new(),
            String::new(),
        );
        bad.value = "%%%not-base64".into();

        // The good event decodes; the bad one quarantines and names its stage.
        assert!(replay::decode_event(&good).is_ok());
        let err = match replay::decode_event(&bad) {
            Ok(_) => panic!("bad event should not decode"),
            Err(e) => e,
        };
        quarantine_failed_decode(&bad, err).await;

        let queue = DlqStore::from_env().load().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].ledger, 100);
        assert_eq!(queue[0].decode_stage, "value");
        assert_eq!(queue[0].raw_payload, "%%%not-base64");

        std::env::remove_var("RWA_DLQ_STORE");
        std::env::remove_var("RWA_EVENT_STORE");
    }
}
