//! Redis Streams queue for reliable webhook delivery (issue #279).
//!
//! Jobs are appended with `XADD`, workers in one consumer group pull them with
//! `XREADGROUP`, and each delivery is settled with `XACK`, so queued work
//! survives API restarts and is shared between worker processes. A failed
//! delivery is retried with exponential backoff (same schedule as the
//! in-memory dispatcher) by acknowledging the entry and re-adding it with
//! `attempt + 1`; after [`MAX_ATTEMPTS`] it is moved to [`DEAD_LETTER_STREAM`].
//!
//! This module is the queue only; the HTTP delivery step stays in `webhook`.

// Not yet driven by the dispatcher; kept compiling and tested.
#![allow(dead_code)]

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const STREAM: &str = "tessera:webhooks";
pub const DEAD_LETTER_STREAM: &str = "tessera:webhooks:dead";
pub const GROUP: &str = "webhook-workers";
/// Maximum delivery attempts before a job is dead-lettered.
pub const MAX_ATTEMPTS: u32 = 5;
const BASE_BACKOFF_MS: u64 = 1_000;
const BLOCK_MS: usize = 5_000;

/// `XREADGROUP` reply: per stream, entries of `(id, field -> value)`.
type ReadReply = Option<Vec<(String, Vec<(String, HashMap<String, String>)>)>>;

/// What a stream entry carries: who to call and what to send.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedJob {
    pub endpoint_url: String,
    pub secret_key: String,
    pub event: serde_json::Value,
}

/// An entry read from the stream, not yet acknowledged.
#[derive(Debug, Clone)]
pub struct Delivery {
    /// Stream entry id, needed for `XACK`.
    pub id: String,
    pub job: QueuedJob,
    /// 1-based delivery attempt.
    pub attempt: u32,
}

/// Delay before retrying after delivery attempt `attempt` (1-based) failed:
/// 1 s, 2 s, 4 s, ...
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(BASE_BACKOFF_MS << attempt.saturating_sub(1).min(16))
}

pub struct RedisWebhookQueue {
    client: redis::Client,
}

impl RedisWebhookQueue {
    pub fn new(redis_url: &str) -> Result<Self, String> {
        let client = redis::Client::open(redis_url).map_err(|e| e.to_string())?;
        Ok(Self { client })
    }

    async fn con(&self) -> Result<redis::aio::MultiplexedConnection, String> {
        self.client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| e.to_string())
    }

    /// Create the consumer group (and the stream) if they do not exist yet.
    pub async fn ensure_group(&self) -> Result<(), String> {
        let mut con = self.con().await?;
        let created: Result<String, redis::RedisError> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(STREAM)
            .arg(GROUP)
            .arg("0")
            .arg("MKSTREAM")
            .query_async(&mut con)
            .await;
        match created {
            Err(e) if e.code() != Some("BUSYGROUP") => Err(e.to_string()),
            _ => Ok(()),
        }
    }

    /// `XADD` a job onto the delivery stream.
    pub async fn enqueue(&self, job: &QueuedJob, attempt: u32) -> Result<(), String> {
        self.add(STREAM, job, attempt).await
    }

    async fn add(&self, stream: &str, job: &QueuedJob, attempt: u32) -> Result<(), String> {
        let payload = serde_json::to_string(job).map_err(|e| e.to_string())?;
        let mut con = self.con().await?;
        let _id: String = redis::cmd("XADD")
            .arg(stream)
            .arg("*")
            .arg("payload")
            .arg(payload)
            .arg("attempt")
            .arg(attempt)
            .query_async(&mut con)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// `XREADGROUP` up to `count` new entries for `consumer`, blocking briefly.
    /// Entries that cannot be decoded are skipped.
    pub async fn read(&self, consumer: &str, count: usize) -> Result<Vec<Delivery>, String> {
        let mut con = self.con().await?;
        let reply: ReadReply = redis::cmd("XREADGROUP")
            .arg("GROUP")
            .arg(GROUP)
            .arg(consumer)
            .arg("COUNT")
            .arg(count)
            .arg("BLOCK")
            .arg(BLOCK_MS)
            .arg("STREAMS")
            .arg(STREAM)
            .arg(">")
            .query_async(&mut con)
            .await
            .map_err(|e| e.to_string())?;

        Ok(reply
            .into_iter()
            .flatten()
            .flat_map(|(_stream, entries)| entries)
            .filter_map(|(id, fields)| {
                let job = serde_json::from_str(fields.get("payload")?).ok()?;
                let attempt = fields.get("attempt")?.parse().ok()?;
                Some(Delivery { id, job, attempt })
            })
            .collect())
    }

    /// `XACK` a delivered entry.
    pub async fn ack(&self, id: &str) -> Result<(), String> {
        let mut con = self.con().await?;
        let _acked: u64 = redis::cmd("XACK")
            .arg(STREAM)
            .arg(GROUP)
            .arg(id)
            .query_async(&mut con)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Settle a failed delivery: wait out the backoff and re-add the job with
    /// `attempt + 1`, or move it to the dead-letter stream once attempts are
    /// exhausted; the original entry is acknowledged only after that, so a
    /// crash in between leaves it pending rather than lost.
    pub async fn fail(&self, delivery: &Delivery) -> Result<(), String> {
        if delivery.attempt >= MAX_ATTEMPTS {
            self.add(DEAD_LETTER_STREAM, &delivery.job, delivery.attempt)
                .await?;
        } else {
            tokio::time::sleep(backoff(delivery.attempt)).await;
            self.enqueue(&delivery.job, delivery.attempt + 1).await?;
        }
        self.ack(&delivery.id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_one_second() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(4), Duration::from_secs(8));
    }

    #[test]
    fn queued_job_round_trips_through_json() {
        let job = QueuedJob {
            endpoint_url: "https://example.com/hook".into(),
            secret_key: "s".into(),
            event: serde_json::json!({ "ledger": 10 }),
        };
        let json = serde_json::to_string(&job).unwrap();
        let back: QueuedJob = serde_json::from_str(&json).unwrap();
        assert_eq!(back.endpoint_url, "https://example.com/hook");
        assert_eq!(back.event["ledger"], 10);
    }
}
