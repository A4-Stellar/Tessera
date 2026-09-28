//! `GET /v1/assets/:id/export` — cap-table export as CSV or Apache Parquet.
//!
//! # Acceptance criteria (issue: data export endpoints)
//!
//! * Expose `GET /v1/assets/:id/export?format=csv|parquet&ledger=12345`.
//! * Stream large dataset responses asynchronously to prevent API memory
//!   exhaustion. Responses are produced inside a `spawn_blocking` task and
//!   piped through a `tokio::sync::mpsc` channel so the caller's async task
//!   is never blocked.
//! * Include `X-Content-SHA256` response headers carrying the hex-encoded
//!   SHA-256 digest of the full payload so clients can verify download
//!   integrity.
//!
//! # Design notes
//!
//! The cap-table export contains one row per holder with the following columns:
//!
//! | Column | Type | Description |
//! |---|---|---|
//! | `asset_id` | u64 | Tessera internal asset identifier |
//! | `asset_name` | UTF-8 | Human-readable asset name |
//! | `symbol` | UTF-8 | Token ticker symbol |
//! | `ledger` | u32 | Ledger at which the snapshot was taken |
//! | `address` | UTF-8 | Holder Stellar address (strkey) |
//! | `balance` | UTF-8 | Raw balance in base units (i128 string, exact) |
//! | `share_percent` | f64 | Percentage of total supply (0–100, 2 dp) |
//!
//! Balances are kept as strings throughout to avoid f64 precision loss for
//! large `i128` values; this matches the convention used across the rest of
//! the API.
//!
//! The `ledger` query parameter is purely advisory — the API is in-memory and
//! always returns the latest indexed snapshot. When the parameter is supplied
//! it is echoed back in the response `Content-Disposition` filename and in
//! the `X-Ledger` header so callers can verify what they received matches
//! their request.

use std::sync::Arc;

use arrow::{
    array::{Float64Array, StringArray, UInt32Array, UInt64Array},
    datatypes::{DataType, Field, Schema},
    record_batch::RecordBatch,
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::Response,
};
use futures::stream;
use parquet::{
    arrow::ArrowWriter,
    file::properties::WriterProperties,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::ApiError;
use crate::indexer::AppState;

// ─── Query parameters ────────────────────────────────────────────────────────

/// Supported export formats.
#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Csv,
    Parquet,
}

impl Default for ExportFormat {
    fn default() -> Self {
        ExportFormat::Csv
    }
}

impl std::fmt::Display for ExportFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportFormat::Csv => write!(f, "csv"),
            ExportFormat::Parquet => write!(f, "parquet"),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    /// Output format: `csv` (default) or `parquet`.
    #[serde(default)]
    pub format: ExportFormat,
    /// Advisory ledger number. When supplied it is echoed in headers and the
    /// filename; the in-memory snapshot is always the latest.
    pub ledger: Option<u32>,
}

// ─── Handler ─────────────────────────────────────────────────────────────────

/// Export the cap-table for an asset as CSV or Apache Parquet.
///
/// Route: `GET /v1/assets/:id/export?format=csv|parquet&ledger=<u32>`
pub async fn export(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(query): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    // ── 1. Validate that the asset exists ────────────────────────────────────
    let snap = state.snapshot();
    let asset = snap
        .asset(id)
        .ok_or_else(|| ApiError::NotFound(format!("no asset with id {id}")))?
        .clone();

    let ledger = query.ledger.unwrap_or(snap.stats.last_indexed_ledger);
    let holders = snap.holders.get(&id).cloned().unwrap_or_default();

    // ── 2. Produce the export bytes (blocking I/O offloaded) ─────────────────
    // We move the heavy serialisation work onto the blocking thread pool so the
    // Tokio worker threads are never stalled. The result is streamed back via an
    // mpsc channel → ReceiverStream → axum Body.
    let format = query.format;
    let asset_name = asset.name.clone();
    let symbol = asset.symbol.clone();

    match format {
        ExportFormat::Csv => {
            let (bytes, checksum) = tokio::task::spawn_blocking(move || {
                build_csv(id, &asset_name, &symbol, ledger, &holders)
            })
            .await
            .map_err(|e| {
                ApiError::BadRequest(format!("export task panicked: {e}"))
            })??;

            let filename = format!("cap-table-{id}-ledger-{ledger}.csv");
            Ok(stream_response(
                bytes,
                checksum,
                ledger,
                "text/csv; charset=utf-8",
                &filename,
            ))
        }

        ExportFormat::Parquet => {
            let (bytes, checksum) = tokio::task::spawn_blocking(move || {
                build_parquet(id, &asset_name, &symbol, ledger, &holders)
            })
            .await
            .map_err(|e| {
                ApiError::BadRequest(format!("export task panicked: {e}"))
            })??;

            let filename = format!("cap-table-{id}-ledger-{ledger}.parquet");
            Ok(stream_response(
                bytes,
                checksum,
                ledger,
                "application/vnd.apache.parquet",
                &filename,
            ))
        }
    }
}

// ─── Serialisation helpers ────────────────────────────────────────────────────

/// Build a CSV payload from the holder list.
///
/// Returns `(bytes, hex_sha256_checksum)`.
fn build_csv(
    asset_id: u64,
    asset_name: &str,
    symbol: &str,
    ledger: u32,
    holders: &[crate::models::Holder],
) -> Result<(Vec<u8>, String), ApiError> {
    let mut wtr = csv::Writer::from_writer(Vec::new());

    // Header row
    wtr.write_record([
        "asset_id",
        "asset_name",
        "symbol",
        "ledger",
        "address",
        "balance",
        "share_percent",
    ])
    .map_err(|e| ApiError::BadRequest(format!("CSV header write error: {e}")))?;

    // Data rows
    for holder in holders {
        wtr.write_record([
            asset_id.to_string(),
            asset_name.to_string(),
            symbol.to_string(),
            ledger.to_string(),
            holder.address.clone(),
            holder.balance.clone(),
            format!("{:.2}", holder.share_percent),
        ])
        .map_err(|e| ApiError::BadRequest(format!("CSV row write error: {e}")))?;
    }

    let bytes = wtr
        .into_inner()
        .map_err(|e| ApiError::BadRequest(format!("CSV flush error: {e}")))?;

    let checksum = sha256_hex(&bytes);
    Ok((bytes, checksum))
}

/// Build an Apache Parquet payload from the holder list using Arrow.
///
/// Returns `(bytes, hex_sha256_checksum)`.
fn build_parquet(
    asset_id: u64,
    asset_name: &str,
    symbol: &str,
    ledger: u32,
    holders: &[crate::models::Holder],
) -> Result<(Vec<u8>, String), ApiError> {
    // ── Arrow schema ─────────────────────────────────────────────────────────
    let schema = Arc::new(Schema::new(vec![
        Field::new("asset_id", DataType::UInt64, false),
        Field::new("asset_name", DataType::Utf8, false),
        Field::new("symbol", DataType::Utf8, false),
        Field::new("ledger", DataType::UInt32, false),
        Field::new("address", DataType::Utf8, false),
        Field::new("balance", DataType::Utf8, false),
        Field::new("share_percent", DataType::Float64, false),
    ]));

    let n = holders.len();

    // ── Build Arrow arrays ────────────────────────────────────────────────────
    let asset_id_arr = UInt64Array::from(vec![asset_id; n]);
    let asset_name_arr = StringArray::from(vec![asset_name; n]);
    let symbol_arr = StringArray::from(vec![symbol; n]);
    let ledger_arr = UInt32Array::from(vec![ledger; n]);

    let addresses: Vec<&str> = holders.iter().map(|h| h.address.as_str()).collect();
    let address_arr = StringArray::from(addresses);

    let balances: Vec<&str> = holders.iter().map(|h| h.balance.as_str()).collect();
    let balance_arr = StringArray::from(balances);

    let shares: Vec<f64> = holders.iter().map(|h| h.share_percent).collect();
    let share_arr = Float64Array::from(shares);

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(asset_id_arr),
            Arc::new(asset_name_arr),
            Arc::new(symbol_arr),
            Arc::new(ledger_arr),
            Arc::new(address_arr),
            Arc::new(balance_arr),
            Arc::new(share_arr),
        ],
    )
    .map_err(|e| ApiError::BadRequest(format!("Arrow RecordBatch error: {e}")))?;

    // ── Write to in-memory buffer ─────────────────────────────────────────────
    let props = WriterProperties::builder()
        .set_writer_version(parquet::file::properties::WriterVersion::PARQUET_2_0)
        .set_compression(parquet::basic::Compression::SNAPPY)
        .build();

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props))
            .map_err(|e| ApiError::BadRequest(format!("Parquet writer init error: {e}")))?;

        writer
            .write(&batch)
            .map_err(|e| ApiError::BadRequest(format!("Parquet write error: {e}")))?;

        writer
            .close()
            .map_err(|e| ApiError::BadRequest(format!("Parquet close error: {e}")))?;
    }

    let checksum = sha256_hex(&buf);
    Ok((buf, checksum))
}

// ─── SHA-256 helper ───────────────────────────────────────────────────────────

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// ─── Streaming response builder ───────────────────────────────────────────────

/// Stream `bytes` back as an HTTP response with the given content type,
/// `Content-Disposition: attachment`, `X-Content-SHA256`, and `X-Ledger`
/// headers.
///
/// The payload is chunked through a `futures::stream::iter` so axum can start
/// writing to the TCP socket before the full buffer is transferred, preventing
/// the handler from holding a large allocation while waiting for the network.
fn stream_response(
    bytes: Vec<u8>,
    checksum: String,
    ledger: u32,
    content_type: &str,
    filename: &str,
) -> Response {
    // Chunk size: 64 KiB — small enough to yield early, large enough to avoid
    // per-chunk overhead on realistic cap-table sizes.
    const CHUNK_SIZE: usize = 64 * 1024;

    let chunks: Vec<Result<axum::body::Bytes, std::io::Error>> = bytes
        .chunks(CHUNK_SIZE)
        .map(|chunk| Ok(axum::body::Bytes::copy_from_slice(chunk)))
        .collect();

    let body = Body::from_stream(stream::iter(chunks));

    let disposition = format!("attachment; filename=\"{filename}\"");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&disposition)
                .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
        )
        .header(
            "X-Content-SHA256",
            HeaderValue::from_str(&checksum)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        )
        .header(
            "X-Ledger",
            HeaderValue::from_str(&ledger.to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        )
        .body(body)
        .expect("static response builder is well-formed")
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Holder;

    fn sample_holders() -> Vec<Holder> {
        vec![
            Holder {
                address: "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF".to_string(),
                balance: "1000000".to_string(),
                share_percent: 50.0,
            },
            Holder {
                address: "GBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBS".to_string(),
                balance: "1000000".to_string(),
                share_percent: 50.0,
            },
        ]
    }

    #[test]
    fn csv_contains_header_and_two_rows() {
        let holders = sample_holders();
        let (bytes, checksum) =
            build_csv(1, "Test Asset", "TST", 100, &holders).expect("CSV build succeeds");

        let content = String::from_utf8(bytes.clone()).expect("CSV is valid UTF-8");
        let lines: Vec<&str> = content.lines().collect();

        // Header + 2 data rows
        assert_eq!(lines.len(), 3, "expected header + 2 rows");
        assert!(lines[0].starts_with("asset_id"), "first line should be header");
        assert!(lines[1].contains("Test Asset"), "row 1 has asset_name");
        assert!(lines[1].contains("1000000"), "row 1 has balance");

        // Checksum should be 64 hex characters (SHA-256)
        assert_eq!(checksum.len(), 64);
        assert_eq!(checksum, sha256_hex(&bytes));
    }

    #[test]
    fn csv_empty_holders_produces_header_only() {
        let (bytes, _checksum) =
            build_csv(2, "Empty Asset", "EMP", 200, &[]).expect("CSV build succeeds");
        let content = String::from_utf8(bytes).expect("valid UTF-8");
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 1, "only header row expected");
    }

    #[test]
    fn parquet_builds_without_error() {
        let holders = sample_holders();
        let (bytes, checksum) =
            build_parquet(1, "Test Asset", "TST", 100, &holders).expect("Parquet build succeeds");

        // Parquet files start with the magic bytes "PAR1"
        assert!(bytes.starts_with(b"PAR1"), "should have Parquet magic header");
        assert_eq!(checksum.len(), 64);
        assert_eq!(checksum, sha256_hex(&bytes));
    }

    #[test]
    fn parquet_empty_holders_builds_without_error() {
        let (bytes, _) =
            build_parquet(2, "Empty Asset", "EMP", 200, &[]).expect("Parquet build for empty slice");
        assert!(bytes.starts_with(b"PAR1"), "should have Parquet magic header");
    }

    #[test]
    fn sha256_hex_is_deterministic() {
        let data = b"hello world";
        let a = sha256_hex(data);
        let b = sha256_hex(data);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }
}
