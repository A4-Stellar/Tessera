//! OpenTelemetry APM and Distributed Tracing Module (Issue #228).
//!
//! Provides end-to-end distributed tracing across HTTP requests, Tokio background
//! tasks, and Soroban RPC calls with W3C Trace Context propagation and OTLP export.

pub use ::tracing::*;

pub mod opentelemetry;

pub use opentelemetry::{
    OtelCollectorConfig, OtlpExporter, Span, SpanKind, SpanStatus, TraceContext, Tracer,
};
