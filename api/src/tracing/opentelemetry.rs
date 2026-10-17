//! Distributed Tracing & APM Instrumentation with OpenTelemetry (Issue #228).
//!
//! Provides production-grade OpenTelemetry tracing across the Tessera API,
//! Soroban RPC poller/indexer, and HTTP request pipeline:
//! - W3C Trace Context (`traceparent`, `tracestate`) injection and extraction.
//! - Tokio background task and RPC span propagation.
//! - Trace span attributes for `ledger_sequence`, `contract_id`, and `query_duration_ms`.
//! - Asynchronous, batched OTLP export to OpenTelemetry collectors (Jaeger, Grafana Tempo, Datadog).

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::http::{header::HeaderName, HeaderMap, HeaderValue};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// W3C Header constants
pub const W3C_TRACEPARENT_HEADER: &str = "traceparent";
pub const W3C_TRACESTATE_HEADER: &str = "tracestate";
pub const W3C_VERSION: &str = "00";

/// Canonical OpenTelemetry span attribute keys for Tessera
pub const ATTR_LEDGER_SEQUENCE: &str = "ledger_sequence";
pub const ATTR_CONTRACT_ID: &str = "contract_id";
pub const ATTR_QUERY_DURATION_MS: &str = "query_duration_ms";
pub const ATTR_HTTP_METHOD: &str = "http.method";
pub const ATTR_HTTP_ROUTE: &str = "http.route";
pub const ATTR_HTTP_STATUS_CODE: &str = "http.status_code";
pub const ATTR_RPC_SYSTEM: &str = "rpc.system";
pub const ATTR_RPC_METHOD: &str = "rpc.method";
pub const ATTR_RPC_ENDPOINT: &str = "rpc.endpoint";
pub const ATTR_SERVICE_NAME: &str = "service.name";
pub const ATTR_SERVICE_VERSION: &str = "service.version";

// ---------------------------------------------------------------------------
// 1. W3C Trace Context Representation & Propagation
// ---------------------------------------------------------------------------

/// W3C Trace Context compliant with the W3C Recommendation (Trace Context Level 1).
/// Format: `00-{trace_id}-{parent_id}-{trace_flags}`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceContext {
    /// 16-byte (32 lowercase hex characters) unique trace identifier.
    pub trace_id: String,
    /// 8-byte (16 lowercase hex characters) unique span identifier.
    pub span_id: String,
    /// Optional 8-byte (16 lowercase hex characters) parent span identifier.
    pub parent_span_id: Option<String>,
    /// 8-bit bitmap representing trace options (0x01 = sampled).
    pub trace_flags: u8,
    /// Optional opaque vendor-specific state string.
    pub tracestate: Option<String>,
}

impl TraceContext {
    /// Create a new root trace context with random trace_id and span_id.
    pub fn new_root() -> Self {
        Self::with_sampling(true)
    }

    /// Create a new root trace context with an explicit sampling decision.
    pub fn with_sampling(sampled: bool) -> Self {
        let mut trace_id_bytes = [0u8; 16];
        let mut span_id_bytes = [0u8; 8];
        rand::rng().fill_bytes(&mut trace_id_bytes);
        rand::rng().fill_bytes(&mut span_id_bytes);

        // Ensure non-zero IDs per W3C specification
        if trace_id_bytes.iter().all(|&b| b == 0) {
            trace_id_bytes[0] = 1;
        }
        if span_id_bytes.iter().all(|&b| b == 0) {
            span_id_bytes[0] = 1;
        }

        Self {
            trace_id: hex::encode(trace_id_bytes),
            span_id: hex::encode(span_id_bytes),
            parent_span_id: None,
            trace_flags: if sampled { 0x01 } else { 0x00 },
            tracestate: None,
        }
    }

    /// Spawn a child trace context retaining the same trace_id, setting this
    /// span as the parent, and generating a fresh child span_id.
    pub fn child(&self) -> Self {
        let mut child_span_id_bytes = [0u8; 8];
        rand::rng().fill_bytes(&mut child_span_id_bytes);
        if child_span_id_bytes.iter().all(|&b| b == 0) {
            child_span_id_bytes[0] = 1;
        }

        Self {
            trace_id: self.trace_id.clone(),
            span_id: hex::encode(child_span_id_bytes),
            parent_span_id: Some(self.span_id.clone()),
            trace_flags: self.trace_flags,
            tracestate: self.tracestate.clone(),
        }
    }

    /// Parse a W3C `traceparent` header string.
    /// Format: `00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01`
    pub fn parse_traceparent(header_val: &str) -> Option<Self> {
        let parts: Vec<&str> = header_val.trim().split('-').collect();
        if parts.len() < 4 {
            return None;
        }

        let version = parts[0];
        let trace_id = parts[1];
        let parent_id = parts[2];
        let flags_str = parts[3];

        // Currently supported W3C version is '00'
        if version != W3C_VERSION && version != "ff" && parts.len() == 4 {
            // Forward compatible if len == 4 and version not ff
        } else if version == "ff" {
            return None;
        }

        // Trace ID must be 32 hex chars and not all zeros
        if trace_id.len() != 32 || trace_id.chars().all(|c| c == '0') || !trace_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }

        // Parent ID must be 16 hex chars and not all zeros
        if parent_id.len() != 16 || parent_id.chars().all(|c| c == '0') || !parent_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }

        // Flags must be 2 hex chars
        if flags_str.len() != 2 {
            return None;
        }
        let trace_flags = u8::from_str_radix(flags_str, 16).ok()?;

        Some(Self {
            trace_id: trace_id.to_ascii_lowercase(),
            span_id: parent_id.to_ascii_lowercase(),
            parent_span_id: None,
            trace_flags,
            tracestate: None,
        })
    }

    /// Serialize this context into a W3C `traceparent` header value string.
    pub fn to_traceparent_header(&self) -> String {
        format!("{}-{}-{}-{:02x}", W3C_VERSION, self.trace_id, self.span_id, self.trace_flags)
    }

    /// Extract W3C Trace Context from an Axum/Hyper `HeaderMap`.
    pub fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let traceparent_header = headers.get(W3C_TRACEPARENT_HEADER)?.to_str().ok()?;
        let mut ctx = Self::parse_traceparent(traceparent_header)?;

        if let Some(state_header) = headers.get(W3C_TRACESTATE_HEADER).and_then(|h| h.to_str().ok()) {
            if !state_header.trim().is_empty() {
                ctx.tracestate = Some(state_header.trim().to_string());
            }
        }
        Some(ctx)
    }

    /// Inject W3C Trace Context headers into an outgoing HTTP `HeaderMap`.
    pub fn inject_headers(&self, headers: &mut HeaderMap) {
        if let Ok(val) = HeaderValue::from_str(&self.to_traceparent_header()) {
            headers.insert(HeaderName::from_static(W3C_TRACEPARENT_HEADER), val);
        }
        if let Some(ref state) = self.tracestate {
            if let Ok(val) = HeaderValue::from_str(state) {
                headers.insert(HeaderName::from_static(W3C_TRACESTATE_HEADER), val);
            }
        }
    }

    /// Whether this trace is sampled.
    pub fn is_sampled(&self) -> bool {
        (self.trace_flags & 0x01) == 0x01
    }
}

// ---------------------------------------------------------------------------
// 2. Span Data Model & Semantic Attributes
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SpanKind {
    Internal = 1,
    Server = 2,
    Client = 3,
    Producer = 4,
    Consumer = 5,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpanStatus {
    Unset,
    Ok,
    Error { description: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl From<&str> for AttributeValue {
    fn from(s: &str) -> Self {
        AttributeValue::String(s.to_string())
    }
}

impl From<String> for AttributeValue {
    fn from(s: String) -> Self {
        AttributeValue::String(s)
    }
}

impl From<i64> for AttributeValue {
    fn from(i: i64) -> Self {
        AttributeValue::Int(i)
    }
}

impl From<u32> for AttributeValue {
    fn from(u: u32) -> Self {
        AttributeValue::Int(u as i64)
    }
}

impl From<u64> for AttributeValue {
    fn from(u: u64) -> Self {
        AttributeValue::Int(u as i64)
    }
}

impl From<f64> for AttributeValue {
    fn from(f: f64) -> Self {
        AttributeValue::Float(f)
    }
}

impl From<bool> for AttributeValue {
    fn from(b: bool) -> Self {
        AttributeValue::Bool(b)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpanEvent {
    pub name: String,
    pub timestamp_unix_nano: u64,
    pub attributes: HashMap<String, AttributeValue>,
}

/// An individual trace span instrumented across microservices.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Span {
    pub name: String,
    pub context: TraceContext,
    pub parent_span_id: Option<String>,
    pub kind: SpanKind,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub status: SpanStatus,
    pub attributes: HashMap<String, AttributeValue>,
    pub events: Vec<SpanEvent>,
}

impl Span {
    pub fn new(name: impl Into<String>, context: TraceContext, kind: SpanKind) -> Self {
        let parent_span_id = context.parent_span_id.clone();
        let now_nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        Self {
            name: name.into(),
            context,
            parent_span_id,
            kind,
            start_time_unix_nano: now_nano,
            end_time_unix_nano: 0,
            status: SpanStatus::Unset,
            attributes: HashMap::new(),
            events: Vec::new(),
        }
    }

    /// Set required Acceptance Criteria attribute: `ledger_sequence`
    pub fn set_ledger_sequence(&mut self, seq: u32) -> &mut Self {
        self.attributes.insert(ATTR_LEDGER_SEQUENCE.to_string(), AttributeValue::Int(seq as i64));
        self
    }

    /// Set required Acceptance Criteria attribute: `contract_id`
    pub fn set_contract_id(&mut self, contract_id: impl Into<String>) -> &mut Self {
        self.attributes.insert(ATTR_CONTRACT_ID.to_string(), AttributeValue::String(contract_id.into()));
        self
    }

    /// Set required Acceptance Criteria attribute: `query_duration_ms`
    pub fn set_query_duration_ms(&mut self, duration_ms: f64) -> &mut Self {
        self.attributes.insert(ATTR_QUERY_DURATION_MS.to_string(), AttributeValue::Float(duration_ms));
        self
    }

    /// Set generic attribute
    pub fn set_attribute(&mut self, key: impl Into<String>, value: impl Into<AttributeValue>) -> &mut Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// Record an in-span event (milestone)
    pub fn add_event(&mut self, name: impl Into<String>) {
        let now_nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        self.events.push(SpanEvent {
            name: name.into(),
            timestamp_unix_nano: now_nano,
            attributes: HashMap::new(),
        });
    }

    /// Mark the span as finished, setting the completion timestamp.
    pub fn finish(&mut self) {
        if self.end_time_unix_nano == 0 {
            self.end_time_unix_nano = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            if self.status == SpanStatus::Unset {
                self.status = SpanStatus::Ok;
            }
        }
    }

    /// Mark the span as finished with an explicit status.
    pub fn finish_with_status(&mut self, status: SpanStatus) {
        self.finish();
        self.status = status;
    }

    /// Get elapsed duration in milliseconds.
    pub fn duration_ms(&self) -> f64 {
        let end = if self.end_time_unix_nano > 0 {
            self.end_time_unix_nano
        } else {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
        };
        (end.saturating_sub(self.start_time_unix_nano)) as f64 / 1_000_000.0
    }
}

// ---------------------------------------------------------------------------
// 3. OTLP JSON Exporter (Jaeger / Grafana Tempo / Datadog)
// ---------------------------------------------------------------------------

/// Configuration for the OpenTelemetry collector exporter.
#[derive(Clone, Debug)]
pub struct OtelCollectorConfig {
    /// OTLP HTTP/protobuf endpoint (e.g. `http://localhost:4318/v1/traces`).
    pub endpoint: String,
    /// Service name reported in resource attributes.
    pub service_name: String,
    /// Service version string.
    pub service_version: String,
    /// Deployment environment (e.g., `production`, `staging`, `local`).
    pub environment: String,
    /// Maximum number of spans queued before a forced batch export.
    pub batch_size: usize,
    /// Flush interval when batch size is not reached.
    pub flush_interval: Duration,
    /// Timeout for HTTP requests to the collector.
    pub export_timeout: Duration,
    /// Whether export is globally enabled.
    pub enabled: bool,
}

impl Default for OtelCollectorConfig {
    fn default() -> Self {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:4318/v1/traces".to_string());
        let service_name = std::env::var("OTEL_SERVICE_NAME")
            .unwrap_or_else(|_| "tessera-api".to_string());
        let environment = std::env::var("TESSERA_ENV")
            .unwrap_or_else(|_| "production".to_string());

        Self {
            endpoint,
            service_name,
            service_version: "0.1.0".to_string(),
            environment,
            batch_size: 512,
            flush_interval: Duration::from_millis(1000),
            export_timeout: Duration::from_secs(5),
            enabled: true,
        }
    }
}

/// Standalone OTLP Exporter serializing spans into OpenTelemetry v1 JSON.
#[derive(Clone)]
pub struct OtlpExporter {
    config: OtelCollectorConfig,
    client: reqwest::Client,
}

impl OtlpExporter {
    pub fn new(config: OtelCollectorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.export_timeout)
            .build()
            .unwrap_or_default();
        Self { config, client }
    }

    /// Serialize a batch of spans to standard OpenTelemetry Trace Service JSON payload.
    pub fn build_otlp_payload(&self, spans: &[Span]) -> serde_json::Value {
        let otlp_spans: Vec<serde_json::Value> = spans
            .iter()
            .map(|s| {
                let attrs: Vec<serde_json::Value> = s
                    .attributes
                    .iter()
                    .map(|(k, v)| {
                        let val_obj = match v {
                            AttributeValue::String(val) => serde_json::json!({ "stringValue": val }),
                            AttributeValue::Int(val) => serde_json::json!({ "intValue": val.to_string() }),
                            AttributeValue::Float(val) => serde_json::json!({ "doubleValue": val }),
                            AttributeValue::Bool(val) => serde_json::json!({ "boolValue": val }),
                        };
                        serde_json::json!({
                            "key": k,
                            "value": val_obj
                        })
                    })
                    .collect();

                let status_obj = match &s.status {
                    SpanStatus::Ok => serde_json::json!({ "code": 1 }),
                    SpanStatus::Error { description } => {
                        serde_json::json!({ "code": 2, "message": description })
                    }
                    SpanStatus::Unset => serde_json::json!({ "code": 0 }),
                };

                let mut span_json = serde_json::json!({
                    "traceId": s.context.trace_id,
                    "spanId": s.context.span_id,
                    "name": s.name,
                    "kind": s.kind as u32,
                    "startTimeUnixNano": s.start_time_unix_nano.to_string(),
                    "endTimeUnixNano": s.end_time_unix_nano.to_string(),
                    "attributes": attrs,
                    "status": status_obj,
                });

                if let Some(ref parent) = s.parent_span_id {
                    span_json["parentSpanId"] = serde_json::json!(parent);
                }

                span_json
            })
            .collect();

        serde_json::json!({
            "resourceSpans": [
                {
                    "resource": {
                        "attributes": [
                            {
                                "key": ATTR_SERVICE_NAME,
                                "value": { "stringValue": self.config.service_name }
                            },
                            {
                                "key": ATTR_SERVICE_VERSION,
                                "value": { "stringValue": self.config.service_version }
                            },
                            {
                                "key": "deployment.environment",
                                "value": { "stringValue": self.config.environment }
                            }
                        ]
                    },
                    "scopeSpans": [
                        {
                            "scope": {
                                "name": "tessera-opentelemetry",
                                "version": "0.1.0"
                            },
                            "spans": otlp_spans
                        }
                    ]
                }
            ]
        })
    }

    /// Asynchronously export spans to the configured OTLP collector.
    pub async fn export(&self, spans: &[Span]) -> Result<(), String> {
        if spans.is_empty() || !self.config.enabled {
            return Ok(());
        }

        let payload = self.build_otlp_payload(spans);
        let resp = self
            .client
            .post(&self.config.endpoint)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("OTLP HTTP post failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!(
                "Collector rejected trace batch with status: {}",
                resp.status()
            ));
        }

        debug!(
            batch_size = spans.len(),
            endpoint = %self.config.endpoint,
            "Successfully exported trace spans to OpenTelemetry collector"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 4. Central Tracer & Asynchronous Batch Processor
// ---------------------------------------------------------------------------

struct TracerInner {
    config: OtelCollectorConfig,
    exporter: OtlpExporter,
    span_tx: mpsc::Sender<Span>,
}

/// Global/Shared OpenTelemetry Tracer for Tessera.
#[derive(Clone)]
pub struct Tracer {
    inner: Arc<TracerInner>,
}

impl Tracer {
    /// Initialize a new Tracer and start its background export task.
    pub fn init(config: OtelCollectorConfig) -> Self {
        let (span_tx, mut span_rx) = mpsc::channel::<Span>(4096);
        let exporter = OtlpExporter::new(config.clone());

        let batch_size = config.batch_size;
        let flush_interval = config.flush_interval;
        let bg_exporter = exporter.clone();

        // Background worker collecting and flushing spans
        tokio::spawn(async move {
            let mut buffer: Vec<Span> = Vec::with_capacity(batch_size);
            let mut timer = tokio::time::interval(flush_interval);

            loop {
                tokio::select! {
                    Some(span) = span_rx.recv() => {
                        buffer.push(span);
                        if buffer.len() >= batch_size {
                            let to_export = std::mem::replace(&mut buffer, Vec::with_capacity(batch_size));
                            if let Err(e) = bg_exporter.export(&to_export).await {
                                warn!(error = %e, "Failed to export trace spans batch to collector");
                            }
                        }
                    }
                    _ = timer.tick() => {
                        if !buffer.is_empty() {
                            let to_export = std::mem::replace(&mut buffer, Vec::with_capacity(batch_size));
                            if let Err(e) = bg_exporter.export(&to_export).await {
                                warn!(error = %e, "Periodic trace flush failed");
                            }
                        }
                    }
                    else => break,
                }
            }
        });

        Self {
            inner: Arc::new(TracerInner {
                config,
                exporter,
                span_tx,
            }),
        }
    }

    /// Start a new span with parent context or create a new root context.
    pub fn start_span(
        &self,
        name: impl Into<String>,
        parent_context: Option<&TraceContext>,
        kind: SpanKind,
    ) -> Span {
        let ctx = match parent_context {
            Some(parent) => parent.child(),
            None => TraceContext::new_root(),
        };
        Span::new(name, ctx, kind)
    }

    /// Enqueue a finished span for export.
    pub fn record_span(&self, mut span: Span) {
        span.finish();
        // Ignore send errors if background receiver has exited during shutdown
        let _ = self.inner.span_tx.try_send(span);
    }

    /// Direct export for synchronous testing or shutdown flushing.
    pub async fn export_direct(&self, spans: &[Span]) -> Result<(), String> {
        self.inner.exporter.export(spans).await
    }
}

// ---------------------------------------------------------------------------
// 5. Instrumentation Helpers (HTTP, Tokio Tasks, and Soroban RPC)
// ---------------------------------------------------------------------------

/// Instrument a Soroban RPC query simulation, recording `ledger_sequence`,
/// `contract_id`, and `query_duration_ms` attributes.
pub async fn trace_rpc_query<F, Fut, T>(
    tracer: &Tracer,
    parent_ctx: Option<&TraceContext>,
    contract_id: &str,
    ledger_sequence: u32,
    rpc_method: &str,
    fut: F,
) -> (T, TraceContext)
where
    F: FnOnce(TraceContext) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut span = tracer.start_span(format!("soroban_rpc/{rpc_method}"), parent_ctx, SpanKind::Client);
    span.set_contract_id(contract_id)
        .set_ledger_sequence(ledger_sequence)
        .set_attribute(ATTR_RPC_SYSTEM, "soroban_rpc")
        .set_attribute(ATTR_RPC_METHOD, rpc_method);

    let start = Instant::now();
    let ctx = span.context.clone();
    let result = fut(ctx.clone()).await;
    let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

    span.set_query_duration_ms(duration_ms);

    match &result {
        Ok(_) => {
            span.finish_with_status(SpanStatus::Ok);
        }
        Err(err) => {
            span.finish_with_status(SpanStatus::Error {
                description: err.clone(),
            });
        }
    }

    tracer.record_span(span);
    (result.unwrap(), ctx)
}

/// Propagate a trace context across a Tokio background task boundary.
pub fn spawn_traced_task<F, Fut, T>(
    tracer: Tracer,
    parent_ctx: TraceContext,
    task_name: &'static str,
    f: F,
) -> tokio::task::JoinHandle<T>
where
    F: FnOnce(TraceContext) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    tokio::spawn(async move {
        let mut span = tracer.start_span(task_name, Some(&parent_ctx), SpanKind::Internal);
        let child_ctx = span.context.clone();
        let res = f(child_ctx).await;
        span.finish_with_status(SpanStatus::Ok);
        tracer.record_span(span);
        res
    })
}

// ---------------------------------------------------------------------------
// 6. Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_w3c_traceparent_parsing_and_generation() {
        let valid_header = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let ctx = TraceContext::parse_traceparent(valid_header).expect("must parse valid header");

        assert_eq!(ctx.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(ctx.span_id, "00f067aa0ba902b7");
        assert_eq!(ctx.trace_flags, 0x01);
        assert!(ctx.is_sampled());

        let regenerated = ctx.to_traceparent_header();
        assert_eq!(regenerated, valid_header);
    }

    #[test]
    fn test_w3c_traceparent_invalid_headers() {
        // All-zeros trace ID is invalid per W3C
        assert!(TraceContext::parse_traceparent("00-00000000000000000000000000000000-00f067aa0ba902b7-01").is_none());
        // All-zeros parent ID is invalid per W3C
        assert!(TraceContext::parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01").is_none());
        // Invalid length
        assert!(TraceContext::parse_traceparent("00-short-00f067aa0ba902b7-01").is_none());
        // Unsupported ff version
        assert!(TraceContext::parse_traceparent("ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_none());
    }

    #[test]
    fn test_child_context_propagation() {
        let root = TraceContext::new_root();
        assert_eq!(root.trace_id.len(), 32);
        assert_eq!(root.span_id.len(), 16);
        assert!(root.parent_span_id.is_none());

        let child = root.child();
        assert_eq!(child.trace_id, root.trace_id, "Trace ID must propagate to child");
        assert_ne!(child.span_id, root.span_id, "Child must have new span ID");
        assert_eq!(child.parent_span_id, Some(root.span_id.clone()), "Parent span ID must be linked");
        assert_eq!(child.trace_flags, root.trace_flags);
    }

    #[test]
    fn test_header_injection_and_extraction() {
        let ctx = TraceContext::new_root();
        let mut headers = HeaderMap::new();

        ctx.inject_headers(&mut headers);
        assert!(headers.contains_key(W3C_TRACEPARENT_HEADER));

        let extracted = TraceContext::from_headers(&headers).expect("must extract from headers");
        assert_eq!(extracted.trace_id, ctx.trace_id);
        assert_eq!(extracted.span_id, ctx.span_id);
        assert_eq!(extracted.trace_flags, ctx.trace_flags);
    }

    #[test]
    fn test_acceptance_criteria_span_attributes() {
        let ctx = TraceContext::new_root();
        let mut span = Span::new("indexer_simulate", ctx, SpanKind::Client);

        // Required Acceptance Criteria:
        // Include span attributes for ledger_sequence, contract_id, and query_duration_ms
        span.set_ledger_sequence(1048576)
            .set_contract_id("CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3")
            .set_query_duration_ms(42.5);

        assert_eq!(
            span.attributes.get(ATTR_LEDGER_SEQUENCE),
            Some(&AttributeValue::Int(1048576))
        );
        assert_eq!(
            span.attributes.get(ATTR_CONTRACT_ID),
            Some(&AttributeValue::String("CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3".to_string()))
        );
        assert_eq!(
            span.attributes.get(ATTR_QUERY_DURATION_MS),
            Some(&AttributeValue::Float(42.5))
        );

        span.finish();
        assert!(span.end_time_unix_nano >= span.start_time_unix_nano);
        assert_eq!(span.status, SpanStatus::Ok);
    }

    #[test]
    fn test_otlp_json_payload_structure() {
        let config = OtelCollectorConfig {
            endpoint: "http://localhost:4318/v1/traces".to_string(),
            service_name: "tessera-api".to_string(),
            service_version: "0.1.0".to_string(),
            environment: "test".to_string(),
            batch_size: 10,
            flush_interval: Duration::from_millis(500),
            export_timeout: Duration::from_secs(1),
            enabled: true,
        };
        let exporter = OtlpExporter::new(config);

        let mut span = Span::new("http_get_assets", TraceContext::new_root(), SpanKind::Server);
        span.set_ledger_sequence(500000)
            .set_contract_id("CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU")
            .set_query_duration_ms(18.2);
        span.finish();

        let payload = exporter.build_otlp_payload(&[span]);
        let resource_spans = payload["resourceSpans"].as_array().expect("resourceSpans array");
        assert_eq!(resource_spans.len(), 1);

        let scope_spans = resource_spans[0]["scopeSpans"].as_array().expect("scopeSpans array");
        assert_eq!(scope_spans.len(), 1);

        let spans = scope_spans[0]["spans"].as_array().expect("spans array");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0]["name"], "http_get_assets");
        assert_eq!(spans[0]["kind"], 2); // Server
    }
}
