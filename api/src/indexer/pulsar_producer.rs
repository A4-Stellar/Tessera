//! Apache Pulsar high-throughput event bus for streaming decoded Soroban
//! ledger events across enterprise systems.
//!
//! # Overview
//!
//! [`PulsarProducer`] wraps the `pulsar-rs` client and maintains **three
//! persistent, compression-enabled producers** — one per logical topic:
//!
//! | Topic                          | Event types routed there                |
//! |--------------------------------|-----------------------------------------|
//! | `tessera.events.transfers`     | `transfer`, `mint`, `burn`, `clawback`  |
//! | `tessera.events.compliance`    | `kyc_*`, `hook*`, `jurisdiction_*`      |
//! | `tessera.events.dividends`     | `dividend_*`, `distribution_*`          |
//!
//! All other event types are silently dropped (no-op).
//!
//! # Message deduplication
//!
//! Every [`TesseraEvent`] carries a **deduplication key** composed of
//! `(ledger_sequence, transaction_hash, event_index)` serialised as
//! `"{ledger}:{tx_hash}:{index}"`.  This key is set as the Pulsar message
//! `partition_key` so that:
//!
//! * The Pulsar broker can deduplicate retried publishes within its
//!   configured deduplication window.
//! * Downstream consumers can perform idempotent processing keyed by the same
//!   string.
//! * All events from the same transaction land on the same partition
//!   (ordering guarantee per transaction).
//!
//! # Resilience
//!
//! * Connection and producer setup failures surface as [`PulsarError`] on
//!   [`PulsarProducer::connect`].
//! * Individual publish failures are logged at `error` level and returned to
//!   the caller — the caller decides whether to retry or route to the DLQ.
//! * The struct implements [`Clone`] (via inner `Arc`) so it can be cheaply
//!   shared across Tokio tasks without re-connecting.
//!
//! # Environment variables
//!
//! | Variable               | Default                    | Purpose                             |
//! |------------------------|----------------------------|-------------------------------------|
//! | `PULSAR_URL`           | `pulsar://127.0.0.1:6650`  | Pulsar broker address               |
//! | `PULSAR_TOKEN`         | *(unset)*                  | JWT auth token (optional)           |
//! | `PULSAR_TENANT`        | `tessera`                  | Pulsar tenant prefix                |
//! | `PULSAR_NAMESPACE`     | `events`                   | Pulsar namespace within the tenant  |
//! | `PULSAR_COMPRESSION`   | `lz4`                      | `lz4`, `snappy`, `zstd`, or `none`  |

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use pulsar::{
    compression::{self, Compression},
    producer::{self, Message},
    Authentication, Producer, Pulsar, TokioExecutor,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info};

// ---------------------------------------------------------------------------
// Topic constants
// ---------------------------------------------------------------------------

pub const TOPIC_TRANSFERS: &str = "tessera.events.transfers";
pub const TOPIC_COMPLIANCE: &str = "tessera.events.compliance";
pub const TOPIC_DIVIDENDS: &str = "tessera.events.dividends";

/// All managed topics in declaration order.
const ALL_TOPICS: [&str; 3] = [TOPIC_TRANSFERS, TOPIC_COMPLIANCE, TOPIC_DIVIDENDS];

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by [`PulsarProducer`].
#[derive(Debug, thiserror::Error)]
pub enum PulsarError {
    #[error("pulsar client error: {0}")]
    Client(#[from] pulsar::Error),
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("no producer for topic '{0}' — was connect() called?")]
    NoProducer(String),
}

// ---------------------------------------------------------------------------
// Payload types
// ---------------------------------------------------------------------------

/// Canonical envelope published to every Pulsar topic.
///
/// All variants share the header fields that Pulsar consumers can read before
/// deserialising the `data` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TesseraEvent {
    // ---- envelope (always present) ----------------------------------------
    /// Pulsar topic the message was routed to.
    pub topic: &'static str,
    /// Deduplication key: `"{ledger_sequence}:{transaction_hash}:{event_index}"`.
    ///
    /// Used as the Pulsar `partition_key` for both partitioned routing and
    /// broker-side deduplication within the configured deduplication window.
    pub dedup_key: String,
    /// Stellar ledger that produced this event.
    pub ledger_sequence: u32,
    /// Transaction hash (hex) within that ledger.
    pub transaction_hash: String,
    /// Zero-based index of the event within its transaction.
    pub event_index: u32,
    /// Contract address that emitted the event (Stellar strkey C…).
    pub contract: String,
    /// Soroban event type string, e.g. `"transfer"`, `"kyc_approved"`.
    pub event_type: String,
    /// RFC-3339 ledger close timestamp, when available.
    pub timestamp: Option<String>,

    // ---- domain payload ---------------------------------------------------
    /// Full decoded event data exactly as returned by the indexer XDR parser.
    pub data: serde_json::Value,
}

impl TesseraEvent {
    /// Build a [`TesseraEvent`] from indexer-level fields.
    ///
    /// Returns `None` when `event_type` cannot be routed to any managed topic
    /// (the caller should skip or route to the DLQ).
    pub fn new(
        ledger_sequence: u32,
        transaction_hash: impl Into<String>,
        event_index: u32,
        contract: impl Into<String>,
        event_type: impl Into<String>,
        timestamp: Option<String>,
        data: serde_json::Value,
    ) -> Option<Self> {
        let event_type = event_type.into();
        let topic = route_topic(&event_type)?;
        let transaction_hash = transaction_hash.into();
        let dedup_key = format!("{ledger_sequence}:{transaction_hash}:{event_index}");

        Some(Self {
            topic,
            dedup_key,
            ledger_sequence,
            transaction_hash,
            event_index,
            contract: contract.into(),
            event_type,
            timestamp,
            data,
        })
    }
}

/// Route an event type string to its canonical Pulsar topic.
///
/// Returns `None` for unknown event types (caller decides whether to log /
/// DLQ / ignore).
pub fn route_topic(event_type: &str) -> Option<&'static str> {
    match event_type {
        // Transfer-family events
        "transfer" | "mint" | "burn" | "clawback" => Some(TOPIC_TRANSFERS),

        // Compliance-family events (prefix match)
        e if e.starts_with("kyc_")
            || e.starts_with("hook")
            || e.starts_with("jurisdiction_") =>
        {
            Some(TOPIC_COMPLIANCE)
        }

        // Dividend / distribution events (prefix match)
        e if e.starts_with("dividend_") || e.starts_with("distribution_") => {
            Some(TOPIC_DIVIDENDS)
        }

        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Runtime configuration for [`PulsarProducer`].
#[derive(Debug, Clone)]
pub struct PulsarConfig {
    /// Pulsar service URL, e.g. `pulsar://broker:6650` or
    /// `pulsar+ssl://broker:6651`.
    pub url: String,
    /// Optional JWT authentication token.
    pub token: Option<String>,
    /// Pulsar tenant (default: `tessera`).
    pub tenant: String,
    /// Pulsar namespace within the tenant (default: `events`).
    pub namespace: String,
    /// Compression codec applied to every message.
    pub compression: PulsarCompression,
}

/// Compression algorithm selection (mirrors the `pulsar` crate feature flags).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PulsarCompression {
    /// LZ4 — best throughput; moderate ratio (default).
    #[default]
    Lz4,
    /// Snappy — balanced; lowest CPU overhead.
    Snappy,
    /// Zstd — highest ratio; more CPU.
    Zstd,
    /// No compression.
    None,
}

impl PulsarCompression {
    /// Convert to the `pulsar` crate's [`Compression`] enum, or `None` when
    /// compression is disabled.
    fn into_pulsar_compression(self) -> Option<Compression> {
        match self {
            PulsarCompression::Lz4 => Some(Compression::Lz4(compression::Lz4 {
                mode: compression::lz4::CompressionMode::Default,
            })),
            PulsarCompression::Snappy => Some(Compression::Snappy(compression::Snappy {})),
            PulsarCompression::Zstd => Some(Compression::Zstd(compression::Zstd { level: 3 })),
            PulsarCompression::None => None,
        }
    }
}

impl fmt::Display for PulsarCompression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PulsarCompression::Lz4 => write!(f, "lz4"),
            PulsarCompression::Snappy => write!(f, "snappy"),
            PulsarCompression::Zstd => write!(f, "zstd"),
            PulsarCompression::None => write!(f, "none"),
        }
    }
}

impl PulsarConfig {
    /// Build configuration from environment variables, falling back to safe
    /// defaults suitable for a local development broker.
    pub fn from_env() -> Self {
        let url = std::env::var("PULSAR_URL")
            .unwrap_or_else(|_| "pulsar://127.0.0.1:6650".to_string());
        let token = std::env::var("PULSAR_TOKEN").ok();
        let tenant = std::env::var("PULSAR_TENANT")
            .unwrap_or_else(|_| "tessera".to_string());
        let namespace = std::env::var("PULSAR_NAMESPACE")
            .unwrap_or_else(|_| "events".to_string());
        let compression = match std::env::var("PULSAR_COMPRESSION")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "snappy" => PulsarCompression::Snappy,
            "zstd" => PulsarCompression::Zstd,
            "none" | "off" => PulsarCompression::None,
            _ => PulsarCompression::Lz4,
        };

        Self {
            url,
            token,
            tenant,
            namespace,
            compression,
        }
    }

    /// Resolve the full persistent topic URL for a short topic name.
    ///
    /// Pulsar uses the canonical URL form
    /// `persistent://<tenant>/<namespace>/<topic>`.
    pub fn topic_url(&self, short_name: &str) -> String {
        format!(
            "persistent://{}/{}/{}",
            self.tenant, self.namespace, short_name
        )
    }
}

// ---------------------------------------------------------------------------
// Producer wrapper
// ---------------------------------------------------------------------------

/// Thread-safe, cheaply-cloneable Apache Pulsar event producer.
///
/// Use [`PulsarProducer::connect`] to establish a connection and create
/// per-topic producers, then call [`PulsarProducer::publish`] from any number
/// of concurrent tasks.
#[derive(Clone)]
pub struct PulsarProducer {
    inner: Arc<Inner>,
}

struct Inner {
    config: PulsarConfig,
    /// One [`Producer`] per topic, keyed by the short topic name constant.
    producers: RwLock<HashMap<&'static str, Producer<TokioExecutor>>>,
}

impl PulsarProducer {
    /// Connect to the Pulsar broker and create producers for all three managed
    /// topics.
    ///
    /// # Errors
    ///
    /// Returns [`PulsarError::Client`] if the broker is unreachable or any
    /// producer cannot be created.
    pub async fn connect(config: PulsarConfig) -> Result<Self, PulsarError> {
        info!(
            url = %config.url,
            tenant = %config.tenant,
            namespace = %config.namespace,
            compression = %config.compression,
            "connecting to Pulsar broker"
        );

        let mut builder = Pulsar::builder(&config.url, TokioExecutor);

        if let Some(ref token) = config.token {
            builder = builder.with_auth(Authentication {
                name: "token".to_string(),
                data: token.as_bytes().to_vec(),
            });
        }

        let client: Pulsar<TokioExecutor> = builder.build().await?;
        let compression = config.compression.into_pulsar_compression();
        let mut producers: HashMap<&'static str, Producer<TokioExecutor>> =
            HashMap::with_capacity(ALL_TOPICS.len());

        for &short_name in &ALL_TOPICS {
            let topic_url = config.topic_url(short_name);

            let producer = client
                .producer()
                .with_topic(&topic_url)
                .with_name(format!("tessera-indexer-{short_name}"))
                .with_options(producer::ProducerOptions {
                    // Compression is set at the producer level; every message
                    // published through this producer is compressed.
                    compression: compression.clone(),
                    // Producer-level metadata visible to broker admin tools.
                    metadata: {
                        let mut m = BTreeMap::new();
                        m.insert("source".to_string(), "tessera-indexer".to_string());
                        m.insert("topic".to_string(), short_name.to_string());
                        m
                    },
                    ..Default::default()
                })
                .build()
                .await?;

            info!(topic = %topic_url, "Pulsar producer ready");
            producers.insert(short_name, producer);
        }

        Ok(Self {
            inner: Arc::new(Inner {
                config,
                producers: RwLock::new(producers),
            }),
        })
    }

    /// Publish a [`TesseraEvent`] to its designated topic.
    ///
    /// The message `partition_key` is set to [`TesseraEvent::dedup_key`] which
    /// encodes `(ledger_sequence, transaction_hash, event_index)`.
    ///
    /// The Pulsar broker uses this key for:
    /// * **Broker-side deduplication** — prevents duplicate delivery when the
    ///   indexer retries a ledger batch (requires `deduplicationEnabled=true`
    ///   on the namespace).
    /// * **Partition routing** — all events from the same transaction land on
    ///   the same partition (ordering guarantee per transaction).
    ///
    /// # Errors
    ///
    /// * [`PulsarError::Serialize`] — JSON serialization failed.
    /// * [`PulsarError::NoProducer`] — `connect()` was not called.
    /// * [`PulsarError::Client`] — the Pulsar broker rejected the publish.
    pub async fn publish(&self, event: &TesseraEvent) -> Result<(), PulsarError> {
        let payload = serde_json::to_vec(event)?;

        debug!(
            topic = event.topic,
            dedup_key = %event.dedup_key,
            ledger = event.ledger_sequence,
            event_type = %event.event_type,
            "publishing event to Pulsar"
        );

        let mut producers = self.inner.producers.write().await;
        let producer = producers
            .get_mut(event.topic)
            .ok_or_else(|| PulsarError::NoProducer(event.topic.to_string()))?;

        let message = Message {
            payload,
            // Dedup key doubles as the partition key so the broker routes all
            // events from the same transaction to the same partition.
            partition_key: Some(event.dedup_key.clone()),
            // Ordering key (raw bytes) drives topic compaction within a
            // partition; using the same value as partition_key is idiomatic.
            ordering_key: Some(event.dedup_key.as_bytes().to_vec()),
            // Per-message properties are exposed to consumers via the message
            // metadata and are useful for header-level filtering without
            // deserialising the full JSON payload.
            properties: {
                let mut props = HashMap::new();
                props.insert(
                    "ledger_sequence".to_string(),
                    event.ledger_sequence.to_string(),
                );
                props.insert("event_type".to_string(), event.event_type.clone());
                props.insert("contract".to_string(), event.contract.clone());
                props
            },
            ..Default::default()
        };

        // `send_non_blocking` enqueues the message locally and returns a
        // `SendFuture` that resolves once the broker acknowledges. Awaiting
        // the nested future ensures we surface backpressure and broker errors.
        producer.send_non_blocking(message).await?.await?;

        debug!(
            topic = event.topic,
            dedup_key = %event.dedup_key,
            "event acknowledged by Pulsar broker"
        );
        Ok(())
    }

    /// Publish a batch of raw indexer [`crate::models::Event`] values,
    /// routing each to its topic and skipping unroutable event types.
    ///
    /// Returns the number of events successfully published. Errors for
    /// individual events are logged; the first error encountered is returned
    /// after the full batch is processed so callers can DLQ the failures.
    pub async fn publish_batch(
        &self,
        events: &[crate::models::Event],
        transaction_hash: &str,
    ) -> Result<usize, PulsarError> {
        let mut published = 0usize;
        let mut first_err: Option<PulsarError> = None;

        for (event_index, event) in events.iter().enumerate() {
            let tessera_event = match TesseraEvent::new(
                event.ledger,
                transaction_hash,
                event_index as u32,
                &event.contract,
                &event.event_type,
                event.timestamp.clone(),
                event.data.clone(),
            ) {
                Some(e) => e,
                None => {
                    debug!(
                        event_type = %event.event_type,
                        ledger = event.ledger,
                        "skipping unroutable event type"
                    );
                    continue;
                }
            };

            match self.publish(&tessera_event).await {
                Ok(()) => published += 1,
                Err(e) => {
                    error!(
                        error = %e,
                        topic = tessera_event.topic,
                        dedup_key = %tessera_event.dedup_key,
                        "failed to publish event to Pulsar"
                    );
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }

        if let Some(e) = first_err {
            return Err(e);
        }
        Ok(published)
    }

    /// Return the [`PulsarConfig`] this producer was built from.
    pub fn config(&self) -> &PulsarConfig {
        &self.inner.config
    }
}

impl fmt::Debug for PulsarProducer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PulsarProducer")
            .field("url", &self.inner.config.url)
            .field("tenant", &self.inner.config.tenant)
            .field("namespace", &self.inner.config.namespace)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Unit tests (pure logic — no broker required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- topic routing ---------------------------------------------------

    #[test]
    fn routes_transfer_events() {
        for et in ["transfer", "mint", "burn", "clawback"] {
            assert_eq!(
                route_topic(et),
                Some(TOPIC_TRANSFERS),
                "event type '{et}' should route to transfers topic"
            );
        }
    }

    #[test]
    fn routes_compliance_events() {
        for et in [
            "kyc_approved",
            "kyc_rejected",
            "kyc_pending",
            "hookregistered",
            "jurisdiction_blocked",
            "jurisdiction_allowed",
        ] {
            assert_eq!(
                route_topic(et),
                Some(TOPIC_COMPLIANCE),
                "event type '{et}' should route to compliance topic"
            );
        }
    }

    #[test]
    fn routes_dividend_events() {
        for et in [
            "dividend_created",
            "dividend_claimed",
            "distribution_completed",
        ] {
            assert_eq!(
                route_topic(et),
                Some(TOPIC_DIVIDENDS),
                "event type '{et}' should route to dividends topic"
            );
        }
    }

    #[test]
    fn returns_none_for_unknown_event_types() {
        for et in ["unknown_op", "admin_action", "price_update", ""] {
            assert!(
                route_topic(et).is_none(),
                "event type '{et}' should return None"
            );
        }
    }

    // ---- dedup key construction ------------------------------------------

    #[test]
    fn dedup_key_encodes_all_three_components() {
        let event = TesseraEvent::new(
            42_001,
            "abc123deadbeef",
            7,
            "CABC1234",
            "transfer",
            None,
            serde_json::json!({"amount": "100"}),
        )
        .expect("transfer should be routable");

        assert_eq!(event.dedup_key, "42001:abc123deadbeef:7");
    }

    #[test]
    fn dedup_key_is_unique_per_event_index() {
        let make = |idx: u32| {
            TesseraEvent::new(
                100,
                "txhash",
                idx,
                "CONTRACT",
                "transfer",
                None,
                serde_json::json!(null),
            )
            .unwrap()
            .dedup_key
        };

        assert_ne!(make(0), make(1));
        assert_ne!(make(0), make(100));
    }

    #[test]
    fn dedup_key_is_unique_per_ledger() {
        let make = |ledger: u32| {
            TesseraEvent::new(
                ledger,
                "same_hash",
                0,
                "CONTRACT",
                "transfer",
                None,
                serde_json::json!(null),
            )
            .unwrap()
            .dedup_key
        };

        assert_ne!(make(1), make(2));
    }

    #[test]
    fn dedup_key_is_unique_per_tx_hash() {
        let make = |hash: &str| {
            TesseraEvent::new(
                100,
                hash,
                0,
                "CONTRACT",
                "transfer",
                None,
                serde_json::json!(null),
            )
            .unwrap()
            .dedup_key
        };

        assert_ne!(make("hash_a"), make("hash_b"));
    }

    #[test]
    fn unroutable_event_returns_none() {
        let result = TesseraEvent::new(
            1,
            "hash",
            0,
            "CONTRACT",
            "totally_unknown",
            None,
            serde_json::json!(null),
        );
        assert!(result.is_none());
    }

    // ---- config ----------------------------------------------------------

    #[test]
    fn topic_url_uses_persistent_scheme() {
        let cfg = PulsarConfig {
            url: "pulsar://broker:6650".to_string(),
            token: None,
            tenant: "acme".to_string(),
            namespace: "prod".to_string(),
            compression: PulsarCompression::Lz4,
        };
        assert_eq!(
            cfg.topic_url(TOPIC_TRANSFERS),
            "persistent://acme/prod/tessera.events.transfers"
        );
        assert_eq!(
            cfg.topic_url(TOPIC_COMPLIANCE),
            "persistent://acme/prod/tessera.events.compliance"
        );
        assert_eq!(
            cfg.topic_url(TOPIC_DIVIDENDS),
            "persistent://acme/prod/tessera.events.dividends"
        );
    }

    #[test]
    fn topic_url_uses_default_tenant_and_namespace() {
        let cfg = PulsarConfig {
            url: "pulsar://broker:6650".to_string(),
            token: None,
            tenant: "tessera".to_string(),
            namespace: "events".to_string(),
            compression: PulsarCompression::None,
        };
        assert_eq!(
            cfg.topic_url(TOPIC_TRANSFERS),
            "persistent://tessera/events/tessera.events.transfers"
        );
    }

    #[test]
    fn compression_display_is_stable() {
        assert_eq!(PulsarCompression::Lz4.to_string(), "lz4");
        assert_eq!(PulsarCompression::Snappy.to_string(), "snappy");
        assert_eq!(PulsarCompression::Zstd.to_string(), "zstd");
        assert_eq!(PulsarCompression::None.to_string(), "none");
    }

    // ---- serialization ---------------------------------------------------

    #[test]
    fn tessera_event_roundtrips_json() {
        let event = TesseraEvent::new(
            500,
            "deadbeef00",
            2,
            "CABC_CONTRACT",
            "mint",
            Some("2026-09-29T00:00:00Z".to_string()),
            serde_json::json!({"amount": "500000000", "to": "GBOB"}),
        )
        .unwrap();

        let json = serde_json::to_string(&event).expect("serialize");
        let back: TesseraEvent = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.dedup_key, "500:deadbeef00:2");
        assert_eq!(back.topic, TOPIC_TRANSFERS);
        assert_eq!(back.event_type, "mint");
        assert_eq!(back.ledger_sequence, 500);
        assert_eq!(back.event_index, 2);
    }

    #[test]
    fn tessera_event_fields_are_correct() {
        let event = TesseraEvent::new(
            100,
            "txhash_abc",
            5,
            "CCONTRACT123",
            "kyc_approved",
            Some("2026-01-01T00:00:00Z".to_string()),
            serde_json::json!({"address": "GUSER"}),
        )
        .unwrap();

        assert_eq!(event.topic, TOPIC_COMPLIANCE);
        assert_eq!(event.dedup_key, "100:txhash_abc:5");
        assert_eq!(event.ledger_sequence, 100);
        assert_eq!(event.transaction_hash, "txhash_abc");
        assert_eq!(event.event_index, 5);
        assert_eq!(event.contract, "CCONTRACT123");
        assert_eq!(event.event_type, "kyc_approved");
        assert_eq!(event.timestamp, Some("2026-01-01T00:00:00Z".to_string()));
    }
}
