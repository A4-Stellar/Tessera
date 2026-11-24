//! Cross-chain bridge monitoring (issue #475).
//!
//! Pairs the bridge contract's `SendPacket` events with the `RecvPacket` that
//! should follow on the other chain, and watches outbound volume for a drain.
//!
//! * **Packets.** Each packet has a key that both chains can name. An outbound
//!   Stellar packet is `stellar:<sequence>` (the contract's global packet
//!   sequence). An inbound packet is the hex message hash the validators
//!   signed. Stellar events come from the indexer ([`BridgeMonitor::observe_events`]).
//!   Watchers of the other chain report theirs with [`BridgeMonitor::observe_remote`].
//! * **Timeouts.** A `SendPacket` with no matching `RecvPacket` within
//!   `expected_relay_ledgers` becomes `timeout` and raises one `BridgeTimeout`
//!   alert through the webhook service.
//! * **Anomalies.** Outbound packets feed the z-score detector from issue #101
//!   (`anomaly_detector`), keyed per destination chain: `Volume` catches a few
//!   huge transfers and `Velocity` a flood of small ones. A flag becomes an
//!   anomaly score in basis points (`z / (2·threshold)`, capped at 10 000) and
//!   a `BridgeAnomaly` alert. The holder of the contract's anomaly-reporter key
//!   submits that score with `report_anomaly`. At or above the contract's
//!   threshold the breaker pauses new `SendPacket` calls until a Guardian
//!   quorum unpauses. The API never holds that key.
//!
//! Prometheus: `rwa_bridge_packets{status}`, `rwa_bridge_timeouts_total`,
//! `rwa_bridge_anomaly_score_bps{chain}`, `rwa_bridge_outbound_amount_total{chain}`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{json, Value};

use crate::models::Event;
use crate::services::anomaly_detector::{
    AnomalyDetector, DetectorConfig, Metric, Observation, ObservationKind,
};

/// Stellar closes a ledger about every five seconds.
const SECONDS_PER_LEDGER: u64 = 5;
/// The chain name used for this side of the bridge.
pub const STELLAR: &str = "stellar";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum PacketStatus {
    Pending,
    Completed,
    Timeout,
}

impl std::str::FromStr for PacketStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "completed" => Ok(Self::Completed),
            "timeout" => Ok(Self::Timeout),
            other => Err(format!(
                "unknown status {other:?}; use pending, completed or timeout"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Packet {
    pub key: String,
    pub source_chain: String,
    pub destination_chain: String,
    pub sender: Option<String>,
    /// Raw i128 amount as a string.
    pub amount: String,
    /// Stellar ledger at which the send was observed.
    pub sent_ledger: u32,
    pub received_ledger: Option<u32>,
    /// Ledger after which the packet counts as timed out.
    pub deadline_ledger: u32,
    pub status: PacketStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum BridgeAlert {
    BridgeTimeout {
        key: String,
        source_chain: String,
        destination_chain: String,
        amount: String,
        sent_ledger: u32,
        deadline_ledger: u32,
    },
    BridgeAnomaly {
        destination_chain: String,
        metric: Metric,
        z_score: f64,
        score_bps: u32,
        observed: f64,
        baseline_mean: f64,
    },
}

impl BridgeAlert {
    pub fn event_type(&self) -> &'static str {
        match self {
            BridgeAlert::BridgeTimeout { .. } => "bridge_timeout",
            BridgeAlert::BridgeAnomaly { .. } => "bridge_anomaly",
        }
    }
}

/// Where alerts go: the webhook dispatcher in production.
pub trait AlertSink: Send + Sync {
    fn send(&self, alert: &BridgeAlert);
}

/// A packet event seen on the other chain, reported by its watcher.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RemotePacketEvent {
    pub kind: RemoteKind,
    pub chain: String,
    pub key: String,
    #[serde(default)]
    pub amount: Option<String>,
    #[serde(default)]
    pub sender: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteKind {
    Send,
    Recv,
}

#[derive(Debug, Clone)]
pub struct BridgeMonitorConfig {
    /// Ledgers a relayer may take before a send is a timeout (~10 minutes).
    pub expected_relay_ledgers: u32,
    /// Detector settings for outbound packets, in seconds of ledger time.
    pub detector: DetectorConfig,
}

impl Default for BridgeMonitorConfig {
    fn default() -> Self {
        Self {
            expected_relay_ledgers: 120,
            detector: DetectorConfig {
                // Ten-minute buckets over the last day.
                bucket_secs: 600,
                window_buckets: 144,
                min_baseline_buckets: 6,
                ..DetectorConfig::default()
            },
        }
    }
}

#[derive(Default)]
struct Inner {
    packets: BTreeMap<String, Packet>,
    /// Receipts seen before their send (relayers can be faster than the
    /// indexer): key → ledger.
    early_receipts: HashMap<String, u32>,
    anomaly_scores: BTreeMap<String, u32>,
    alerts: Vec<BridgeAlert>,
    last_ledger: u32,
}

pub struct BridgeMonitor {
    config: BridgeMonitorConfig,
    detector: AnomalyDetector,
    inner: Mutex<Inner>,
    sink: Option<Box<dyn AlertSink>>,
}

impl Default for BridgeMonitor {
    fn default() -> Self {
        Self::new(BridgeMonitorConfig::default())
    }
}

/// Maps a z-score to the contract's basis-point anomaly score.
pub fn score_bps(z: f64, z_threshold: f64) -> u32 {
    if !z.is_finite() || z <= 0.0 || z_threshold <= 0.0 {
        return 0;
    }
    ((z / (2.0 * z_threshold)).min(1.0) * 10_000.0).round() as u32
}

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A hex `Bytes` value that holds UTF-8 text (chain names) is shown as text.
fn chain_name(v: Option<&Value>) -> String {
    let Some(raw) = text(v) else {
        return "unknown".into();
    };
    hex::decode(&raw)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_graphic()))
        .unwrap_or(raw)
}

impl BridgeMonitor {
    pub fn new(config: BridgeMonitorConfig) -> Self {
        let detector = AnomalyDetector::new(config.detector.clone());
        Self {
            config,
            detector,
            inner: Mutex::new(Inner::default()),
            sink: None,
        }
    }

    pub fn with_sink(mut self, sink: Box<dyn AlertSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn raise(&self, inner: &mut Inner, alert: BridgeAlert) {
        tracing::warn!(alert = ?alert, "bridge alert");
        if let Some(sink) = &self.sink {
            sink.send(&alert);
        }
        inner.alerts.push(alert);
        let overflow = inner.alerts.len().saturating_sub(1_000);
        inner.alerts.drain(..overflow);
    }

    fn record_send(&self, inner: &mut Inner, packet: Packet) {
        let key = packet.key.clone();
        let amount: f64 = packet.amount.parse().unwrap_or(0.0);
        let chain = packet.destination_chain.clone();
        let mut packet = packet;
        if let Some(ledger) = inner.early_receipts.remove(&key) {
            packet.status = PacketStatus::Completed;
            packet.received_ledger = Some(ledger);
        }
        if inner.packets.contains_key(&key) {
            return;
        }
        let timestamp = u64::from(packet.sent_ledger) * SECONDS_PER_LEDGER;
        inner.packets.insert(key.clone(), packet);
        metrics::counter!("rwa_bridge_outbound_amount_total", "chain" => chain.clone())
            .increment(amount.max(0.0) as u64);

        let obs = Observation {
            account: format!("bridge:{chain}"),
            timestamp,
            kind: ObservationKind::Transfer { amount },
        };
        for flag in self.detector.observe(&obs) {
            let score = score_bps(flag.z_score, self.config.detector.z_threshold);
            let best = inner.anomaly_scores.entry(chain.clone()).or_insert(0);
            *best = (*best).max(score);
            metrics::gauge!("rwa_bridge_anomaly_score_bps", "chain" => chain.clone())
                .set(f64::from(*best));
            self.raise(
                inner,
                BridgeAlert::BridgeAnomaly {
                    destination_chain: chain.clone(),
                    metric: flag.metric,
                    z_score: flag.z_score,
                    score_bps: score,
                    observed: flag.observed,
                    baseline_mean: flag.baseline_mean,
                },
            );
        }
    }

    fn record_recv(&self, inner: &mut Inner, key: &str, ledger: u32) {
        match inner.packets.get_mut(key) {
            Some(p) => {
                if p.status == PacketStatus::Timeout {
                    tracing::info!(key, "late RecvPacket for a timed-out packet");
                }
                p.status = PacketStatus::Completed;
                p.received_ledger = Some(ledger);
            }
            None => {
                inner.early_receipts.insert(key.to_string(), ledger);
            }
        }
    }

    /// Feeds bridge-contract events from the indexer, then checks deadlines at
    /// `current_ledger`. Returns the alerts raised.
    pub fn observe_events(&self, events: &[Event], current_ledger: u32) -> Vec<BridgeAlert> {
        let mut inner = self.lock();
        let before = inner.alerts.len();
        for e in events {
            let topics = e.data.get("topics").and_then(Value::as_array);
            let value = e.data.get("value").and_then(Value::as_array);
            let (Some(topics), Some(value)) = (topics, value) else {
                continue;
            };
            match e.event_type.as_str() {
                // value: [sequence, nonce, amount, destination_chain, destination_address, ledger]
                "SendPacket" => {
                    let Some(sequence) = text(value.first()) else {
                        continue;
                    };
                    let sent_ledger = value
                        .get(5)
                        .and_then(Value::as_u64)
                        .map_or(e.ledger, |l| l as u32);
                    let packet = Packet {
                        key: format!("{STELLAR}:{sequence}"),
                        source_chain: STELLAR.into(),
                        destination_chain: chain_name(value.get(3)),
                        sender: text(topics.get(1)),
                        amount: text(value.get(2)).unwrap_or_else(|| "0".into()),
                        sent_ledger,
                        received_ledger: None,
                        deadline_ledger: sent_ledger
                            .saturating_add(self.config.expected_relay_ledgers),
                        status: PacketStatus::Pending,
                    };
                    self.record_send(&mut inner, packet);
                }
                // value: [message_hash, amount, ledger]
                "RecvPacket" => {
                    if let Some(hash) = text(value.first()) {
                        self.record_recv(&mut inner, &hash.to_ascii_lowercase(), e.ledger);
                    }
                }
                _ => {}
            }
        }
        self.check_deadlines(&mut inner, current_ledger);
        inner.alerts[before..].to_vec()
    }

    /// A packet event observed on the other chain. Ledger positions use the
    /// current Stellar ledger, so deadlines are measured on one clock.
    pub fn observe_remote(
        &self,
        event: RemotePacketEvent,
        current_ledger: u32,
    ) -> Vec<BridgeAlert> {
        let mut inner = self.lock();
        let before = inner.alerts.len();
        let key = event.key.trim().to_ascii_lowercase();
        match event.kind {
            RemoteKind::Recv => self.record_recv(&mut inner, &key, current_ledger),
            RemoteKind::Send => {
                let packet = Packet {
                    key: key.clone(),
                    source_chain: event.chain.clone(),
                    destination_chain: STELLAR.into(),
                    sender: event.sender,
                    amount: event.amount.unwrap_or_else(|| "0".into()),
                    sent_ledger: current_ledger,
                    received_ledger: None,
                    deadline_ledger: current_ledger
                        .saturating_add(self.config.expected_relay_ledgers),
                    status: PacketStatus::Pending,
                };
                self.record_send(&mut inner, packet);
            }
        }
        self.check_deadlines(&mut inner, current_ledger);
        inner.alerts[before..].to_vec()
    }

    fn check_deadlines(&self, inner: &mut Inner, current_ledger: u32) {
        inner.last_ledger = inner.last_ledger.max(current_ledger);
        let expired: Vec<Packet> = inner
            .packets
            .values_mut()
            .filter(|p| p.status == PacketStatus::Pending && current_ledger > p.deadline_ledger)
            .map(|p| {
                p.status = PacketStatus::Timeout;
                p.clone()
            })
            .collect();
        for p in expired {
            metrics::counter!("rwa_bridge_timeouts_total").increment(1);
            self.raise(
                inner,
                BridgeAlert::BridgeTimeout {
                    key: p.key,
                    source_chain: p.source_chain,
                    destination_chain: p.destination_chain,
                    amount: p.amount,
                    sent_ledger: p.sent_ledger,
                    deadline_ledger: p.deadline_ledger,
                },
            );
        }
        for status in [
            PacketStatus::Pending,
            PacketStatus::Completed,
            PacketStatus::Timeout,
        ] {
            let n = inner
                .packets
                .values()
                .filter(|p| p.status == status)
                .count();
            let label = serde_json::to_value(status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            metrics::gauge!("rwa_bridge_packets", "status" => label).set(n as f64);
        }
    }

    /// Packets, optionally filtered by status, newest first, at most `limit`.
    pub fn packets(&self, status: Option<PacketStatus>, limit: usize) -> Vec<Packet> {
        let inner = self.lock();
        let mut out: Vec<Packet> = inner
            .packets
            .values()
            .filter(|p| status.is_none_or(|s| p.status == s))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.sent_ledger
                .cmp(&a.sent_ledger)
                .then_with(|| b.key.cmp(&a.key))
        });
        out.truncate(limit);
        out
    }

    pub fn counts(&self) -> BTreeMap<PacketStatus, usize> {
        let inner = self.lock();
        let mut counts = BTreeMap::from([
            (PacketStatus::Pending, 0),
            (PacketStatus::Completed, 0),
            (PacketStatus::Timeout, 0),
        ]);
        for p in inner.packets.values() {
            *counts.entry(p.status).or_default() += 1;
        }
        counts
    }

    /// Highest anomaly score seen per destination chain.
    pub fn anomaly_scores(&self) -> BTreeMap<String, u32> {
        self.lock().anomaly_scores.clone()
    }

    pub fn expected_relay_ledgers(&self) -> u32 {
        self.config.expected_relay_ledgers
    }
}

/// The webhook payload for an alert.
pub fn webhook_payload(alert: &BridgeAlert) -> Value {
    json!({ "event_type": alert.event_type(), "alert": alert })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    const BRIDGE: &str = "CBRIDGE";

    fn send(seq: u64, amount: i128, ledger: u32) -> Event {
        Event {
            id: seq + 1,
            contract: BRIDGE.into(),
            event_type: "SendPacket".into(),
            ledger,
            timestamp: None,
            data: json!({
                "topics": ["SendPacket", "GSENDER"],
                // `ethereum` as the hex the indexer renders Bytes with.
                "value": [seq, 0, amount.to_string(), hex::encode("ethereum"), "0xabc", ledger],
            }),
        }
    }

    #[derive(Default)]
    struct Collect(Mutex<Vec<BridgeAlert>>);
    impl AlertSink for Arc<Collect> {
        fn send(&self, alert: &BridgeAlert) {
            self.0.lock().unwrap().push(alert.clone());
        }
    }

    #[test]
    fn send_without_receipt_times_out_once_and_alerts() {
        let sink = Arc::new(Collect::default());
        let m = BridgeMonitor::default().with_sink(Box::new(sink.clone()));
        assert!(m.observe_events(&[send(7, 500, 1_000)], 1_000).is_empty());
        assert_eq!(
            m.packets(Some(PacketStatus::Pending), 10)[0].destination_chain,
            "ethereum"
        );

        // Still inside expected_relay_ledgers (120).
        assert!(m.observe_events(&[], 1_120).is_empty());
        let alerts = m.observe_events(&[], 1_121);
        assert_eq!(alerts.len(), 1);
        assert!(matches!(&alerts[0], BridgeAlert::BridgeTimeout { key, .. } if key == "stellar:7"));
        assert!(
            m.observe_events(&[], 1_500).is_empty(),
            "one alert per packet"
        );
        assert_eq!(sink.0.lock().unwrap().len(), 1);
        assert_eq!(m.counts()[&PacketStatus::Timeout], 1);
    }

    #[test]
    fn remote_receipt_completes_the_packet_even_if_it_arrives_first() {
        let m = BridgeMonitor::default();
        let recv = |key: &str| RemotePacketEvent {
            kind: RemoteKind::Recv,
            chain: "ethereum".into(),
            key: key.into(),
            amount: None,
            sender: None,
        };
        m.observe_remote(recv("stellar:3"), 1_000);
        m.observe_events(&[send(3, 10, 1_000), send(4, 10, 1_000)], 1_010);
        m.observe_remote(recv("STELLAR:4"), 1_020);
        let done = m.packets(Some(PacketStatus::Completed), 10);
        assert_eq!(done.len(), 2);
        assert!(
            m.observe_events(&[], 5_000).is_empty(),
            "completed packets never time out"
        );
    }

    #[test]
    fn inbound_packets_are_matched_by_message_hash() {
        let m = BridgeMonitor::default();
        let hash = "ab".repeat(32);
        m.observe_remote(
            RemotePacketEvent {
                kind: RemoteKind::Send,
                chain: "ethereum".into(),
                key: hash.to_uppercase(),
                amount: Some("9".into()),
                sender: None,
            },
            2_000,
        );
        let recv = Event {
            id: 99,
            contract: BRIDGE.into(),
            event_type: "RecvPacket".into(),
            ledger: 2_030,
            timestamp: None,
            data: json!({ "topics": ["RecvPacket", "GCALLER"], "value": [hash, "9", 2_030] }),
        };
        m.observe_events(&[recv], 2_030);
        let p = &m.packets(None, 10)[0];
        assert_eq!(p.status, PacketStatus::Completed);
        assert_eq!(p.received_ledger, Some(2_030));
    }

    /// Drain simulation: a steady day of traffic, then a burst of large
    /// outbound packets. The detector flags it, the score crosses the
    /// contract's default 8 000 bps breaker threshold, and the packets that
    /// never land on the other chain time out.
    #[test]
    fn bridge_drain_raises_anomaly_and_timeouts() {
        let sink = Arc::new(Collect::default());
        let m = BridgeMonitor::default().with_sink(Box::new(sink.clone()));
        let bucket_ledgers = 600 / SECONDS_PER_LEDGER as u32;
        let mut seq = 0;
        let mut ledger = 100_000;
        // 12 quiet buckets: two ~1 000-unit transfers each, all relayed.
        for _ in 0..12 {
            for amount in [900, 1_100] {
                m.observe_events(&[send(seq, amount, ledger)], ledger);
                m.observe_remote(
                    RemotePacketEvent {
                        kind: RemoteKind::Recv,
                        chain: "ethereum".into(),
                        key: format!("stellar:{seq}"),
                        amount: None,
                        sender: None,
                    },
                    ledger + 5,
                );
                seq += 1;
            }
            ledger += bucket_ledgers;
        }
        assert!(
            m.anomaly_scores().is_empty(),
            "normal traffic is not flagged"
        );

        // The drain: 30 packets of 50 000 within one bucket, none relayed.
        let burst: Vec<Event> = (0..30)
            .map(|i| send(seq + i, 50_000, ledger + i as u32))
            .collect();
        let alerts = m.observe_events(&burst, ledger + 30);
        let score = m.anomaly_scores()["ethereum"];
        assert!(
            score >= 8_000,
            "drain score {score} must reach the breaker threshold"
        );
        assert!(alerts.iter().any(|a| matches!(
            a,
            BridgeAlert::BridgeAnomaly {
                metric: Metric::Volume,
                ..
            }
        )));
        assert!(alerts.iter().any(|a| matches!(
            a,
            BridgeAlert::BridgeAnomaly {
                metric: Metric::Velocity,
                ..
            }
        )));

        let timeouts = m.observe_events(&[], ledger + 30 + 121);
        assert_eq!(timeouts.len(), 30);
        assert_eq!(m.counts()[&PacketStatus::Completed], 24);
        assert!(sink.0.lock().unwrap().len() >= 32);
    }

    #[test]
    fn score_is_scaled_to_basis_points() {
        assert_eq!(score_bps(3.5, 3.5), 5_000);
        assert_eq!(score_bps(7.0, 3.5), 10_000);
        assert_eq!(score_bps(70.0, 3.5), 10_000);
        assert_eq!(score_bps(-1.0, 3.5), 0);
    }
}
