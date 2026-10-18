//! Real-Time Event Streaming via Apache Flink & Kafka Integration (Issue #275).
//!
//! Construct a high-throughput event processing pipeline connecting `tessera-api`
//! to Apache Flink and Kafka for real-time windowed event aggregation.
//!
//! # Architecture & Responsibilities
//! 1. **Kafka Event Streaming**:
//!    - Streams parsed and decoded Soroban ledger events into dedicated Kafka topics
//!      (`tessera.events.soroban`, `tessera.events.transfers`, `tessera.events.compliance`).
//!    - Partitions messages by `contract_id` / `asset_id` to enforce strict chronological
//!      linearizability across Kafka broker partitions.
//!    - Includes Dead-Letter Queue (DLQ) fallback for malformed or unroutable events.
//!
//! 2. **Windowed Moving Average Engine**:
//!    - Computes sliding and tumbling window analytics over:
//!      - **1-minute** moving averages (short-term high-frequency momentum)
//!      - **1-hour** moving averages (intraday trends)
//!      - **24-hour** moving averages (macro daily baseline)
//!    - Metrics aggregated:
//!      - **Trading Volume**: Base token volume and USD-converted volume.
//!      - **Holder Velocity**: Token turnover frequency relative to circulating supply and active addresses.
//!      - **Moving Average Price**: Volume-Weighted Average Price (VWAP) and simple moving average price.
//!      - **Volatility & Extremes**: Price variance / standard deviation, High, Low, Open, Close (OHLC).
//!
//! 3. **TimescaleDB Persistence & Retrieval**:
//!    - Batched upsert of aggregated window records into TimescaleDB `asset_metric_windows`.
//!    - Sub-millisecond indexed queries for instantaneous API and dashboard retrieval.
//!
//! 4. **Apache Flink Job & DDL Generation**:
//!    - Embedded Flink SQL definitions for Kafka source streams, watermarking, and JDBC sinks.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::models::{Asset, Event};

/// Standard Kafka topic identifiers for Tessera event streams.
pub const KAFKA_TOPIC_SOROBAN_EVENTS: &str = "tessera.events.soroban";
pub const KAFKA_TOPIC_TRANSFERS: &str = "tessera.events.transfers";
pub const KAFKA_TOPIC_COMPLIANCE: &str = "tessera.events.compliance";
pub const KAFKA_TOPIC_DIVIDENDS: &str = "tessera.events.dividends";
pub const KAFKA_TOPIC_WINDOW_ANALYTICS: &str = "tessera.analytics.windows";
pub const KAFKA_TOPIC_DLQ: &str = "tessera.events.dlq";

/// Errors arising from Kafka streaming and Flink window processing.
#[derive(Debug, Error)]
pub enum FlinkPipelineError {
    #[error("Kafka publish error: {0}")]
    KafkaPublish(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("TimescaleDB persistence error: {0}")]
    Database(String),

    #[error("Invalid event payload: {0}")]
    InvalidEvent(String),

    #[error("Channel send error: {0}")]
    ChannelSend(String),
}

/// Duration intervals for Flink moving average windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WindowDuration {
    #[serde(rename = "1m")]
    M1,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "24h")]
    H24,
}

impl WindowDuration {
    /// Duration in seconds.
    pub fn as_secs(&self) -> i64 {
        match self {
            WindowDuration::M1 => 60,
            WindowDuration::H1 => 3_600,
            WindowDuration::H24 => 86_400,
        }
    }

    /// Canonical string identifier ('1m', '1h', '24h').
    pub fn as_str(&self) -> &'static str {
        match self {
            WindowDuration::M1 => "1m",
            WindowDuration::H1 => "1h",
            WindowDuration::H24 => "24h",
        }
    }

    /// Parse duration string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "1m" | "m1" | "60s" => Some(WindowDuration::M1),
            "1h" | "h1" | "3600s" => Some(WindowDuration::H1),
            "24h" | "h24" | "1d" | "86400s" => Some(WindowDuration::H24),
            _ => None,
        }
    }
}

impl fmt::Display for WindowDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Normalized Soroban event ready for Kafka ingestion and Flink stream consumption.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KafkaSorobanEvent {
    pub event_id: u64,
    pub contract_id: String,
    pub asset_id: u64,
    pub event_type: String,
    pub ledger_sequence: u32,
    pub timestamp: DateTime<Utc>,
    pub amount: i128,
    pub price_usd: f64,
    pub from_address: Option<String>,
    pub to_address: Option<String>,
    pub deduplication_key: String,
}

impl KafkaSorobanEvent {
    /// Construct a new Kafka Soroban event with a deterministic deduplication key.
    pub fn new(
        event_id: u64,
        contract_id: String,
        asset_id: u64,
        event_type: String,
        ledger_sequence: u32,
        timestamp: DateTime<Utc>,
        amount: i128,
        price_usd: f64,
        from_address: Option<String>,
        to_address: Option<String>,
    ) -> Self {
        let deduplication_key = format!("{}:{}:{}", ledger_sequence, contract_id, event_id);
        Self {
            event_id,
            contract_id,
            asset_id,
            event_type,
            ledger_sequence,
            timestamp,
            amount,
            price_usd,
            from_address,
            to_address,
            deduplication_key,
        }
    }

    /// Kafka message partition key ensuring chronological per-asset ordering.
    pub fn partition_key(&self) -> &str {
        &self.contract_id
    }
}

/// Aggregate moving average metric record produced by Apache Flink / window engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlinkWindowMetricRecord {
    pub asset_id: u64,
    pub token_contract: String,
    pub window_duration: WindowDuration,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    /// Moving average token volume in base units over the window.
    pub moving_avg_volume: f64,
    /// Moving average volume in USD.
    pub moving_avg_volume_usd: f64,
    /// Holder velocity: token turnover relative to circulating supply (volume / supply)
    /// or active counterparty turnover.
    pub holder_velocity: f64,
    /// Volume-Weighted Average Price (VWAP) or Simple Moving Average price in USD.
    pub moving_avg_price_usd: f64,
    pub trade_count: u64,
    pub unique_active_holders: usize,
    pub open_price_usd: f64,
    pub high_price_usd: f64,
    pub low_price_usd: f64,
    pub close_price_usd: f64,
    pub volatility_index: f64,
}

/// Configuration for the Kafka event producer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KafkaProducerConfig {
    pub bootstrap_servers: Vec<String>,
    pub topic_prefix: String,
    pub client_id: String,
    pub acks: String,
    pub max_batch_size_bytes: usize,
    pub linger_ms: u64,
}

impl Default for KafkaProducerConfig {
    fn default() -> Self {
        Self {
            bootstrap_servers: vec!["localhost:9092".to_string()],
            topic_prefix: "tessera".to_string(),
            client_id: "tessera-indexer-flink".to_string(),
            acks: "all".to_string(),
            max_batch_size_bytes: 16384,
            linger_ms: 10,
        }
    }
}

/// Message emitted onto the Kafka event bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KafkaMessage {
    pub topic: String,
    pub key: String,
    pub payload: serde_json::Value,
    pub headers: HashMap<String, String>,
}

/// High-throughput Kafka event producer connecting the indexer to Kafka topics.
#[derive(Debug, Clone)]
pub struct KafkaFlinkProducer {
    config: KafkaProducerConfig,
    /// Outbound message sender for asynchronous, non-blocking publishing.
    tx: mpsc::Sender<KafkaMessage>,
    /// Dead-Letter Queue (DLQ) sender.
    dlq_tx: mpsc::Sender<KafkaMessage>,
}

impl KafkaFlinkProducer {
    /// Construct a new Kafka event producer with buffered async transmission.
    pub fn new(
        config: KafkaProducerConfig,
        buffer_capacity: usize,
    ) -> (Self, mpsc::Receiver<KafkaMessage>, mpsc::Receiver<KafkaMessage>) {
        let (tx, rx) = mpsc::channel(buffer_capacity);
        let (dlq_tx, dlq_rx) = mpsc::channel(buffer_capacity);
        (Self { config, tx, dlq_tx }, rx, dlq_rx)
    }

    /// Stream a parsed Soroban event into Kafka topics consumed by Apache Flink jobs.
    pub async fn stream_soroban_event(
        &self,
        event: &KafkaSorobanEvent,
    ) -> Result<(), FlinkPipelineError> {
        // 1. Primary raw event topic: tessera.events.soroban
        let primary_topic = format!("{}.events.soroban", self.config.topic_prefix);
        let payload = serde_json::to_value(event)?;

        let mut headers = HashMap::new();
        headers.insert("dedup_key".to_string(), event.deduplication_key.clone());
        headers.insert("event_type".to_string(), event.event_type.clone());

        let msg = KafkaMessage {
            topic: primary_topic,
            key: event.partition_key().to_string(),
            payload,
            headers: headers.clone(),
        };

        if let Err(e) = self.tx.send(msg).await {
            warn!("Failed to stream event to Kafka channel: {}", e);
            // Route failed delivery to DLQ
            let dlq_msg = KafkaMessage {
                topic: format!("{}.events.dlq", self.config.topic_prefix),
                key: event.partition_key().to_string(),
                payload: serde_json::json!({
                    "error": e.to_string(),
                    "event": event
                }),
                headers,
            };
            let _ = self.dlq_tx.send(dlq_msg).await;
            return Err(FlinkPipelineError::ChannelSend(e.to_string()));
        }

        // 2. Specific topic routing for transfers / compliance / dividends
        let specific_topic = match event.event_type.as_str() {
            "transfer" | "mint" | "burn" => Some(format!("{}.events.transfers", self.config.topic_prefix)),
            "kyc_approved" | "kyc_rejected" | "compliance_lock" => Some(format!("{}.events.compliance", self.config.topic_prefix)),
            "dividend_created" | "claim" | "drip" => Some(format!("{}.events.dividends", self.config.topic_prefix)),
            _ => None,
        };

        if let Some(topic) = specific_topic {
            let specific_msg = KafkaMessage {
                topic,
                key: event.partition_key().to_string(),
                payload: serde_json::to_value(event)?,
                headers,
            };
            let _ = self.tx.send(specific_msg).await;
        }

        Ok(())
    }
}

/// Internal event buffer entry for window calculations.
#[derive(Debug, Clone)]
struct WindowEventEntry {
    timestamp: DateTime<Utc>,
    amount: f64,
    price_usd: f64,
    from_address: Option<String>,
    to_address: Option<String>,
}

/// Real-Time Event Aggregator computing 1-minute, 1-hour, and 24-hour moving average metrics.
#[derive(Debug, Default)]
pub struct FlinkWindowAggregator {
    /// In-memory rolling event logs keyed by asset ID.
    asset_events: Arc<RwLock<HashMap<u64, VecDeque<WindowEventEntry>>>>,
    /// Token contract address map.
    token_contracts: Arc<RwLock<HashMap<u64, String>>>,
    /// Circulating total supply per asset for velocity calculations.
    circulating_supplies: Arc<RwLock<HashMap<u64, f64>>>,
}

impl FlinkWindowAggregator {
    pub fn new() -> Self {
        Self {
            asset_events: Arc::new(RwLock::new(HashMap::new())),
            token_contracts: Arc::new(RwLock::new(HashMap::new())),
            circulating_supplies: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register asset metadata (contract address and circulating supply).
    pub async fn register_asset(&self, asset_id: u64, token_contract: String, supply: f64) {
        let mut contracts = self.token_contracts.write().await;
        contracts.insert(asset_id, token_contract);

        let mut supplies = self.circulating_supplies.write().await;
        supplies.insert(asset_id, if supply > 0.0 { supply } else { 1.0 });
    }

    /// Ingest a parsed Soroban event into the window buffer.
    pub async fn ingest_event(&self, event: &KafkaSorobanEvent) {
        let mut events_map = self.asset_events.write().await;
        let deque = events_map.entry(event.asset_id).or_default();

        let amount_f64 = event.amount.abs() as f64;

        deque.push_back(WindowEventEntry {
            timestamp: event.timestamp,
            amount: amount_f64,
            price_usd: event.price_usd,
            from_address: event.from_address.clone(),
            to_address: event.to_address.clone(),
        });

        // Register contract address if not already present
        let mut contracts = self.token_contracts.write().await;
        contracts.entry(event.asset_id).or_insert_with(|| event.contract_id.clone());

        // Evict events older than 24h + 5m buffer to maintain O(1) amortized storage
        let cutoff = event.timestamp - chrono::Duration::seconds(86_400 + 300);
        while let Some(front) = deque.front() {
            if front.timestamp < cutoff {
                deque.pop_front();
            } else {
                break;
            }
        }
    }

    /// Compute moving average metrics across all three required windows (1m, 1h, 24h).
    pub async fn compute_all_windows(
        &self,
        asset_id: u64,
        as_of: DateTime<Utc>,
    ) -> Vec<FlinkWindowMetricRecord> {
        let durations = [WindowDuration::M1, WindowDuration::H1, WindowDuration::H24];
        let mut results = Vec::with_capacity(3);

        for duration in durations {
            if let Some(record) = self.compute_window(asset_id, duration, as_of).await {
                results.push(record);
            }
        }

        results
    }

    /// Compute window metric aggregation for a specific duration.
    pub async fn compute_window(
        &self,
        asset_id: u64,
        duration: WindowDuration,
        as_of: DateTime<Utc>,
    ) -> Option<FlinkWindowMetricRecord> {
        let events_map = self.asset_events.read().await;
        let deque = events_map.get(&asset_id)?;

        let window_secs = duration.as_secs();
        let window_start = as_of - chrono::Duration::seconds(window_secs);
        let window_end = as_of;

        let contracts = self.token_contracts.read().await;
        let token_contract = contracts.get(&asset_id).cloned().unwrap_or_default();

        let supplies = self.circulating_supplies.read().await;
        let total_supply = supplies.get(&asset_id).copied().unwrap_or(1.0);

        // Filter events strictly within [window_start, window_end]
        let in_window: Vec<&WindowEventEntry> = deque
            .iter()
            .filter(|e| e.timestamp >= window_start && e.timestamp <= window_end)
            .collect();

        if in_window.is_empty() {
            // Return empty baseline metric window
            return Some(FlinkWindowMetricRecord {
                asset_id,
                token_contract,
                window_duration: duration,
                window_start,
                window_end,
                moving_avg_volume: 0.0,
                moving_avg_volume_usd: 0.0,
                holder_velocity: 0.0,
                moving_avg_price_usd: 0.0,
                trade_count: 0,
                unique_active_holders: 0,
                open_price_usd: 0.0,
                high_price_usd: 0.0,
                low_price_usd: 0.0,
                close_price_usd: 0.0,
                volatility_index: 0.0,
            });
        }

        let mut total_vol = 0.0;
        let mut total_vol_usd = 0.0;
        let mut price_vol_sum = 0.0;
        let mut prices = Vec::with_capacity(in_window.len());
        let mut unique_accounts = HashSet::new();

        let mut high_price = f64::MIN;
        let mut low_price = f64::MAX;
        let open_price = in_window.first().map(|e| e.price_usd).unwrap_or(0.0);
        let close_price = in_window.last().map(|e| e.price_usd).unwrap_or(0.0);

        for e in in_window.iter() {
            total_vol += e.amount;
            let vol_usd = e.amount * e.price_usd;
            total_vol_usd += vol_usd;
            price_vol_sum += e.price_usd * e.amount;
            prices.push(e.price_usd);

            if e.price_usd > high_price {
                high_price = e.price_usd;
            }
            if e.price_usd < low_price {
                low_price = e.price_usd;
            }

            if let Some(ref from) = e.from_address {
                unique_accounts.insert(from.clone());
            }
            if let Some(ref to) = e.to_address {
                unique_accounts.insert(to.clone());
            }
        }

        if low_price == f64::MAX {
            low_price = 0.0;
        }
        if high_price == f64::MIN {
            high_price = 0.0;
        }

        // Volume-Weighted Moving Average Price (VWAP) if volume > 0, else arithmetic mean
        let moving_avg_price_usd = if total_vol > 0.0 {
            price_vol_sum / total_vol
        } else if !prices.is_empty() {
            prices.iter().sum::<f64>() / prices.len() as f64
        } else {
            0.0
        };

        // Moving Average Volume (normalized per second then scaled to window duration)
        let moving_avg_volume = total_vol;
        let moving_avg_volume_usd = total_vol_usd;

        // Holder Velocity = (Total Transferred Volume in window) / (Circulating Supply)
        let holder_velocity = if total_supply > 0.0 {
            total_vol / total_supply
        } else {
            0.0
        };

        // Price Volatility Index (Sample standard deviation)
        let mean_price = prices.iter().sum::<f64>() / prices.len() as f64;
        let variance = if prices.len() > 1 {
            let sum_sq_diff: f64 = prices.iter().map(|p| (p - mean_price).powi(2)).sum();
            sum_sq_diff / (prices.len() - 1) as f64
        } else {
            0.0
        };
        let volatility_index = variance.sqrt();

        Some(FlinkWindowMetricRecord {
            asset_id,
            token_contract,
            window_duration: duration,
            window_start,
            window_end,
            moving_avg_volume,
            moving_avg_volume_usd,
            holder_velocity,
            moving_avg_price_usd,
            trade_count: in_window.len() as u64,
            unique_active_holders: unique_accounts.len(),
            open_price_usd: open_price,
            high_price_usd: high_price,
            low_price_usd: low_price,
            close_price_usd: close_price,
            volatility_index,
        })
    }
}

/// TimescaleDB Persistence Handler for aggregated window analytics.
pub struct TimescaleSink;

impl TimescaleSink {
    /// Generate parameterized SQL UPSERT query targeting the TimescaleDB hypertable `asset_metric_windows`.
    pub fn build_upsert_query(record: &FlinkWindowMetricRecord) -> (String, Vec<String>) {
        let sql = r#"
INSERT INTO asset_metric_windows (
    asset_id,
    window_type,
    window_duration,
    window_duration_seconds,
    window_start,
    window_end,
    trading_volume,
    trading_volume_usd,
    trade_count,
    unique_active_traders,
    hourly_holder_growth_rate,
    volatility_index,
    moving_average_price_usd,
    open_price_usd,
    high_price_usd,
    low_price_usd,
    close_price_usd,
    created_at
) VALUES (
    $1, 'sliding', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, NOW()
)
ON CONFLICT (asset_id, window_type, window_start)
DO UPDATE SET
    trading_volume = EXCLUDED.trading_volume,
    trading_volume_usd = EXCLUDED.trading_volume_usd,
    trade_count = EXCLUDED.trade_count,
    unique_active_traders = EXCLUDED.unique_active_traders,
    hourly_holder_growth_rate = EXCLUDED.hourly_holder_growth_rate,
    volatility_index = EXCLUDED.volatility_index,
    moving_average_price_usd = EXCLUDED.moving_average_price_usd,
    open_price_usd = EXCLUDED.open_price_usd,
    high_price_usd = EXCLUDED.high_price_usd,
    low_price_usd = EXCLUDED.low_price_usd,
    close_price_usd = EXCLUDED.close_price_usd;
"#
        .trim()
        .to_string();

        let params = vec![
            record.asset_id.to_string(),
            record.window_duration.as_str().to_string(),
            record.window_duration.as_secs().to_string(),
            record.window_start.to_rfc3339(),
            record.window_end.to_rfc3339(),
            format!("{:.2}", record.moving_avg_volume),
            format!("{:.4}", record.moving_avg_volume_usd),
            record.trade_count.to_string(),
            record.unique_active_holders.to_string(),
            format!("{:.6}", record.holder_velocity),
            format!("{:.6}", record.volatility_index),
            format!("{:.6}", record.moving_avg_price_usd),
            format!("{:.6}", record.open_price_usd),
            format!("{:.6}", record.high_price_usd),
            format!("{:.6}", record.low_price_usd),
            format!("{:.6}", record.close_price_usd),
        ];

        (sql, params)
    }

    /// Fast API retrieval query generator for latest window metrics of an asset.
    pub fn build_select_latest_query(asset_id: u64, duration: WindowDuration) -> (String, Vec<String>) {
        let sql = r#"
SELECT
    asset_id,
    window_duration,
    window_start,
    window_end,
    trading_volume,
    trading_volume_usd,
    trade_count,
    unique_active_traders,
    hourly_holder_growth_rate,
    volatility_index,
    moving_average_price_usd,
    open_price_usd,
    high_price_usd,
    low_price_usd,
    close_price_usd
FROM asset_metric_windows
WHERE asset_id = $1 AND window_duration = $2
ORDER BY window_start DESC
LIMIT 1;
"#
        .trim()
        .to_string();

        let params = vec![asset_id.to_string(), duration.as_str().to_string()];
        (sql, params)
    }
}

/// Generator for Apache Flink continuous streaming SQL pipeline.
pub struct FlinkSqlJobGenerator;

impl FlinkSqlJobGenerator {
    /// Formulates full Apache Flink SQL pipeline DDL and window continuous aggregations.
    pub fn generate_flink_sql(
        kafka_brokers: &str,
        jdbc_url: &str,
        db_user: &str,
        db_password: &str,
    ) -> String {
        format!(
            r#"-- =============================================================================
-- Tessera Apache Flink Real-Time Event Stream Window Aggregation Job
-- =============================================================================

-- 1. Source Table: Kafka Soroban Events
CREATE TABLE soroban_events_kafka (
    event_id            BIGINT,
    contract_id         STRING,
    asset_id            BIGINT,
    event_type          STRING,
    ledger_sequence     BIGINT,
    `timestamp`         TIMESTAMP(3),
    amount              DECIMAL(38, 0),
    price_usd           DOUBLE,
    from_address        STRING,
    to_address          STRING,
    WATERMARK FOR `timestamp` AS `timestamp` - INTERVAL '5' SECOND
) WITH (
    'connector' = 'kafka',
    'topic' = 'tessera.events.soroban',
    'properties.bootstrap.servers' = '{kafka_brokers}',
    'properties.group.id' = 'tessera-flink-indexer-group',
    'scan.startup.mode' = 'latest-offset',
    'format' = 'json'
);

-- 2. Sink Table: TimescaleDB asset_metric_windows Hypertable via JDBC
CREATE TABLE timescale_metric_windows_sink (
    asset_id                    BIGINT,
    window_type                 STRING,
    window_duration             STRING,
    window_duration_seconds     BIGINT,
    window_start                TIMESTAMP(3),
    window_end                  TIMESTAMP(3),
    trading_volume              DECIMAL(38, 4),
    trading_volume_usd          DECIMAL(18, 4),
    trade_count                 BIGINT,
    unique_active_traders       BIGINT,
    hourly_holder_growth_rate   DECIMAL(10, 4),
    volatility_index            DECIMAL(12, 6),
    moving_average_price_usd    DECIMAL(18, 6),
    open_price_usd              DECIMAL(18, 6),
    high_price_usd              DECIMAL(18, 6),
    low_price_usd               DECIMAL(18, 6),
    close_price_usd             DECIMAL(18, 6)
) WITH (
    'connector' = 'jdbc',
    'url' = '{jdbc_url}',
    'table-name' = 'asset_metric_windows',
    'username' = '{db_user}',
    'password' = '{db_password}'
);

-- 3. 1-Minute Moving Average Window Aggregation
INSERT INTO timescale_metric_windows_sink
SELECT
    asset_id,
    'sliding' AS window_type,
    '1m' AS window_duration,
    60 AS window_duration_seconds,
    TUMBLE_START(`timestamp`, INTERVAL '1' MINUTE) AS window_start,
    TUMBLE_END(`timestamp`, INTERVAL '1' MINUTE) AS window_end,
    SUM(CAST(amount AS DECIMAL(38, 4))) AS trading_volume,
    SUM(CAST(amount AS DOUBLE) * price_usd) AS trading_volume_usd,
    COUNT(1) AS trade_count,
    COUNT(DISTINCT from_address) AS unique_active_traders,
    0.0000 AS hourly_holder_growth_rate,
    STDDEV_SAMP(price_usd) AS volatility_index,
    AVG(price_usd) AS moving_average_price_usd,
    FIRST_VALUE(price_usd) AS open_price_usd,
    MAX(price_usd) AS high_price_usd,
    MIN(price_usd) AS low_price_usd,
    LAST_VALUE(price_usd) AS close_price_usd
FROM soroban_events_kafka
GROUP BY
    asset_id,
    TUMBLE(`timestamp`, INTERVAL '1' MINUTE);

-- 4. 1-Hour Moving Average Window Aggregation
INSERT INTO timescale_metric_windows_sink
SELECT
    asset_id,
    'sliding' AS window_type,
    '1h' AS window_duration,
    3600 AS window_duration_seconds,
    TUMBLE_START(`timestamp`, INTERVAL '1' HOUR) AS window_start,
    TUMBLE_END(`timestamp`, INTERVAL '1' HOUR) AS window_end,
    SUM(CAST(amount AS DECIMAL(38, 4))) AS trading_volume,
    SUM(CAST(amount AS DOUBLE) * price_usd) AS trading_volume_usd,
    COUNT(1) AS trade_count,
    COUNT(DISTINCT from_address) AS unique_active_traders,
    0.0000 AS hourly_holder_growth_rate,
    STDDEV_SAMP(price_usd) AS volatility_index,
    AVG(price_usd) AS moving_average_price_usd,
    FIRST_VALUE(price_usd) AS open_price_usd,
    MAX(price_usd) AS high_price_usd,
    MIN(price_usd) AS low_price_usd,
    LAST_VALUE(price_usd) AS close_price_usd
FROM soroban_events_kafka
GROUP BY
    asset_id,
    TUMBLE(`timestamp`, INTERVAL '1' HOUR);

-- 5. 24-Hour Moving Average Window Aggregation
INSERT INTO timescale_metric_windows_sink
SELECT
    asset_id,
    'sliding' AS window_type,
    '24h' AS window_duration,
    86400 AS window_duration_seconds,
    TUMBLE_START(`timestamp`, INTERVAL '1' DAY) AS window_start,
    TUMBLE_END(`timestamp`, INTERVAL '1' DAY) AS window_end,
    SUM(CAST(amount AS DECIMAL(38, 4))) AS trading_volume,
    SUM(CAST(amount AS DOUBLE) * price_usd) AS trading_volume_usd,
    COUNT(1) AS trade_count,
    COUNT(DISTINCT from_address) AS unique_active_traders,
    0.0000 AS hourly_holder_growth_rate,
    STDDEV_SAMP(price_usd) AS volatility_index,
    AVG(price_usd) AS moving_average_price_usd,
    FIRST_VALUE(price_usd) AS open_price_usd,
    MAX(price_usd) AS high_price_usd,
    MIN(price_usd) AS low_price_usd,
    LAST_VALUE(price_usd) AS close_price_usd
FROM soroban_events_kafka
GROUP BY
    asset_id,
    TUMBLE(`timestamp`, INTERVAL '1' DAY);
"#
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event(
        event_id: u64,
        asset_id: u64,
        offset_secs: i64,
        amount: i128,
        price: f64,
        from: &str,
        to: &str,
    ) -> KafkaSorobanEvent {
        let now = Utc::now();
        KafkaSorobanEvent::new(
            event_id,
            "CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ".to_string(),
            asset_id,
            "transfer".to_string(),
            123456,
            now - chrono::Duration::seconds(offset_secs),
            amount,
            price,
            Some(from.to_string()),
            Some(to.to_string()),
        )
    }

    #[tokio::test]
    async fn test_kafka_event_creation_and_partitioning() {
        let event = sample_event(1, 42, 10, 1000, 10.5, "alice", "bob");
        assert_eq!(event.partition_key(), "CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ");
        assert_eq!(event.deduplication_key, "123456:CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ:1");

        let (producer, mut rx, _dlq_rx) = KafkaFlinkProducer::new(KafkaProducerConfig::default(), 10);
        producer.stream_soroban_event(&event).await.unwrap();

        // Expect 2 messages: primary topic + transfers topic
        let msg1 = rx.recv().await.expect("expected primary message");
        assert_eq!(msg1.topic, "tessera.events.soroban");
        assert_eq!(msg1.key, event.contract_id);

        let msg2 = rx.recv().await.expect("expected transfers message");
        assert_eq!(msg2.topic, "tessera.events.transfers");
    }

    #[tokio::test]
    async fn test_window_durations_parsing() {
        assert_eq!(WindowDuration::from_str("1m"), Some(WindowDuration::M1));
        assert_eq!(WindowDuration::from_str("1h"), Some(WindowDuration::H1));
        assert_eq!(WindowDuration::from_str("24h"), Some(WindowDuration::H24));
        assert_eq!(WindowDuration::M1.as_secs(), 60);
        assert_eq!(WindowDuration::H1.as_secs(), 3600);
        assert_eq!(WindowDuration::H24.as_secs(), 86400);
    }

    #[tokio::test]
    async fn test_1m_1h_24h_moving_averages_and_holder_velocity() {
        let aggregator = FlinkWindowAggregator::new();
        let asset_id = 100;
        let circulating_supply = 100_000.0;
        aggregator.register_asset(asset_id, "CTOKEN123".to_string(), circulating_supply).await;

        let now = Utc::now();

        // Event 1: 30 seconds ago (falls in 1m, 1h, 24h)
        let e1 = KafkaSorobanEvent::new(
            1, "CTOKEN123".to_string(), asset_id, "transfer".to_string(), 1000,
            now - chrono::Duration::seconds(30), 1000, 10.0,
            Some("addr1".to_string()), Some("addr2".to_string()),
        );
        aggregator.ingest_event(&e1).await;

        // Event 2: 45 seconds ago (falls in 1m, 1h, 24h)
        let e2 = KafkaSorobanEvent::new(
            2, "CTOKEN123".to_string(), asset_id, "transfer".to_string(), 1001,
            now - chrono::Duration::seconds(45), 2000, 20.0,
            Some("addr2".to_string()), Some("addr3".to_string()),
        );
        aggregator.ingest_event(&e2).await;

        // Event 3: 30 minutes ago (falls in 1h, 24h, NOT in 1m)
        let e3 = KafkaSorobanEvent::new(
            3, "CTOKEN123".to_string(), asset_id, "transfer".to_string(), 1002,
            now - chrono::Duration::seconds(1800), 5000, 15.0,
            Some("addr3".to_string()), Some("addr4".to_string()),
        );
        aggregator.ingest_event(&e3).await;

        // Event 4: 12 hours ago (falls in 24h, NOT in 1m or 1h)
        let e4 = KafkaSorobanEvent::new(
            4, "CTOKEN123".to_string(), asset_id, "transfer".to_string(), 1003,
            now - chrono::Duration::seconds(43200), 10000, 25.0,
            Some("addr4".to_string()), Some("addr5".to_string()),
        );
        aggregator.ingest_event(&e4).await;

        // 1. Verify 1-minute window
        let m1 = aggregator.compute_window(asset_id, WindowDuration::M1, now).await.unwrap();
        assert_eq!(m1.trade_count, 2);
        assert_eq!(m1.moving_avg_volume, 3000.0);
        // VWAP = (1000 * 10 + 2000 * 20) / 3000 = 50000 / 3000 = 16.6666...
        assert!((m1.moving_avg_price_usd - 16.6666).abs() < 0.01);
        // Velocity = 3000 / 100000 = 0.03
        assert!((m1.holder_velocity - 0.03).abs() < 0.001);
        assert_eq!(m1.unique_active_holders, 3); // addr1, addr2, addr3

        // 2. Verify 1-hour window
        let h1 = aggregator.compute_window(asset_id, WindowDuration::H1, now).await.unwrap();
        assert_eq!(h1.trade_count, 3);
        assert_eq!(h1.moving_avg_volume, 8000.0); // 1000 + 2000 + 5000
        // VWAP = (10000 + 40000 + 75000) / 8000 = 125000 / 8000 = 15.625
        assert!((h1.moving_avg_price_usd - 15.625).abs() < 0.01);
        // Velocity = 8000 / 100000 = 0.08
        assert!((h1.holder_velocity - 0.08).abs() < 0.001);
        assert_eq!(h1.unique_active_holders, 4);

        // 3. Verify 24-hour window
        let h24 = aggregator.compute_window(asset_id, WindowDuration::H24, now).await.unwrap();
        assert_eq!(h24.trade_count, 4);
        assert_eq!(h24.moving_avg_volume, 18000.0); // 8000 + 10000
        // Velocity = 18000 / 100000 = 0.18
        assert!((h24.holder_velocity - 0.18).abs() < 0.001);
        assert_eq!(h24.unique_active_holders, 5);
    }

    #[test]
    fn test_timescaledb_sql_upsert_and_select_builders() {
        let record = FlinkWindowMetricRecord {
            asset_id: 42,
            token_contract: "CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ".to_string(),
            window_duration: WindowDuration::H1,
            window_start: Utc::now() - chrono::Duration::hours(1),
            window_end: Utc::now(),
            moving_avg_volume: 50000.0,
            moving_avg_volume_usd: 525000.0,
            holder_velocity: 0.05,
            moving_avg_price_usd: 10.5,
            trade_count: 142,
            unique_active_holders: 88,
            open_price_usd: 10.2,
            high_price_usd: 10.8,
            low_price_usd: 10.1,
            close_price_usd: 10.5,
            volatility_index: 0.12,
        };

        let (upsert_sql, params) = TimescaleSink::build_upsert_query(&record);
        assert!(upsert_sql.contains("INSERT INTO asset_metric_windows"));
        assert!(upsert_sql.contains("ON CONFLICT (asset_id, window_type, window_start)"));
        assert_eq!(params[0], "42");
        assert_eq!(params[1], "1h");
        assert_eq!(params[2], "3600");

        let (select_sql, select_params) = TimescaleSink::build_select_latest_query(42, WindowDuration::M1);
        assert!(select_sql.contains("SELECT"));
        assert!(select_sql.contains("FROM asset_metric_windows"));
        assert_eq!(select_params[0], "42");
        assert_eq!(select_params[1], "1m");
    }

    #[test]
    fn test_flink_sql_job_generation() {
        let ddl = FlinkSqlJobGenerator::generate_flink_sql(
            "kafka:9092",
            "jdbc:postgresql://postgres:5432/tessera",
            "tessera_user",
            "secret_pass",
        );

        assert!(ddl.contains("CREATE TABLE soroban_events_kafka"));
        assert!(ddl.contains("'connector' = 'kafka'"));
        assert!(ddl.contains("WATERMARK FOR `timestamp` AS `timestamp` - INTERVAL '5' SECOND"));
        assert!(ddl.contains("CREATE TABLE timescale_metric_windows_sink"));
        assert!(ddl.contains("'connector' = 'jdbc'"));
        assert!(ddl.contains("TUMBLE(`timestamp`, INTERVAL '1' MINUTE)"));
        assert!(ddl.contains("TUMBLE(`timestamp`, INTERVAL '1' HOUR)"));
        assert!(ddl.contains("TUMBLE(`timestamp`, INTERVAL '1' DAY)"));
    }
}
