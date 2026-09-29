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
//! After a parser fix, `POST /v1/admin/dlq/retry` re-decodes quarantined
//! events: those that now decode are merged into the event store and marked
//! `resolved`, the rest stay quarantined with their attempt count and latest
//! error updated.
//!
//! An event is skipped only after its quarantine row is committed. If the
//! insert fails, the window fails with a transient error and is retried, so
//! no event is ever silently dropped.
//!
//! # Complexity
//!
//! * Quarantine: one upsert on the unique `event_id` index, O(log m) for
//!   `m` rows.
//! * Retry: at most [`RETRY_BATCH`] rows read through the partial
//!   `status = 'quarantined'` index, O(b log m), plus the event-store merge,
//!   O(E log E) for `E` stored events. `FOR UPDATE SKIP LOCKED` lets
//!   concurrent retries split the backlog instead of processing it twice.

use std::sync::Arc;

use serde::Serialize;
use sqlx::{postgres::PgPool, Row};

use super::replay::{decode_event, FileEventStore, RawEvent, ReplayError, ShadowState};

const SCHEMA: &str = include_str!("../db/migrations/0003_failed_events.sql");
/// Upper bound on quarantined events re-processed by one retry call.
const RETRY_BATCH: i64 = 500;

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
}

pub struct DeadLetterQueue {
    pool: PgPool,
}

impl DeadLetterQueue {
    /// Open the queue on the primary database, creating `failed_events` if
    /// needed. Returns `None` (logged) when the database is unreachable, in
    /// which case ingestion keeps failing fast on undecodable events.
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
        .bind(&raw.topic)
        .bind(&raw.value)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use stellar_xdr::curr::{self as xdr, Limits, WriteXdr};

    fn b64(v: xdr::ScVal) -> String {
        v.to_xdr_base64(Limits::none()).unwrap()
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
}
