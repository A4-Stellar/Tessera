//! Dead-letter queue for unparseable contract events (issue #163).
//!
//! When Soroban RPC `getEvents` returns an event whose topic or data XDR
//! cannot be decoded, ingestion quarantines it in the `failed_events` table
//! and carries on with the rest of the window instead of failing it. Every
//! quarantine emits a high-priority `ERROR` log carrying the contract ID, the
//! ledger sequence and the raw payload bytes (the base64 `ScVal` XDR exactly
//! as RPC returned it, which `stellar xdr decode --type ScVal --input single-base64`
//! reads as is).
//!
//! Retryable rows are scheduled with a bounded backoff and can be claimed by
//! concurrent workers using `FOR UPDATE SKIP LOCKED`. After five failed
//! attempts a row is terminally quarantined. After a parser fix,
//! `POST /v1/admin/dlq/retry` can retry due rows or explicitly selected IDs.
//!
//! An event is skipped only after its quarantine row is committed. If the
//! insert fails, the window fails with a transient error and is retried, so
//! no event is ever silently dropped.
//!
//! # Complexity
//!
//! * Quarantine: one upsert on the unique `event_id` index, O(log m) for
//!   `m` rows.
//! * Retry: at most [`RETRY_BATCH`] rows read through the partial due-row
//!   index, O(b log m), plus the event-store merge,
//!   O(E log E) for `E` stored events. `FOR UPDATE SKIP LOCKED` lets
//!   concurrent retries split the backlog instead of processing it twice.

use std::{sync::Arc, time::Duration};

use serde::Serialize;
use sqlx::{postgres::PgPool, Row};
use tokio::sync::{watch, RwLock};

use super::pulsar_producer::{PoisonPill, PulsarProducer};
use super::replay::{decode_event, FileEventStore, RawEvent, ReplayError, ShadowState};

const SCHEMA: &str = include_str!("../db/migrations/0003_failed_events.sql");
/// Upper bound on due rows claimed in one worker poll/manual request.
pub const RETRY_BATCH: i64 = 100;
pub const MAX_ATTEMPTS: i32 = 5;
const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(1);
const RETRY_DELAYS_SECS: [i64; 5] = [5, 30, 120, 600, 3600];

fn retry_delay_seconds(attempt: i32) -> i64 {
    RETRY_DELAYS_SECS[attempt.saturating_sub(1).clamp(0, 4) as usize]
}

fn is_poisoned(attempts: i32) -> bool {
    attempts >= MAX_ATTEMPTS
}

#[derive(Debug, thiserror::Error)]
pub enum DlqError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Store(#[from] ReplayError),
}

/// Outcome of `POST /v1/admin/dlq/retry`.
#[derive(Debug, Serialize)]
pub struct RetryReport {
    pub retried: usize,
    pub resolved: usize,
    pub still_quarantined: usize,
    pub poisoned: usize,
}

pub struct DeadLetterQueue {
    pool: PgPool,
    pulsar: RwLock<Option<PulsarProducer>>,
}

impl DeadLetterQueue {
    /// Open the queue on the primary database, creating `failed_events` if
    /// needed. Returns `None` (logged) when the database is unreachable, in
    /// which case ingestion keeps failing fast on undecodable events.
    pub async fn open(pool: PgPool) -> Option<Arc<Self>> {
        match sqlx::raw_sql(SCHEMA).execute(&pool).await {
            Ok(_) => {
                if let Err(e) = update_quarantined_gauge(&pool).await {
                    tracing::warn!(error = %e, "could not initialize DLQ quarantine gauge");
                }
                Some(Arc::new(DeadLetterQueue {
                    pool,
                    pulsar: RwLock::new(None),
                }))
            }
            Err(e) => {
                tracing::error!(error = %e, "dead-letter queue unavailable");
                None
            }
        }
    }

    /// Record `raw` as unparseable and raise a high-priority alert.
    pub async fn quarantine(&self, raw: &RawEvent, error: &ReplayError) -> Result<(), sqlx::Error> {
        tracing::error!(
            alert = "dead_letter_queue",
            priority = "high",
            contract_id = raw.contract_id.as_deref().unwrap_or("none"),
            ledger_sequence = raw.ledger,
            event_id = %raw.id,
            topic_xdr = ?raw.topic,
            value_xdr = %raw.value,
            error = %error,
            "unparseable contract event quarantined"
        );
        metrics::counter!("rwa_dlq_quarantined_total").increment(1);

        let (is_poisoned, attempts): (bool, i32) = sqlx::query_as(
            "INSERT INTO failed_events \
                 (event_id, contract_id, ledger_sequence, ledger_closed_at, topic_xdr, value_xdr, error, retry_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW() + INTERVAL '5 seconds') \
             ON CONFLICT (event_id) DO UPDATE SET \
                 error = EXCLUDED.error, status = 'quarantined', resolved_at = NULL, \
                 attempts = failed_events.attempts + 1, last_failed_at = NOW(), \
                 quarantined = failed_events.attempts + 1 >= 5, \
                 poison_pill_published = CASE WHEN failed_events.status = 'resolved' \
                     THEN FALSE ELSE failed_events.poison_pill_published END, \
                 poison_pill_published_at = CASE WHEN failed_events.status = 'resolved' \
                     THEN NULL ELSE failed_events.poison_pill_published_at END, \
                 retry_at = CASE WHEN failed_events.attempts + 1 >= 5 THEN NULL \
                     ELSE NOW() + CASE LEAST(failed_events.attempts + 1, 5) \
                         WHEN 1 THEN INTERVAL '5 seconds' \
                         WHEN 2 THEN INTERVAL '30 seconds' \
                         WHEN 3 THEN INTERVAL '2 minutes' \
                         WHEN 4 THEN INTERVAL '10 minutes' \
                         ELSE INTERVAL '1 hour' END END \
             RETURNING quarantined, attempts",
        )
        .bind(&raw.id)
        .bind(&raw.contract_id)
        .bind(i64::from(raw.ledger))
        .bind(&raw.ledger_closed_at)
        .bind(&raw.topic)
        .bind(&raw.value)
        .bind(error.to_string())
        .fetch_one(&self.pool)
        .await?;
        if is_poisoned {
            if attempts == MAX_ATTEMPTS {
                metrics::counter!("tessera_dlq_poison_pills_total").increment(1);
            }
            if let Err(e) = update_quarantined_gauge(&self.pool).await {
                tracing::warn!(error = %e, "could not update DLQ quarantine gauge");
            }
        }
        Ok(())
    }

    /// Re-decode up to [`RETRY_BATCH`] due rows, oldest first.
    pub async fn retry(&self, store: FileEventStore) -> Result<RetryReport, DlqError> {
        self.retry_inner(store, None).await
    }

    /// Manually retry the requested event IDs, ignoring their scheduled
    /// backoff and restarting the attempt budget for terminal rows.
    pub async fn retry_selected(
        &self,
        store: FileEventStore,
        event_ids: &[String],
    ) -> Result<RetryReport, DlqError> {
        self.retry_inner(store, Some(event_ids)).await
    }

    /// Run the worker poll loop until the supplied shutdown signal is set.
    pub async fn run_worker(
        self: Arc<Self>,
        store: FileEventStore,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut interval = tokio::time::interval(WORKER_POLL_INTERVAL);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(error) = self.retry(store.clone()).await {
                        tracing::error!(error = %error, "dead-letter worker poll failed");
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    }

    /// Run the worker with a connected producer for poison-pill publication.
    /// Existing callers of [`Self::run_worker`] remain valid, while the
    /// service startup path should prefer this variant.
    pub async fn run_worker_with_pulsar(
        self: Arc<Self>,
        store: FileEventStore,
        pulsar: PulsarProducer,
        shutdown: watch::Receiver<bool>,
    ) {
        self.set_pulsar(pulsar).await;
        self.run_worker(store, shutdown).await;
    }

    pub async fn set_pulsar(&self, pulsar: PulsarProducer) {
        *self.pulsar.write().await = Some(pulsar);
    }

    async fn retry_inner(
        &self,
        store: FileEventStore,
        selected_ids: Option<&[String]>,
    ) -> Result<RetryReport, DlqError> {
        let mut tx = self.pool.begin().await?;

        let rows = if let Some(event_ids) = selected_ids {
            sqlx::query(
                "UPDATE failed_events SET attempts = 0, quarantined = FALSE, retry_at = NOW(), \
                 poison_pill_published = FALSE, poison_pill_published_at = NULL \
                 WHERE event_id = ANY($1) AND status = 'quarantined' AND quarantined",
            )
            .bind(event_ids)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "SELECT id, event_id, contract_id, ledger_sequence, ledger_closed_at, topic_xdr, value_xdr, attempts \
                 FROM failed_events WHERE event_id = ANY($1) AND status = 'quarantined' \
                   AND NOT quarantined \
                 ORDER BY id LIMIT $2 FOR UPDATE SKIP LOCKED",
            )
            .bind(event_ids)
            .bind(RETRY_BATCH)
            .fetch_all(&mut *tx)
            .await?
        } else {
            sqlx::query(
                "SELECT id, event_id, contract_id, ledger_sequence, ledger_closed_at, topic_xdr, value_xdr, attempts \
                 FROM failed_events WHERE status = 'quarantined' AND NOT quarantined \
                   AND retry_at <= NOW() \
                 ORDER BY retry_at, id LIMIT $1 FOR UPDATE SKIP LOCKED",
            )
            .bind(RETRY_BATCH)
            .fetch_all(&mut *tx)
            .await?
        };

        let (mut resolved, mut resolved_attempts, mut events) =
            (Vec::new(), Vec::new(), Vec::new());
        let (mut failed, mut attempts, mut delays, mut errors) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for row in &rows {
            let id: i64 = row.try_get("id")?;
            let event_id: String = row.try_get("event_id")?;
            let attempt: i32 = row.try_get::<i32, _>("attempts")?.saturating_add(1);
            let raw = RawEvent {
                id: event_id.clone(),
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
                    resolved_attempts.push(attempt);
                    events.push(event);
                }
                Err(e) => {
                    failed.push(id);
                    attempts.push(attempt);
                    delays.push(retry_delay_seconds(attempt));
                    let error = e.to_string();
                    errors.push(error.clone());
                    if is_poisoned(attempt) {
                        tracing::error!(
                            alert = "dead_letter_queue_poison_pill",
                            event_id = %event_id,
                            attempts = attempt,
                            error = %error,
                            "dead-letter event reached the retry limit"
                        );
                    }
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
                "UPDATE failed_events AS f \
                 SET status = 'resolved', resolved_at = NOW(), retry_at = NULL, \
                     quarantined = FALSE, attempts = u.attempts \
                 FROM UNNEST($1::BIGINT[], $2::INTEGER[]) AS u(id, attempts) \
                 WHERE f.id = u.id",
            )
            .bind(&resolved)
            .bind(&resolved_attempts)
            .execute(&mut *tx)
            .await?;
        }
        if !failed.is_empty() {
            sqlx::query(
                "UPDATE failed_events AS f \
                 SET attempts = u.attempts, error = u.error, last_failed_at = NOW(), \
                     quarantined = u.attempts >= $5, \
                     retry_at = CASE WHEN u.attempts >= $5 THEN NULL ELSE \
                         NOW() + u.delay_seconds * INTERVAL '1 second' END \
                 FROM UNNEST($1::BIGINT[], $2::INTEGER[], $3::BIGINT[], $4::TEXT[]) \
                     AS u(id, attempts, delay_seconds, error) \
                 WHERE f.id = u.id",
            )
            .bind(&failed)
            .bind(&attempts)
            .bind(&delays)
            .bind(&errors)
            .bind(MAX_ATTEMPTS)
            .execute(&mut *tx)
            .await?;
        }
        // Committing after the store merge is safe: if the commit fails the
        // rows stay quarantined and the next retry's merge is idempotent.
        tx.commit().await?;

        if poisoned_attempt_count(&attempts) > 0 {
            metrics::counter!("tessera_dlq_poison_pills_total")
                .increment(poisoned_attempt_count(&attempts) as u64);
        }
        self.publish_pending_poison_pills().await?;

        let poisoned = poisoned_attempt_count(&attempts);
        if poisoned > 0 || selected_ids.is_some() {
            if let Err(e) = update_quarantined_gauge(&self.pool).await {
                tracing::warn!(error = %e, "could not update DLQ quarantine gauge");
            }
        }

        Ok(RetryReport {
            retried: rows.len(),
            resolved: resolved.len(),
            still_quarantined: failed.len(),
            poisoned,
        })
    }

    async fn publish_pending_poison_pills(&self) -> Result<(), DlqError> {
        let Some(pulsar) = self.pulsar.read().await.clone() else {
            return Ok(());
        };
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT id, event_id, ledger_sequence, contract_id, topic_xdr, value_xdr, error, attempts \
             FROM failed_events \
             WHERE quarantined = TRUE AND poison_pill_published = FALSE \
             ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED",
        )
        .bind(RETRY_BATCH)
        .fetch_all(&mut *tx)
        .await?;

        for row in rows {
            let id: i64 = row.try_get("id")?;
            let poison = PoisonPill {
                message_type: "poison_pill".into(),
                event_id: row.try_get("event_id")?,
                ledger_sequence: row.try_get("ledger_sequence")?,
                contract_id: row.try_get("contract_id")?,
                topic_xdr: row.try_get("topic_xdr")?,
                value_xdr: row.try_get("value_xdr")?,
                error: row.try_get("error")?,
                attempts: row.try_get("attempts")?,
            };
            match pulsar.publish_poison_pill(&poison).await {
                Ok(()) => {
                    sqlx::query(
                        "UPDATE failed_events SET poison_pill_published = TRUE, \
                         poison_pill_published_at = NOW() WHERE id = $1",
                    )
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                    metrics::counter!("tessera_dlq_poison_pills_published_total").increment(1);
                }
                Err(error) => {
                    metrics::counter!("tessera_dlq_poison_pill_publish_failures_total")
                        .increment(1);
                    tracing::error!(
                        event_id = %poison.event_id,
                        error = %error,
                        "failed to publish poison pill; it remains pending"
                    );
                }
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

fn poisoned_attempt_count(attempts: &[i32]) -> usize {
    attempts.iter().filter(|attempt| is_poisoned(**attempt)).count()
}

async fn update_quarantined_gauge(pool: &PgPool) -> Result<(), sqlx::Error> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM failed_events WHERE quarantined = TRUE")
            .fetch_one(pool)
            .await?;
    metrics::gauge!("tessera_dlq_quarantined_total").set(count as f64);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stellar_xdr::curr::{self as xdr, Limits, WriteXdr};

    fn b64(v: xdr::ScVal) -> String {
        v.to_xdr_base64(Limits::none()).unwrap()
    }

    #[test]
    fn retry_backoff_uses_the_configured_intervals() {
        assert_eq!(
            (1..=5).map(retry_delay_seconds).collect::<Vec<_>>(),
            vec![5, 30, 120, 600, 3600]
        );
        assert_eq!(retry_delay_seconds(0), 5);
        assert_eq!(retry_delay_seconds(6), 3600);
    }

    #[test]
    fn poison_pill_threshold_is_five_failures() {
        assert!(!is_poisoned(MAX_ATTEMPTS - 1));
        assert!(is_poisoned(MAX_ATTEMPTS));
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

        sqlx::query("UPDATE failed_events SET retry_at = NOW()")
            .execute(&pool)
            .await
            .unwrap();
        let report = dlq.retry(FileEventStore::new(&store_path)).await.unwrap();
        assert_eq!(
            (report.retried, report.resolved, report.still_quarantined),
            (1, 0, 1)
        );

        for expected_attempt in [4, 5] {
            sqlx::query("UPDATE failed_events SET retry_at = NOW()")
                .execute(&pool)
                .await
                .unwrap();
            let report = dlq.retry(FileEventStore::new(&store_path)).await.unwrap();
            assert_eq!(report.retried, 1);
            assert_eq!(report.still_quarantined, 1);
            assert_eq!(report.poisoned, if expected_attempt == 5 { 1 } else { 0 });
        }
        let (attempts, quarantined, retry_at): (i32, bool, Option<String>) =
            sqlx::query_as("SELECT attempts, quarantined, retry_at::TEXT FROM failed_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((attempts, quarantined), (5, true));
        assert!(retry_at.is_none());

        // Simulate the parser fix by making the stored payload decodable.
        sqlx::query("UPDATE failed_events SET value_xdr = $1")
            .bind(b64(xdr::ScVal::I128(xdr::Int128Parts {
                hi: 0,
                lo: 12_500,
            })))
            .execute(&pool)
            .await
            .unwrap();
        let report = dlq
            .retry_selected(FileEventStore::new(&store_path), &[raw.id.clone()])
            .await
            .unwrap();
        assert_eq!(
            (report.retried, report.resolved, report.still_quarantined),
            (1, 1, 0)
        );

        let (status, attempts): (String, i32) =
            sqlx::query_as("SELECT status, attempts FROM failed_events")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((status.as_str(), attempts), ("resolved", 1));

        let stored = FileEventStore::new(&store_path).load().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].event_type, "valuation");
        assert_eq!(stored[0].data["value"], "12500");

        // Nothing left to retry.
        let report = dlq.retry(FileEventStore::new(&store_path)).await.unwrap();
        assert_eq!(report.retried, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
