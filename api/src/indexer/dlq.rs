//! Dead-letter queue for unparseable contract events (issue #163).
//!
//! When Soroban RPC `getEvents` returns an event whose topic or data XDR
//! cannot be decoded, ingestion quarantines it and carries on with the rest
//! of the window instead of failing it. Every quarantine emits a
//! high-priority `ERROR` log carrying the contract ID, the ledger sequence
//! and the raw payload bytes (the base64 `ScVal` XDR as RPC returned it,
//! which `stellar xdr decode --type ScVal --input single-base64` reads as
//! is, up to [`MAX_XDR_BYTES`]).
//!
//! # Backends
//!
//! Quarantine has two interchangeable sinks ([`DlqSink`]):
//!
//! * **Postgres** ([`DeadLetterQueue`]): the `failed_events` table
//!   (migration `0003`, applied idempotently at startup). This is the
//!   primary backend when `RWA_DATABASE_URL` is configured.
//! * **JSON file** ([`FileDlq`]): a lock-guarded, atomically rewritten JSON
//!   store at `RWA_DLQ_STORE` (default `./data/dlq.json`). This is the
//!   fallback when no database is configured or the table cannot be opened,
//!   so an undecodable event never aborts the window for want of Postgres.
//!
//! After a parser fix, `POST /v1/admin/dlq/retry` re-decodes quarantined
//! events: those that now decode are merged into the event store and marked
//! `resolved`, the rest stay quarantined with their attempt count and latest
//! error updated. `GET /v1/admin/dlq` lists the queue for triage, whichever
//! backend is active.
//!
//! An event is skipped only after its quarantine row is committed. If the
//! quarantine fails (e.g. the database is down), the window fails with a
//! transient error and is retried, so no event is ever silently dropped.
//!
//! # Complexity
//!
//! * Quarantine: Postgres — one upsert on the unique `event_id` index,
//!   O(log m) for `m` rows. File — one load + rewrite of the JSON store,
//!   O(m), acceptable for a fallback.
//! * Retry: at most [`RETRY_BATCH`] rows (Postgres through the partial
//!   `status = 'quarantined'` index with `FOR UPDATE SKIP LOCKED` so
//!   concurrent retries split the backlog; file in insertion order), plus
//!   the event-store merge, O(E log E) for `E` stored events.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgPool, Row};

use super::replay::{decode_event, FileEventStore, RawEvent, ReplayError, ShadowState};

const SCHEMA: &str = include_str!("../db/migrations/0003_failed_events.sql");
/// Upper bound on quarantined events re-processed by one retry call.
const RETRY_BATCH: i64 = 500;

/// Where the JSON-file DLQ store lives when `RWA_DLQ_STORE` is unset.
pub const DEFAULT_DLQ_STORE: &str = "./data/dlq.json";

/// Cap on any stored XDR payload (each topic and the event value), in bytes.
/// A hostile or buggy event can carry megabytes of base64; capping keeps one
/// bad event from bloating Postgres or the JSON store. A truncated payload
/// can no longer decode, so a capped entry safely stays quarantined on retry
/// instead of "recovering" a half-event.
const MAX_XDR_BYTES: usize = 2048;

/// Marker appended to any payload that hit [`MAX_XDR_BYTES`].
const TRUNCATED_SUFFIX: &str = "[truncated]";

#[derive(Debug, thiserror::Error)]
pub enum DlqError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Store(#[from] ReplayError),
    #[error("file store error: {0}")]
    File(String),
}

/// Outcome of `POST /v1/admin/dlq/retry`.
#[derive(Debug, Serialize)]
pub struct RetryReport {
    pub retried: usize,
    pub resolved: usize,
    pub still_quarantined: usize,
}

/// One quarantined event, as listed by `GET /v1/admin/dlq` and as persisted
/// by the JSON-file backend. Field names mirror the `failed_events` table so
/// both backends render identically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuarantinedEvent {
    /// RPC event id (TOID + event index); unique key in both backends.
    pub event_id: String,
    /// C... strkey; absent for system events.
    pub contract_id: Option<String>,
    pub ledger_sequence: i64,
    /// ISO-8601, as returned by RPC (Postgres lists it via `::text`).
    pub ledger_closed_at: Option<String>,
    /// Base64 ScVal per topic, capped at [`MAX_XDR_BYTES`] each.
    pub topic_xdr: Vec<String>,
    /// Base64 ScVal event data, capped at [`MAX_XDR_BYTES`].
    pub value_xdr: String,
    /// Most recent decode error.
    pub error: String,
    /// `"quarantined"` or `"resolved"`; resolved events leave both stores.
    pub status: String,
    pub attempts: u32,
    /// RFC-3339 timestamps (file store) or `::text` renderings (Postgres).
    pub first_failed_at: String,
    pub last_failed_at: String,
}

/// High-priority alert for a quarantined event: contract id, ledger sequence
/// and raw payload bytes (issue #163 acceptance criterion 2). Shared by both
/// backends so alerting is identical.
fn log_quarantine(raw: &RawEvent, error: &ReplayError) {
    tracing::error!(
        alert = "dead_letter_queue",
        priority = "high",
        contract_id = raw.contract_id.as_deref().unwrap_or("none"),
        ledger_sequence = raw.ledger,
        event_id = %raw.id,
        topic_xdr = ?cap_topics(&raw.topic),
        value_xdr = %cap_xdr(&raw.value),
        error = %error,
        "unparseable contract event quarantined"
    );
    metrics::counter!("rwa_dlq_quarantined_total").increment(1);
}

/// Cap an oversized XDR payload on a char boundary, marking it truncated.
fn cap_xdr(payload: &str) -> String {
    if payload.len() <= MAX_XDR_BYTES {
        return payload.to_string();
    }
    let mut end = MAX_XDR_BYTES;
    while end > 0 && !payload.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED_SUFFIX}", &payload[..end])
}

fn cap_topics(topics: &[String]) -> Vec<String> {
    topics.iter().map(|t| cap_xdr(t)).collect()
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// Postgres backend (failed_events table)
// ---------------------------------------------------------------------------

pub struct DeadLetterQueue {
    pool: PgPool,
}

impl DeadLetterQueue {
    /// Open the queue on the primary database, creating `failed_events` if
    /// needed. Returns `None` (logged) when the database is unreachable, in
    /// which case callers fall back to [`FileDlq`] instead of dropping
    /// quarantine coverage.
    pub async fn open(pool: PgPool) -> Option<Arc<Self>> {
        match sqlx::raw_sql(SCHEMA).execute(&pool).await {
            Ok(_) => Some(Arc::new(DeadLetterQueue { pool })),
            Err(e) => {
                tracing::error!(error = %e, "dead-letter queue unavailable");
                None
            }
        }
    }

    /// Record `raw` as unparseable and raise a high-priority alert.
    pub async fn quarantine(&self, raw: &RawEvent, error: &ReplayError) -> Result<(), sqlx::Error> {
        log_quarantine(raw, error);

        sqlx::query(
            "INSERT INTO failed_events \
                 (event_id, contract_id, ledger_sequence, ledger_closed_at, topic_xdr, value_xdr, error) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (event_id) DO UPDATE SET \
                 error = EXCLUDED.error, status = 'quarantined', resolved_at = NULL, \
                 attempts = failed_events.attempts + 1, last_failed_at = NOW()",
        )
        .bind(&raw.id)
        .bind(&raw.contract_id)
        .bind(i64::from(raw.ledger))
        .bind(&raw.ledger_closed_at)
        .bind(cap_topics(&raw.topic))
        .bind(cap_xdr(&raw.value))
        .bind(error.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Re-decode up to [`RETRY_BATCH`] quarantined events, oldest first.
    pub async fn retry(&self, store: FileEventStore) -> Result<RetryReport, DlqError> {
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT id, event_id, contract_id, ledger_sequence, ledger_closed_at, topic_xdr, value_xdr \
             FROM failed_events WHERE status = 'quarantined' \
             ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED",
        )
        .bind(RETRY_BATCH)
        .fetch_all(&mut *tx)
        .await?;

        let (mut resolved, mut events) = (Vec::new(), Vec::new());
        let (mut failed, mut errors) = (Vec::new(), Vec::new());
        for row in &rows {
            let id: i64 = row.try_get("id")?;
            let raw = RawEvent {
                id: row.try_get("event_id")?,
                // Written from a u32 by `quarantine`.
                ledger: row.try_get::<i64, _>("ledger_sequence")? as u32,
                ledger_closed_at: row.try_get("ledger_closed_at")?,
                contract_id: row.try_get("contract_id")?,
                topic: row.try_get("topic_xdr")?,
                value: row.try_get("value_xdr")?,
            };
            match decode_event(&raw) {
                Ok(event) => {
                    resolved.push(id);
                    events.push(event);
                }
                Err(e) => {
                    failed.push(id);
                    errors.push(e.to_string());
                }
            }
        }

        if !events.is_empty() {
            // An empty contract scope makes the merge replace nothing: it only
            // adds these events (deduplicated by id) to the store.
            let shadow = ShadowState {
                start_ledger: 0,
                end_ledger: 0,
                contracts: Vec::new(),
                next_ledger: 0,
                events,
            };
            tokio::task::spawn_blocking(move || store.merge(&shadow))
                .await
                .map_err(|e| ReplayError::Store(e.to_string()))??;
            sqlx::query(
                "UPDATE failed_events SET status = 'resolved', resolved_at = NOW() WHERE id = ANY($1)",
            )
            .bind(&resolved)
            .execute(&mut *tx)
            .await?;
        }
        if !failed.is_empty() {
            sqlx::query(
                "UPDATE failed_events AS f \
                 SET attempts = f.attempts + 1, error = u.error, last_failed_at = NOW() \
                 FROM UNNEST($1::BIGINT[], $2::TEXT[]) AS u(id, error) WHERE f.id = u.id",
            )
            .bind(&failed)
            .bind(&errors)
            .execute(&mut *tx)
            .await?;
        }
        // Committing after the store merge is safe: if the commit fails the
        // rows stay quarantined and the next retry's merge is idempotent.
        tx.commit().await?;

        Ok(RetryReport {
            retried: rows.len(),
            resolved: resolved.len(),
            still_quarantined: failed.len(),
        })
    }

    /// List quarantined events for the triage endpoint, oldest first.
    pub async fn list(&self) -> Result<Vec<QuarantinedEvent>, DlqError> {
        // Timestamps are read via `::text` so no extra sqlx feature is needed
        // to decode TIMESTAMPTZ.
        let rows = sqlx::query(
            "SELECT event_id, contract_id, ledger_sequence, \
                    ledger_closed_at::text AS ledger_closed_at, \
                    topic_xdr, value_xdr, error, attempts, \
                    first_failed_at::text AS first_failed_at, \
                    last_failed_at::text AS last_failed_at \
             FROM failed_events WHERE status = 'quarantined' \
             ORDER BY id LIMIT $1",
        )
        .bind(RETRY_BATCH)
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                Ok(QuarantinedEvent {
                    event_id: row.try_get("event_id")?,
                    contract_id: row.try_get("contract_id")?,
                    ledger_sequence: row.try_get("ledger_sequence")?,
                    ledger_closed_at: row.try_get("ledger_closed_at")?,
                    topic_xdr: row.try_get("topic_xdr")?,
                    value_xdr: row.try_get("value_xdr")?,
                    error: row.try_get("error")?,
                    status: "quarantined".into(),
                    attempts: row.try_get::<i32, _>("attempts")?.max(0) as u32,
                    first_failed_at: row.try_get("first_failed_at")?,
                    last_failed_at: row.try_get("last_failed_at")?,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// JSON-file backend (fallback when no database is configured)
// ---------------------------------------------------------------------------

/// Deletes the lock file when dropped, so a panic or early return cannot
/// leave a stale lock wedging later quarantines/retries. Holding the file
/// handle open is deliberate: a second `create_new` on the same path fails
/// while this file exists, which is the mutual exclusion the store needs.
struct Unlock {
    path: PathBuf,
    _file: fs::File,
}

impl Unlock {
    fn acquire(path: &Path) -> Result<Self, DlqError> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| DlqError::File(format!("could not take DLQ lock ({e})")))?;
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

/// JSON-file DLQ store: atomic, lock-guarded, cross-process safe. This is
/// the fallback sink for deployments without a database — the same
/// temp-write + fsync + rename layout the replay event store uses.
pub struct FileDlq {
    path: PathBuf,
}

impl FileDlq {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileDlq { path: path.into() }
    }

    pub fn from_env() -> Self {
        FileDlq::new(
            std::env::var("RWA_DLQ_STORE").unwrap_or_else(|_| DEFAULT_DLQ_STORE.into()),
        )
    }

    /// Load the whole queue. A missing file is an empty queue; a corrupt
    /// store is renamed to `<path>.corrupt` for manual triage and the queue
    /// proceeds empty (a wedged fallback would defeat error isolation).
    fn load(&self) -> Result<Vec<QuarantinedEvent>, DlqError> {
        match fs::read(&self.path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(events) => Ok(events),
                Err(e) => {
                    let backup = self.path.with_extension("corrupt");
                    match fs::rename(&self.path, &backup) {
                        Ok(()) => {
                            tracing::warn!(
                                error = %e,
                                backup = %backup.display(),
                                "DLQ store corrupt; moved aside, continuing with an empty queue"
                            );
                            Ok(Vec::new())
                        }
                        Err(io) => Err(DlqError::File(format!(
                            "corrupt DLQ store {}: {e} (backup failed: {io})",
                            self.path.display()
                        ))),
                    }
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(DlqError::File(e.to_string())),
        }
    }

    /// Replace the queue atomically (temp file, fsync, rename) under a lock
    /// file, so the indexer's quarantine path and a concurrent admin retry
    /// cannot lose each other's updates.
    fn save(&self, events: &[QuarantinedEvent]) -> Result<(), DlqError> {
        let io_err = |e: std::io::Error| DlqError::File(e.to_string());
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(io_err)?;
        }
        let _lock = Unlock::acquire(&self.path.with_extension("lock"))?;

        let bytes = serde_json::to_vec_pretty(events).map_err(|e| DlqError::File(e.to_string()))?;
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp).map_err(io_err)?;
            f.write_all(&bytes).map_err(io_err)?;
            f.sync_all().map_err(io_err)?;
        }
        fs::rename(&tmp, &self.path).map_err(io_err)
    }

    async fn quarantine(&self, raw: &RawEvent, error: &ReplayError) -> Result<(), String> {
        log_quarantine(raw, error);
        let now = rfc3339_now();
        let mut events = self.load().map_err(|e| e.to_string())?;

        // Re-observed event: bump attempts, keep the original first-failure
        // time — mirroring the Postgres upsert.
        let attempts = events
            .iter()
            .find(|e| e.event_id == raw.id)
            .map(|e| e.attempts + 1)
            .unwrap_or(1);
        let first_failed_at = events
            .iter()
            .find(|e| e.event_id == raw.id)
            .map(|e| e.first_failed_at.clone());
        events.retain(|e| e.event_id != raw.id);

        events.push(QuarantinedEvent {
            event_id: raw.id.clone(),
            contract_id: raw.contract_id.clone(),
            ledger_sequence: i64::from(raw.ledger),
            ledger_closed_at: raw.ledger_closed_at.clone(),
            topic_xdr: cap_topics(&raw.topic),
            value_xdr: cap_xdr(&raw.value),
            error: error.to_string(),
            status: "quarantined".into(),
            attempts,
            first_failed_at: first_failed_at.unwrap_or_else(|| now.clone()),
            last_failed_at: now,
        });
        self.save(&events).map_err(|e| e.to_string())
    }

    /// Re-decode up to [`RETRY_BATCH`] quarantined events, in insertion
    /// order. The store merge happens *before* the queue is rewritten: if
    /// the merge fails, the file is untouched; if the rewrite fails after a
    /// successful merge, the next retry re-merges idempotently.
    async fn retry(&self, store: FileEventStore) -> Result<RetryReport, DlqError> {
        let all = self.load()?;
        let (mut batch, rest) = all
            .into_iter()
            .partition::<Vec<_>, _>(|e| e.status == "quarantined");
        batch.truncate(RETRY_BATCH as usize);

        let mut resolved_events = Vec::new();
        let mut failed: Vec<(String, String)> = Vec::new();
        for rec in &batch {
            let raw = RawEvent {
                id: rec.event_id.clone(),
                ledger: u32::try_from(rec.ledger_sequence).unwrap_or(0),
                ledger_closed_at: rec.ledger_closed_at.clone(),
                contract_id: rec.contract_id.clone(),
                topic: rec.topic_xdr.clone(),
                value: rec.value_xdr.clone(),
            };
            match decode_event(&raw) {
                Ok(event) => resolved_events.push(event),
                Err(e) => failed.push((rec.event_id.clone(), e.to_string())),
            }
        }

        if !resolved_events.is_empty() {
            // Empty contract scope: the merge only adds (deduplicates by id).
            let shadow = ShadowState {
                start_ledger: 0,
                end_ledger: 0,
                contracts: Vec::new(),
                next_ledger: 0,
                events: resolved_events,
            };
            tokio::task::spawn_blocking(move || store.merge(&shadow))
                .await
                .map_err(|e| DlqError::Store(ReplayError::Store(e.to_string())))??;
        }

        // Resolved records leave the file; failed ones get attempts+1 and the
        // latest error, mirroring the Postgres UPDATE.
        let now = rfc3339_now();
        let mut remaining = rest;
        for mut rec in batch {
            if let Some((_, err)) = failed.iter().find(|(id, _)| *id == rec.event_id) {
                rec.attempts += 1;
                rec.error = err.clone();
                rec.last_failed_at = now.clone();
                remaining.push(rec);
            }
        }
        self.save(&remaining)?;

        Ok(RetryReport {
            retried: batch.len(),
            resolved: batch.len() - failed.len(),
            still_quarantined: failed.len(),
        })
    }

    fn list(&self) -> Result<Vec<QuarantinedEvent>, DlqError> {
        // Insertion order == oldest first, matching the Postgres backend's
        // `ORDER BY id`.
        Ok(self
            .load()?
            .into_iter()
            .filter(|e| e.status == "quarantined")
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Sink dispatch
// ---------------------------------------------------------------------------

/// The active quarantine backend. Postgres is primary; the JSON file store is
/// the fallback so undecodable events are captured even without a database.
pub enum DlqSink {
    Database(Arc<DeadLetterQueue>),
    File(FileDlq),
}

impl DlqSink {
    /// Record `raw` as unparseable and raise the high-priority alert.
    /// `Err` means the sink could not persist the event; the replay loop
    /// treats that as transient and retries the window rather than dropping
    /// the event.
    pub async fn quarantine(&self, raw: &RawEvent, error: &ReplayError) -> Result<(), String> {
        match self {
            DlqSink::Database(queue) => queue.quarantine(raw, error).await.map_err(|e| e.to_string()),
            DlqSink::File(file) => file.quarantine(raw, error).await,
        }
    }

    /// Re-decode quarantined events; recovered ones are merged into `store`.
    pub async fn retry(&self, store: FileEventStore) -> Result<RetryReport, DlqError> {
        match self {
            DlqSink::Database(queue) => queue.retry(store).await,
            DlqSink::File(file) => file.retry(store).await,
        }
    }

    /// List quarantined events for the triage endpoint.
    pub async fn list(&self) -> Result<Vec<QuarantinedEvent>, DlqError> {
        match self {
            DlqSink::Database(queue) => queue.list().await,
            DlqSink::File(file) => file.list(),
        }
    }

    /// Backend identifier surfaced by `GET /v1/admin/dlq`.
    pub fn backend_name(&self) -> &'static str {
        match self {
            DlqSink::Database(_) => "postgres",
            DlqSink::File(_) => "file",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use stellar_xdr::curr::{self as xdr, Limits, WriteXdr};

    /// Serialize tests that mutate process-global env vars.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn b64(v: xdr::ScVal) -> String {
        v.to_xdr_base64(Limits::none()).unwrap()
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tessera-dlq-{}-{}-{}",
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

    /// Runs against a dedicated test database (it recreates `failed_events`):
    /// `RWA_TEST_DATABASE_URL=postgres://... cargo test dlq -- --ignored`.
    #[tokio::test]
    #[ignore = "requires PostgreSQL via RWA_TEST_DATABASE_URL"]
    async fn quarantined_events_are_retried_after_a_fix() {
        let url = std::env::var("RWA_TEST_DATABASE_URL").expect("RWA_TEST_DATABASE_URL");
        let pool = PgPool::connect(&url).await.unwrap();
        sqlx::query("DROP TABLE IF EXISTS failed_events")
            .execute(&pool)
            .await
            .unwrap();
        let dlq = DeadLetterQueue::open(pool.clone()).await.unwrap();

        let raw = RawEvent {
            id: "0000030064771072-0000000001".into(),
            ledger: 7,
            ledger_closed_at: Some("2026-01-01T00:00:00Z".into()),
            contract_id: Some("CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3".into()),
            topic: vec![b64(xdr::ScVal::Symbol(xdr::ScSymbol(
                "valuation".try_into().unwrap(),
            )))],
            value: "AAAA/w==".into(), // unknown ScVal discriminant
        };
        let error = decode_event(&raw).unwrap_err();
        dlq.quarantine(&raw, &error).await.unwrap();
        dlq.quarantine(&raw, &error).await.unwrap(); // idempotent upsert

        let dir = std::env::temp_dir().join(format!("tessera-dlq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store_path = dir.join("events.json");

        let report = dlq.retry(FileEventStore::new(&store_path)).await.unwrap();
        assert_eq!(
            (report.retried, report.resolved, report.still_quarantined),
            (1, 0, 1)
        );

        // Simulate the parser fix by making the stored payload decodable.
        sqlx::query("UPDATE failed_events SET value_xdr = $1")
            .bind(b64(xdr::ScVal::I128(xdr::Int128Parts {
                hi: 0,
                lo: 12_500,
            })))
            .execute(&pool)
            .await
            .unwrap();
        let report = dlq.retry(FileEventStore::new(&store_path)).await.unwrap();
        assert_eq!(
            (report.retried, report.resolved, report.still_quarantined),
            (1, 1, 0)
        );

        let (status, attempts): (String, i32) =
            sqlx::query_as("SELECT status, attempts FROM failed_events")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((status.as_str(), attempts), ("resolved", 3));

        let stored = FileEventStore::new(&store_path).load().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].event_type, "valuation");
        assert_eq!(stored[0].data["value"], "12500");

        // Nothing left to retry.
        let report = dlq.retry(FileEventStore::new(&store_path)).await.unwrap();
        assert_eq!(report.retried, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The file sink must quarantine, dedupe, retry and recover exactly like
    /// the Postgres backend — it is what stands in when no database exists.
    #[tokio::test]
    async fn file_sink_quarantines_and_retries_after_a_fix() {
        let _env = ENV_LOCK.lock().unwrap();
        let dir = tmpdir("file-sink");
        std::env::set_var("RWA_DLQ_STORE", dir.join("dlq.json"));
        std::env::set_var("RWA_EVENT_STORE", dir.join("events.json"));

        let sink = DlqSink::File(FileDlq::from_env());
        assert_eq!(sink.backend_name(), "file");

        let raw = RawEvent {
            id: "0000030064771072-0000000001".into(),
            ledger: 7,
            ledger_closed_at: Some("2026-01-01T00:00:00Z".into()),
            contract_id: Some("CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3".into()),
            topic: vec![],
            // Decodes as bytes "not-xdr", which is invalid XDR.
            value: "bm90LXhkcg==".into(),
        };
        let error = decode_event(&raw).unwrap_err();
        sink.quarantine(&raw, &error).await.unwrap();
        sink.quarantine(&raw, &error).await.unwrap(); // dedupe: attempts bumps

        let listed = sink.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].attempts, 2);
        assert_eq!(listed[0].status, "quarantined");
        assert_eq!(listed[0].event_id, raw.id);

        let store_path = dir.join("events.json");
        let report = sink.retry(FileEventStore::new(&store_path)).await.unwrap();
        assert_eq!(
            (report.retried, report.resolved, report.still_quarantined),
            (1, 0, 1)
        );
        let listed = sink.list().await.unwrap();
        assert_eq!(listed[0].attempts, 3, "failed retry bumps attempts");

        // Simulate the parser fix by making the stored payload decodable.
        let path = dir.join("dlq.json");
        let mut queue: Vec<QuarantinedEvent> =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        queue[0].value_xdr = b64(xdr::ScVal::I128(xdr::Int128Parts { hi: 0, lo: 12_500 }));
        fs::write(&path, serde_json::to_vec_pretty(&queue).unwrap()).unwrap();

        let report = sink.retry(FileEventStore::new(&store_path)).await.unwrap();
        assert_eq!(
            (report.retried, report.resolved, report.still_quarantined),
            (1, 1, 0)
        );

        let stored = FileEventStore::new(&store_path).load().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].data["value"], "12500");

        assert!(
            sink.list().await.unwrap().is_empty(),
            "resolved events leave the queue"
        );

        std::env::remove_var("RWA_DLQ_STORE");
        std::env::remove_var("RWA_EVENT_STORE");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn file_sink_caps_oversized_payloads() {
        let _env = ENV_LOCK.lock().unwrap();
        let dir = tmpdir("file-cap");
        std::env::set_var("RWA_DLQ_STORE", dir.join("dlq.json"));

        let sink = DlqSink::File(FileDlq::from_env());
        let raw = RawEvent {
            id: "0000030064771072-0000000002".into(),
            ledger: 8,
            ledger_closed_at: None,
            contract_id: None,
            // '%' is outside the base64 alphabet, so decode fails
            // deterministically; 8x the cap exercises truncation.
            topic: vec![format!("%{}", "A".repeat(MAX_XDR_BYTES * 8))],
            value: format!("%{}", "A".repeat(MAX_XDR_BYTES * 8)),
        };
        let error = decode_event(&raw).unwrap_err();
        sink.quarantine(&raw, &error).await.unwrap();

        let listed = sink.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].value_xdr.len() <= MAX_XDR_BYTES + TRUNCATED_SUFFIX.len());
        assert!(listed[0].value_xdr.ends_with(TRUNCATED_SUFFIX));
        assert!(listed[0].topic_xdr[0].ends_with(TRUNCATED_SUFFIX));

        // A truncated payload can never decode: the retry must keep it
        // quarantined, not "recover" a half-event.
        let report = sink
            .retry(FileEventStore::new(dir.join("events.json")))
            .await
            .unwrap();
        assert_eq!(report.still_quarantined, 1);
        assert_eq!(sink.list().await.unwrap().len(), 1);

        std::env::remove_var("RWA_DLQ_STORE");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn file_sink_survives_a_corrupt_store() {
        let _env = ENV_LOCK.lock().unwrap();
        let dir = tmpdir("file-corrupt");
        let path = dir.join("dlq.json");
        std::env::set_var("RWA_DLQ_STORE", &path);
        fs::write(&path, b"{not json").unwrap();

        let sink = DlqSink::File(FileDlq::from_env());
        assert!(sink.list().await.unwrap().is_empty());

        let raw = RawEvent {
            id: "0000030064771072-0000000003".into(),
            ledger: 9,
            ledger_closed_at: None,
            contract_id: None,
            topic: vec![],
            value: "bm90LXhkcg==".into(),
        };
        let error = decode_event(&raw).unwrap_err();
        sink.quarantine(&raw, &error).await.unwrap();
        assert_eq!(sink.list().await.unwrap().len(), 1);
        assert!(
            dir.join("dlq.corrupt").exists(),
            "corrupt store kept aside for triage"
        );

        std::env::remove_var("RWA_DLQ_STORE");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lock_is_released_after_save() {
        let dir = tmpdir("file-unlock");
        let store = FileDlq::new(dir.join("dlq.json"));
        let raw = RawEvent {
            id: "0000030064771072-0000000004".into(),
            ledger: 1,
            ledger_closed_at: None,
            contract_id: None,
            topic: vec![],
            value: "bm90LXhkcg==".into(),
        };
        let error = decode_event(&raw).unwrap_err();
        store.quarantine(&raw, &error).await.unwrap();
        assert!(
            !dir.join("dlq.lock").exists(),
            "lock file must be removed after a successful save"
        );
        // So a second save immediately succeeds (cross-process safety).
        store.quarantine(&raw, &error).await.unwrap();
    }

    #[test]
    fn cap_xdr_truncates_on_char_boundaries() {
        assert_eq!(cap_xdr("short"), "short");
        let big = "é".repeat(MAX_XDR_BYTES); // 2 bytes per char
        let capped = cap_xdr(&big);
        assert!(capped.len() <= MAX_XDR_BYTES + TRUNCATED_SUFFIX.len());
        assert!(capped.ends_with(TRUNCATED_SUFFIX));
    }
}
