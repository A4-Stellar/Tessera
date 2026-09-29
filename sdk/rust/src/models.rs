//! Strongly-typed request and response models for the Tessera API.
//!
//! All models mirror the API's JSON representation. Large integers
//! (balances, valuations, supplies) are serialized as strings to avoid
//! JavaScript precision loss — they are deserialized as `String` here.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Core domain models
// ---------------------------------------------------------------------------

/// A recent contract event emitted by the indexer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,
    pub contract: String,
    pub event_type: String,
    pub ledger: u32,
    pub timestamp: Option<String>,
    pub data: serde_json::Value,
}

/// A structured, typed Soroban diagnostic event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticEventRecord {
    pub contract: Option<String>,
    pub event_type: String,
    pub topics: Vec<serde_json::Value>,
    pub data: serde_json::Value,
    pub in_successful_contract_call: bool,
    pub error_code: Option<u32>,
}

/// A tokenized real-world asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: u64,
    pub token_contract: String,
    pub issuer: String,
    pub name: String,
    pub symbol: String,
    pub asset_type: String,
    pub description: String,
    /// Valuation in USD cents (serialized as string for precision).
    pub valuation_cents: String,
    /// Valuation in USD (f64 convenience field).
    pub valuation_usd: f64,
    pub decimals: u32,
    /// Total supply (serialized as string for precision).
    pub total_supply: String,
    pub holders: usize,
    pub active: bool,
    pub paused: bool,
    pub compliance_contract: String,
    pub created_at_ledger: u32,
    pub indexed_at_ledger: u32,
    pub index_error: Option<String>,
}

/// A holder of a specific asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holder {
    pub address: String,
    /// Token balance (serialized as string for precision).
    pub balance: String,
    pub share_percent: f64,
}

/// An asset holding for a specific address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressHolding {
    pub address: String,
    pub asset_id: u64,
    pub asset_name: String,
    pub symbol: String,
    /// Token balance (serialized as string for precision).
    pub balance: String,
    pub share_percent: f64,
}

/// Compliance status for a specific address on a specific asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressCompliance {
    pub address: String,
    pub asset_id: u64,
    pub asset_name: String,
    pub symbol: String,
    /// Token balance (serialized as string for precision).
    pub balance: String,
    pub status: String,
    pub allowed: bool,
}

/// Compliance summary for an asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceSummary {
    pub total_records: usize,
    pub approved: usize,
    pub suspended: usize,
    pub rejected: usize,
    pub pending: usize,
    pub with_expiry: usize,
    pub jurisdictions: Vec<JurisdictionCount>,
}

/// Count of compliance records per jurisdiction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JurisdictionCount {
    pub jurisdiction: String,
    pub count: usize,
}

/// A dividend distribution for an asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Distribution {
    pub id: u64,
    pub asset_token: String,
    pub payment_token: String,
    /// Total distribution amount (serialized as string for precision).
    pub total_amount: String,
    /// Amount already claimed (serialized as string for precision).
    pub distributed: String,
    pub claimed_percent: f64,
    pub overflow_detected: bool,
    pub completed: bool,
    pub created_at_ledger: u32,
    pub fiat_equivalent_usd: Option<f64>,
}

/// Platform-wide statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub total_assets: usize,
    pub active_assets: usize,
    /// Total value locked in USD cents (serialized as string).
    pub tvl_cents: String,
    /// Total value locked in USD (f64 convenience field).
    pub tvl_usd: f64,
    pub total_holders: usize,
    pub total_distributions: usize,
    pub last_indexed_ledger: u32,
    pub last_updated: Option<String>,
}

/// API error response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub error: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Analytics models
// ---------------------------------------------------------------------------

/// Type of time window aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowType {
    /// Non-overlapping fixed time intervals.
    Tumbling,
    /// Overlapping rolling time intervals.
    Sliding,
}

/// Aggregate metrics produced for a specific time window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowMetricAggregate {
    pub asset_id: u64,
    pub window_type: WindowType,
    pub window_duration: String,
    pub window_start: String,
    pub window_end: String,
    /// Trading volume (serialized as string for precision).
    pub trading_volume: String,
    pub trading_volume_usd: f64,
    pub trade_count: u64,
    pub unique_active_traders: usize,
    pub hourly_holder_growth_rate: f64,
    pub volatility_index: f64,
    pub moving_average_price_usd: f64,
    pub open_price_usd: f64,
    pub high_price_usd: f64,
    pub low_price_usd: f64,
    pub close_price_usd: f64,
}

/// Response payload for `GET /v1/assets/:id/metrics/analytics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetAnalyticsResponse {
    pub asset_id: u64,
    pub symbol: String,
    pub current_window: WindowMetricAggregate,
    pub historical_windows: Vec<WindowMetricAggregate>,
}

// ---------------------------------------------------------------------------
// Security / anomaly models
// ---------------------------------------------------------------------------

/// The statistic a risk flag was raised on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Volume,
    Velocity,
    AllowlistChanges,
}

/// An active risk flag as exposed to maintainers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskFlag {
    pub account: String,
    pub metric: Metric,
    /// Highest Z-score seen while the flag has been active.
    pub peak_z_score: f64,
    /// Z-score of the most recent trigger.
    pub latest_z_score: f64,
    pub observed: f64,
    pub baseline_mean: f64,
    pub baseline_std: f64,
    pub first_seen: u64,
    pub last_seen: u64,
    pub trigger_count: u64,
}

/// Response for `GET /v1/security/anomalies`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomaliesResponse {
    pub threshold: f64,
    pub count: usize,
    pub flags: Vec<RiskFlag>,
}

// ---------------------------------------------------------------------------
// Audit models
// ---------------------------------------------------------------------------

/// Audit action types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditAction {
    AllowlistAdd {
        address: String,
        jurisdiction: String,
        expires_at: Option<String>,
    },
    AllowlistRemove {
        address: String,
    },
    TokenFreeze {
        address: String,
        asset_id: u64,
    },
    TokenUnfreeze {
        address: String,
        asset_id: u64,
    },
    ParameterUpdate {
        parameter: String,
        old_value: String,
        new_value: String,
    },
    EmergencyPause {
        component: String,
        reason: String,
    },
    EmergencyUnpause {
        component: String,
    },
    ContractUpgrade {
        contract_id: String,
        new_wasm_hash: String,
    },
    AdministrativeAction {
        action: String,
        details: String,
    },
}

/// A single audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub sequence: u64,
    pub timestamp: String,
    pub action: AuditAction,
    pub actor: String,
    pub target: Option<String>,
    pub payload: serde_json::Value,
    pub prev_hash: String,
    pub entry_hash: String,
}

/// A Merkle proof step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofStep {
    pub sibling_hash: String,
    pub is_sibling_left: bool,
}

/// A Merkle inclusion proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    pub leaf_index: usize,
    pub leaf_hash: String,
    pub steps: Vec<ProofStep>,
    pub root_hash: String,
}

/// An anchor record published to the blockchain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorRecord {
    pub anchor_id: u64,
    pub root_hash: String,
    pub start_sequence: u64,
    pub end_sequence: u64,
    pub entry_count: usize,
    pub ledger_sequence: u32,
    pub memo_hash: String,
    pub contract_storage_key: Option<String>,
    pub tx_hash: String,
    pub published_at: String,
}

/// Result of an audit verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditVerificationResult {
    pub is_valid: bool,
    pub total_entries: usize,
    pub genesis_hash: String,
    pub chain_tip_hash: String,
    pub merkle_root: String,
    pub anchored_root_hash: Option<String>,
    pub anchor_status: String,
    pub latest_anchor: Option<AnchorRecord>,
    pub first_tamper_index: Option<u64>,
    pub error_detail: Option<String>,
    pub verification_timestamp: String,
    pub auditor_instructions: String,
    pub inclusion_proof: Option<MerkleProof>,
}

/// Paginated audit entries response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedEntriesResponse {
    pub entries: Vec<AuditEntry>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
}

/// Detailed single entry response with cryptographic Merkle proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryDetailResponse {
    pub entry: AuditEntry,
    pub inclusion_proof: Option<MerkleProof>,
}

// ---------------------------------------------------------------------------
// Request body models
// ---------------------------------------------------------------------------

/// Request body for `POST /v1/audit/entries`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAuditEntryRequest {
    pub action: AuditAction,
    pub actor: String,
    pub target: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Request body for `POST /v1/audit/anchor`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PublishAnchorRequest {
    pub ledger_sequence: Option<u32>,
}

// ---------------------------------------------------------------------------
// Query parameter models
// ---------------------------------------------------------------------------

/// Query parameters for `GET /v1/assets`.
#[derive(Debug, Clone, Default)]
pub struct ListAssetsQuery {
    pub asset_type: Option<String>,
    pub active: Option<bool>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub fields: Option<String>,
}

/// Query parameters for `GET /v1/assets/:id`.
#[derive(Debug, Clone, Default)]
pub struct GetAssetQuery {
    pub fields: Option<String>,
}

/// Query parameters for `GET /v1/assets/:id/events`.
#[derive(Debug, Clone, Default)]
pub struct AssetEventsQuery {
    pub include_diagnostics: Option<bool>,
}

/// Query parameters for `GET /v1/assets/:id/metrics/analytics`.
#[derive(Debug, Clone, Default)]
pub struct AssetAnalyticsQuery {
    pub window: Option<String>,
    pub window_type: Option<String>,
    pub limit: Option<usize>,
}

/// Query parameters for `GET /v1/assets/:id/holders`.
#[derive(Debug, Clone, Default)]
pub struct ListHoldersQuery {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

/// Query parameters for `GET /v1/assets/:id/export`.
#[derive(Debug, Clone, Default)]
pub struct ExportQuery {
    pub format: Option<String>,
    pub ledger: Option<u32>,
}

/// Query parameters for `GET /v1/security/anomalies`.
#[derive(Debug, Clone, Default)]
pub struct AnomalyQuery {
    pub account: Option<String>,
    pub min_z: Option<f64>,
}

/// Query parameters for `GET /v1/audit/verify`.
#[derive(Debug, Clone, Default)]
pub struct VerifyQuery {
    pub entry_seq: Option<u64>,
}

/// Query parameters for `GET /v1/audit/entries`.
#[derive(Debug, Clone, Default)]
pub struct ListAuditEntriesQuery {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Sparse asset (field selection)
// ---------------------------------------------------------------------------

/// A sparse asset representation when using the `fields` query parameter.
/// All fields are optional since the API returns only requested fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseAsset {
    pub id: Option<u64>,
    pub token_contract: Option<String>,
    pub issuer: Option<String>,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub asset_type: Option<String>,
    pub description: Option<String>,
    pub valuation_cents: Option<String>,
    pub valuation_usd: Option<f64>,
    pub decimals: Option<u32>,
    pub total_supply: Option<String>,
    pub holders: Option<usize>,
    pub active: Option<bool>,
    pub paused: Option<bool>,
    pub compliance_contract: Option<String>,
    pub created_at_ledger: Option<u32>,
    pub indexed_at_ledger: Option<u32>,
    pub index_error: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A sparse asset representation for list endpoints.
/// Similar to `SparseAsset` but used when listing multiple assets with field selection.
pub type SparseAssetItem = SparseAsset;
