//! SEP-24 — Hosted Deposit and Withdrawal (interactive flow).
//!
//! This module implements the small slice of SEP-24 needed for a hosted
//! deposit: discovery via `stellar.toml`, the interactive deposit initiation
//! endpoint, transaction status/list lookups, KYC collection, the internal
//! status-transition helpers used by the compliance service, and a KYC
//! re-verification hook (issue #77).
//!
//! # IMPORTANT — demo infrastructure, not production
//!
//! * The transaction store is an in-memory `RwLock<HashMap<…>>` behind a
//!   `OnceLock`. It is **volatile**: every transaction is lost on restart and
//!   it is not shared between API replicas. A production deployment MUST
//!   replace [`store`] with a durable database (and a real token store).
//! * Transaction ids and one-time tokens are generated with a timestamp plus
//!   a small xorshift PRNG. The token is **not cryptographically secure** and
//!   must be replaced by a CSPRNG (e.g. the `rand` crate) in production.
//! * [`PlaceholderSigner`] returns a literal placeholder instead of a real
//!   base64 XDR envelope. Production MUST inject a signer (see
//!   [`RemoteSigner`] / `SEP24_SIGNING_SERVICE_URL`) so users receive a real
//!   signed Stellar transaction they can submit themselves.
//! * [`register_allowlist_entry`] calls an HTTP compliance service when
//!   `COMPLIANCE_SERVICE_URL` is configured; otherwise it only records the
//!   intent and marks KYC as `needs_review`. Wire it to the Soroban
//!   compliance contract's `add_to_allowlist` in production.
//!
//! # Middleware interaction
//!
//! The global router wraps every response with the PII scrubber and the
//! anonymizer (issues #103 / #284). SEP-24 core fields (`id`, `status`,
//! `amount_in`, `amount_out`, `signed_transaction`, …) are unaffected, but
//! KYC fields such as `email`, `first_name`, `last_name`, `dob`, `tax_id` are
//! pseudonymised for non-admin callers by design. Supply the configured admin
//! bearer token (`RWA_ADMIN_TOKEN`) when an operator needs to read raw KYC.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock, RwLock},
    time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Path, Query},
    http::header,
    response::IntoResponse,
    Json,
};
#[cfg(test)]
use axum::{routing::{get, post}, Router};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::ApiError;

/// Default signing key is a valid public `G...` strkey so tests and demos do
/// not depend on environment configuration. Override with `SEP24_SIGNING_KEY`.
const DEFAULT_SIGNING_KEY: &str = "GAIQGTOBTTLLDJ4SWGGESM7UWJ2DI4K3ZNHUSHPDKJL2IE5FKY3BSRAA";
const DEFAULT_TRANSFER_SERVER: &str = "http://localhost:8080";
const DEFAULT_KYC_SERVER: &str = "http://localhost:8080";
const DEFAULT_DOCS_URL: &str = "https://docs.example.com";
const DEFAULT_MAINTAINER: &str = "Tessera Maintainers";
const DEFAULT_NETWORK: &str = "testnet";
const DEFAULT_REVERIFY_DAYS: i64 = 365;

/// Weekly cron target for issue #77 (see `tests/sep24/README.md` and the
/// docs page). An external scheduler POSTs here once a week.
pub const REVERIFY_ROUTE: &str = "/sep24/admin/reverify";
/// Companion read-only endpoint listing approvals currently overdue.
pub const REVERIFY_DUE_ROUTE: &str = "/sep24/admin/reverify/due";

/// Configuration snapshot for SEP-24, read from the environment with safe
/// defaults. Keeping this in one place makes the handlers easy to test: the
/// test router never asserts on particular env values, only on field presence.
#[derive(Clone, Debug)]
pub struct Sep24Config {
    pub signing_key: String,
    pub transfer_server_url: String,
    pub kyc_server_url: String,
    pub docs_url: String,
    pub maintainer: String,
    pub network: String,
    pub web_auth_endpoint: String,
    pub accounts: Option<String>,
    pub signing_service_url: Option<String>,
    pub compliance_service_url: Option<String>,
    pub reverify_interval_days: i64,
}

impl Sep24Config {
    /// Read a snapshot from the environment. Values fall back to the defaults
    /// above so a fresh checkout can serve `stellar.toml` without configuration.
    pub fn from_env() -> Self {
        let transfer_server_url = env_or("SEP24_TRANSFER_SERVER_URL", DEFAULT_TRANSFER_SERVER);
        let web_auth_endpoint = env_or("SEP24_WEB_AUTH_ENDPOINT", &format!("{transfer_server_url}/auth"));
        // `SEP24Docs_URL` is accepted as a legacy alias for the canonical
        // `SEP24_DOCS_URL` spelling used in the docs and test harness.
        let docs_url = env_or_alias("SEP24_DOCS_URL", "SEP24Docs_URL", DEFAULT_DOCS_URL);
        Self {
            signing_key: env_or("SEP24_SIGNING_KEY", DEFAULT_SIGNING_KEY),
            transfer_server_url,
            kyc_server_url: env_or("SEP24_KYC_SERVER_URL", DEFAULT_KYC_SERVER),
            docs_url,
            maintainer: env_or("SEP24_MAINTAINER", DEFAULT_MAINTAINER),
            network: env_or("SEP24_NETWORK", DEFAULT_NETWORK),
            web_auth_endpoint,
            accounts: optional_env("SEP24_ACCOUNTS"),
            signing_service_url: optional_env("SEP24_SIGNING_SERVICE_URL"),
            compliance_service_url: optional_env("COMPLIANCE_SERVICE_URL"),
            reverify_interval_days: env_i64("SEP24_REVERIFY_INTERVAL_DAYS", DEFAULT_REVERIFY_DAYS),
        }
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_or_alias(name: &str, alias: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var(alias).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| default.to_string())
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `1` / `true` enables the local compliance harness stub (see
/// `tests/sep24/README.md`). Never enable in production.
fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("True")
    )
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// SEP-10 network passphrase mapping. Testnet and pubnet are the two values
/// SEP-24 deployments use; anything else is treated as testnet (safe default).
fn network_passphrase(network: &str) -> &'static str {
    if network.eq_ignore_ascii_case("pubnet") || network.eq_ignore_ascii_case("public") {
        "Public Global Stellar Network ; September 2015"
    } else {
        "Test Network ; September 2015"
    }
}

/// Volatile demo transaction store. See the module docs — replace with a
/// durable database before any production use.
#[derive(Debug)]
struct Store {
    transactions: HashMap<String, Transaction>,
    next_id: u64,
}

fn store() -> &'static RwLock<Store> {
    static STORE: OnceLock<RwLock<Store>> = OnceLock::new();
    STORE.get_or_init(|| {
        RwLock::new(Store {
            transactions: HashMap::new(),
            next_id: 0,
        })
    })
}

/// A SEP-24 deposit transaction. Also the JSON shape returned inside the
/// SEP-24 `{ "transaction": {…} }` envelope. The one-time `token` is never
/// serialised back to callers (it is only returned by the deposit endpoint).
#[derive(Clone, Debug, Serialize)]
pub struct Transaction {
    pub id: String,
    /// SEP-24 `kind`: this module implements deposits only.
    pub kind: String,
    /// Public Stellar account (`G...`). Public data, not PII.
    pub account: String,
    pub asset_code: String,
    /// `incomplete` → `pending_user_transfer_start` → `pending_external` → `completed`.
    pub status: String,
    /// Every status this transaction has passed through, oldest first.
    pub status_history: Vec<String>,
    pub amount_in: Option<String>,
    pub amount_out: Option<String>,
    pub deposit_memo: Option<String>,
    pub deposit_memo_type: Option<String>,
    pub external_extra: Option<Value>,
    /// `pending`, `needs_review`, `approved`, or `reverification_required`.
    pub kyc_status: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub dob: Option<String>,
    pub country: Option<String>,
    pub id_type: Option<String>,
    pub id_number: Option<String>,
    pub tax_id: Option<String>,
    /// Collected on the interactive deposit request. May be pseudonymised by
    /// the global anonymizer middleware before reaching a non-admin client.
    pub email: Option<String>,
    pub lang: String,
    /// RFC3339 timestamp of the last KYC approval; drives issue #77 reverify.
    pub kyc_verified_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing)]
    pub token: String,
    /// Present only when signing succeeds. Never a fake XDR: the default
    /// placeholder is a clearly-labelled string and production must inject a
    /// real signer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_transaction: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DepositRequest {
    pub account: String,
    pub asset_code: String,
    pub amount: Option<String>,
    pub lang: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TransactionQuery {
    pub id: String,
    pub token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub account: String,
    pub token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KycRequest {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub dob: Option<String>,
    pub country: Option<String>,
    pub id_type: Option<String>,
    pub id_number: Option<String>,
    pub tax_id: Option<String>,
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// A `G...` account is a valid Ed25519 public-key strkey. Rejecting contracts
/// (`C...`), seeds (`S...`) and malformed strings here keeps the demo store
/// well-formed and mirrors what a real anchor would validate.
fn is_valid_account(value: &str) -> bool {
    matches!(
        stellar_strkey::Strkey::from_string(value),
        Ok(stellar_strkey::Strkey::PublicKeyEd25519(_))
    )
}

/// Generate a one-time token hex string from the timestamp and counter.
/// NOT cryptographically secure — a xorshift PRNG is used purely to avoid a
/// dependency in this demo. Production must use a CSPRNG (`rand`).
fn one_time_token(counter: u64) -> String {
    let mut state = counter
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(now_millis() as u64)
        ^ 0xD1B5_4A32_D192_ED03;
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    hex::encode(bytes)
}

/// Keep `lang` to a safe path segment so it cannot escape the docs URL.
fn safe_lang(lang: Option<String>) -> String {
    lang.filter(|v| {
        !v.is_empty()
            && v.len() <= 16
            && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
    .unwrap_or_else(|| "en".to_string())
}

fn lookup(id: &str) -> Option<Transaction> {
    store().read().unwrap().transactions.get(id).cloned()
}

/// Mutate a transaction in place and refresh `updated_at`.
fn update<F: FnOnce(&mut Transaction)>(id: &str, f: F) -> Option<Transaction> {
    let mut guard = store().write().unwrap();
    let tx = guard.transactions.get_mut(id)?;
    f(tx);
    tx.updated_at = now_rfc3339();
    Some(tx.clone())
}

fn approved_count() -> usize {
    store()
        .read()
        .unwrap()
        .transactions
        .values()
        .filter(|t| t.kyc_status == "approved")
        .count()
}

/// Issue #77: transactions whose KYC was approved longer ago than the
/// configured interval (default 365 days) and that have a parseable approval
/// timestamp. Pure function of `now` so it is easy to test.
fn due_transactions(now: DateTime<Utc>, interval_days: i64) -> Vec<Transaction> {
    let guard = store().read().unwrap();
    guard
        .transactions
        .values()
        .filter(|tx| {
            if tx.kyc_status != "approved" {
                return false;
            }
            let Some(verified_at) = tx
                .kyc_verified_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
            else {
                return false;
            };
            now.signed_duration_since(verified_at) > ChronoDuration::days(interval_days)
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Signing
// ---------------------------------------------------------------------------

/// Injectable signing interface. A real implementation returns a base64
/// Stellar transaction envelope (XDR) that the user signs and submits.
pub trait DepositSigner: Send + Sync {
    fn sign_deposit(&self, tx: &Transaction) -> Result<String, String>;
}

/// Demo signer. Returns a clearly-labelled placeholder — **not** a valid XDR
/// envelope. Production must inject a real signer via
/// `SEP24_SIGNING_SERVICE_URL` or [`RemoteSigner`].
pub struct PlaceholderSigner;

impl DepositSigner for PlaceholderSigner {
    fn sign_deposit(&self, _tx: &Transaction) -> Result<String, String> {
        Ok("BASE64_XDR_PLACEHOLDER".to_string())
    }
}

/// Signs by POSTing the transaction details to an external signing service
/// and reading `{ "signed_transaction": "<base64 XDR>" }` from the response.
///
/// The trait is intentionally synchronous for simple injection, so the HTTP
/// call runs on a short-lived Tokio runtime in a scoped thread; this avoids
/// panicking on a current-thread runtime and keeps the handler simple.
pub struct RemoteSigner {
    endpoint: String,
}

impl RemoteSigner {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

impl DepositSigner for RemoteSigner {
    fn sign_deposit(&self, tx: &Transaction) -> Result<String, String> {
        let endpoint = self.endpoint.clone();
        let payload = serde_json::to_string(&json!({
            "transaction_id": tx.id,
            "account": tx.account,
            "asset_code": tx.asset_code,
            "amount_in": tx.amount_in,
        }))
        .map_err(|e| e.to_string())?;

        std::thread::scope(|scope| {
            let handle = scope.spawn(move || -> Result<String, String> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                runtime.block_on(async move {
                    let client = reqwest::Client::builder()
                        .timeout(StdDuration::from_secs(10))
                        .build()
                        .map_err(|e| e.to_string())?;
                    let response = client
                        .post(&endpoint)
                        .header("content-type", "application/json")
                        .body(payload)
                        .send()
                        .await
                        .map_err(|e| e.to_string())?;
                    if !response.status().is_success() {
                        return Err(format!("signing service returned {}", response.status()));
                    }
                    let body: Value = response.json().await.map_err(|e| e.to_string())?;
                    body.get("signed_transaction")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| body.as_str().map(str::to_string))
                        .ok_or_else(|| "signing service response missing signed_transaction".to_string())
                })
            });
            handle
                .join()
                .map_err(|_| "signing service thread panicked".to_string())?
        })
    }
}

fn signer_from_env() -> Arc<dyn DepositSigner> {
    match optional_env("SEP24_SIGNING_SERVICE_URL") {
        Some(url) => Arc::new(RemoteSigner::new(url)),
        None => Arc::new(PlaceholderSigner),
    }
}

// ---------------------------------------------------------------------------
// Compliance hook (issue #77)
// ---------------------------------------------------------------------------

/// Result of a compliance allowlist registration attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllowlistOutcome {
    /// The compliance service accepted the `add_to_allowlist` request.
    Registered,
    /// No service was configured (or the call failed): the intent is recorded
    /// locally and KYC must be reviewed manually.
    RecordedIntent,
}

impl AllowlistOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::RecordedIntent => "needs_review",
        }
    }
}

/// Issue #77 hook — same signature for initial KYC and periodic re-verification.
///
/// In production this must invoke the Soroban compliance contract's
/// `add_to_allowlist` operation (via the trusted signer service). In this demo
/// it POSTs to `COMPLIANCE_SERVICE_URL` when configured; otherwise it records
/// the intent and the caller marks KYC `needs_review`.
pub async fn register_allowlist_entry(address: &str) -> AllowlistOutcome {
    // Local compliance harness (tests/sep24): stub the external service so
    // `sep-tests` can run without a live compliance deployment.
    if env_flag("SEP24_TEST_MODE") {
        tracing::info!(account = %address, "SEP24_TEST_MODE: stubbing allowlist registration");
        return AllowlistOutcome::Registered;
    }
    let Some(url) = optional_env("COMPLIANCE_SERVICE_URL") else {
        tracing::warn!(
            account = %address,
            "COMPLIANCE_SERVICE_URL not set; recording allowlist intent only"
        );
        return AllowlistOutcome::RecordedIntent;
    };
    let client = match reqwest::Client::builder()
        .timeout(StdDuration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(error = %err, "cannot build compliance client; recording intent");
            return AllowlistOutcome::RecordedIntent;
        }
    };
    match client
        .post(&url)
        .json(&json!({ "address": address, "action": "add_to_allowlist" }))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => AllowlistOutcome::Registered,
        Ok(response) => {
            tracing::warn!(
                status = %response.status(),
                "compliance allowlist call failed; recording intent"
            );
            AllowlistOutcome::RecordedIntent
        }
        Err(err) => {
            tracing::warn!(error = %err, "compliance allowlist call failed; recording intent");
            AllowlistOutcome::RecordedIntent
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /.well-known/stellar.toml` — anchor discovery metadata.
pub async fn stellar_toml() -> impl IntoResponse {
    let cfg = Sep24Config::from_env();
    let mut lines = vec![
        format!("SIGNING_KEY={}", cfg.signing_key),
        format!("TRANSFER_SERVER_SEP0024={}", cfg.transfer_server_url),
        format!("KYC_SERVER={}", cfg.kyc_server_url),
        format!("NETWORK_PASSPHRASE={}", network_passphrase(&cfg.network)),
        format!("WEB_AUTH_ENDPOINT={}", cfg.web_auth_endpoint),
        format!("ORG_MAINTAINER={}", cfg.maintainer),
    ];
    if let Some(accounts) = cfg.accounts {
        lines.push(format!("ACCOUNTS={accounts}"));
    }
    ([(header::CONTENT_TYPE, "text/plain")], format!("{}\n", lines.join("\n")))
}

/// `POST /sep24/transactions/deposit/interactive` — begin an interactive
/// deposit. Returns the interactive URL (SEP-24 `url` + `type`) plus the
/// transaction id and one-time token the UI needs for polling.
pub async fn interactive_deposit(
    Json(req): Json<DepositRequest>,
) -> Result<Json<Value>, ApiError> {
    if !is_valid_account(&req.account) {
        return Err(ApiError::BadRequest(
            "account must be a valid Stellar G... public key".to_string(),
        ));
    }
    if req.asset_code.trim().is_empty() {
        return Err(ApiError::BadRequest("asset_code is required".to_string()));
    }

    let lang = safe_lang(req.lang);
    let amount = req.amount.filter(|v| !v.trim().is_empty());
    let email = req.email.filter(|v| !v.trim().is_empty());

    // Volatile store: see module docs. The counter is held under the write
    // lock so ids are unique even under concurrent deposits.
    let (id, token) = {
        let mut guard = store().write().unwrap();
        guard.next_id += 1;
        let counter = guard.next_id;
        let id = format!("sep24-{}-{:08x}", now_millis(), counter);
        let token = one_time_token(counter);
        let timestamp = now_rfc3339();
        let tx = Transaction {
            id: id.clone(),
            kind: "deposit".to_string(),
            account: req.account,
            asset_code: req.asset_code,
            status: "pending_user_transfer_start".to_string(),
            status_history: vec![
                "incomplete".to_string(),
                "pending_user_transfer_start".to_string(),
            ],
            amount_in: amount,
            amount_out: None,
            deposit_memo: None,
            deposit_memo_type: None,
            external_extra: None,
            kyc_status: "not_started".to_string(),
            first_name: None,
            last_name: None,
            dob: None,
            country: None,
            id_type: None,
            id_number: None,
            tax_id: None,
            email,
            lang: lang.clone(),
            kyc_verified_at: None,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            token: token.clone(),
            signed_transaction: None,
        };
        guard.transactions.insert(id.clone(), tx);
        (id, token)
    };

    let docs_url = Sep24Config::from_env().docs_url;
    let url = format!(
        "{}/{}/sep24/deposit?transaction_id={}&token={}",
        docs_url.trim_end_matches('/'),
        lang,
        id,
        token
    );

    Ok(Json(json!({
        "id": id,
        "url": url,
        "token": token,
        "type": "interactive"
    })))
}

/// `GET /sep24/transaction?id=…&token=…` — SEP-24 transaction envelope.
pub async fn get_transaction(
    Query(query): Query<TransactionQuery>,
) -> Result<Json<Value>, ApiError> {
    let tx = lookup(&query.id)
        .ok_or_else(|| ApiError::NotFound(format!("no transaction with id {}", query.id)))?;
    if let Some(token) = query.token {
        if token != tx.token {
            return Err(ApiError::Unauthorized("invalid transaction token".to_string()));
        }
    }
    Ok(Json(json!({ "transaction": tx })))
}

/// `GET /sep24/transactions?account=<G...>&token?=` — list an account's
/// deposits. When a token is supplied only transactions carrying it are
/// returned (useful for a single interactive session).
pub async fn list_transactions(
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    if !is_valid_account(&query.account) {
        return Err(ApiError::BadRequest(
            "account must be a valid Stellar G... public key".to_string(),
        ));
    }
    let guard = store().read().unwrap();
    let mut items: Vec<&Transaction> = guard
        .transactions
        .values()
        .filter(|tx| tx.account == query.account)
        .collect();
    if let Some(token) = &query.token {
        items.retain(|tx| &tx.token == token);
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(Json(json!({ "transactions": items })))
}

/// Internal helper (called by the compliance service after it observes the
/// user's external transfer): `pending_user_transfer_start` → `pending_external`.
/// Idempotent for transactions already past that state.
pub async fn confirm_external(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let tx = update(&id, |tx| {
        if tx.status == "pending_user_transfer_start" {
            tx.status = "pending_external".to_string();
            tx.status_history.push("pending_external".to_string());
            tx.external_extra = Some(json!({
                "note": "external transfer observed by the compliance service"
            }));
        }
    })
    .ok_or_else(|| ApiError::NotFound(format!("no transaction with id {id}")))?;
    Ok(Json(json!({ "transaction": tx })))
}

/// Internal helper (called by the compliance service): `pending_external` →
/// `completed` and attach a signed transaction when signing succeeds.
///
/// A signing failure is logged but does not block completion: the accounting
/// state is settled and signing can be retried by operations. This keeps the
/// demo independent of `SEP24_SIGNING_SERVICE_URL` availability.
pub async fn complete(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let current = lookup(&id)
        .ok_or_else(|| ApiError::NotFound(format!("no transaction with id {id}")))?;
    if current.status != "pending_external" && current.status != "completed" {
        return Err(ApiError::BadRequest(format!(
            "transaction {id} is '{}'; expected 'pending_external'",
            current.status
        )));
    }

    let signed = match signer_from_env().sign_deposit(&current) {
        Ok(xdr) => Some(xdr),
        Err(err) => {
            tracing::warn!(error = %err, id = %id, "deposit signing failed; completing without XDR");
            None
        }
    };

    let tx = update(&id, |tx| {
        tx.amount_out = tx.amount_in.clone();
        tx.status = "completed".to_string();
        if !tx.status_history.iter().any(|s| s == "completed") {
            tx.status_history.push("completed".to_string());
        }
        if let Some(xdr) = signed {
            tx.signed_transaction = Some(xdr);
        }
    })
    .expect("transaction was checked above");
    Ok(Json(json!({ "transaction": tx })))
}

/// `POST /sep24/transactions/:id/kyc` — collect KYC, set status `pending`,
/// then invoke the allowlist compliance hook (issue #77). Without a configured
/// compliance service KYC is marked `needs_review` for manual processing.
pub async fn submit_kyc(
    Path(id): Path<String>,
    Json(kyc): Json<KycRequest>,
) -> Result<Json<Value>, ApiError> {
    let tx = update(&id, |tx| {
        if let Some(v) = kyc.first_name { tx.first_name = Some(v); }
        if let Some(v) = kyc.last_name { tx.last_name = Some(v); }
        if let Some(v) = kyc.dob { tx.dob = Some(v); }
        if let Some(v) = kyc.country { tx.country = Some(v); }
        if let Some(v) = kyc.id_type { tx.id_type = Some(v); }
        if let Some(v) = kyc.id_number { tx.id_number = Some(v); }
        if let Some(v) = kyc.tax_id { tx.tax_id = Some(v); }
        tx.kyc_status = "pending".to_string();
    })
    .ok_or_else(|| ApiError::NotFound(format!("no transaction with id {id}")))?;

    let outcome = register_allowlist_entry(&tx.account).await;
    if outcome == AllowlistOutcome::RecordedIntent {
        update(&id, |tx| tx.kyc_status = "needs_review".to_string());
    }

    let tx = lookup(&id).expect("transaction still exists");
    Ok(Json(json!({
        "transaction": tx,
        "allowlist": outcome.as_str()
    })))
}

/// `POST /sep24/admin/reverify` — issue #77. Intended to be invoked weekly by
/// an external cron. Scans approved KYC records older than
/// `SEP24_REVERIFY_INTERVAL_DAYS` (default 365), re-runs the allowlist hook,
/// and marks them `reverification_required`.
pub async fn reverify() -> Result<Json<Value>, ApiError> {
    let cfg = Sep24Config::from_env();
    let due = due_transactions(Utc::now(), cfg.reverify_interval_days);
    let mut marked = Vec::new();
    for tx in &due {
        let _outcome = register_allowlist_entry(&tx.account).await;
        let _ = update(&tx.id, |t| {
            t.kyc_status = "reverification_required".to_string();
        });
        marked.push(tx.id.clone());
    }
    Ok(Json(json!({
        "checked": approved_count(),
        "marked": marked.len(),
        "ids": marked
    })))
}

/// `GET /sep24/admin/reverify/due` — list KYC approvals currently overdue for
/// re-verification without mutating them. Useful for cron dashboards.
pub async fn reverify_due() -> Result<Json<Value>, ApiError> {
    let cfg = Sep24Config::from_env();
    let due = due_transactions(Utc::now(), cfg.reverify_interval_days);
    Ok(Json(json!({ "transactions": due })))
}

/// Build the standalone SEP-24 router. `routes::router` registers the same
/// paths on the top-level API router; this helper keeps the inline tests in
/// sync without pulling in the global indexer state or middlewares.
#[cfg(test)]
fn test_router() -> Router {
    Router::new()
        .route("/.well-known/stellar.toml", get(stellar_toml))
        .route(
            "/sep24/transactions/deposit/interactive",
            post(interactive_deposit),
        )
        .route("/sep24/transaction", get(get_transaction))
        .route("/sep24/transactions", get(list_transactions))
        .route(
            "/sep24/transactions/:id/confirm_external",
            post(confirm_external),
        )
        .route("/sep24/transactions/:id/complete", post(complete))
        .route("/sep24/transactions/:id/kyc", post(submit_kyc))
        .route(REVERIFY_ROUTE, post(reverify))
        .route(REVERIFY_DUE_ROUTE, get(reverify_due))
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;

    use super::*;

    const ACCOUNT: &str = "GAIQGTOBTTLLDJ4SWGGESM7UWJ2DI4K3ZNHUSHPDKJL2IE5FKY3BSRAA";

    async fn send(uri: &str, method: &str, body: Option<Value>) -> (StatusCode, Value) {
        let builder = Request::builder().uri(uri).method(method);
        let request = match body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(value.to_string())),
            None => builder.body(Body::empty()),
        }
        .unwrap();
        let response = test_router().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn deposit() -> Value {
        let (status, body) = send(
            "/sep24/transactions/deposit/interactive",
            "POST",
            Some(json!({
                "account": ACCOUNT,
                "asset_code": "USDC",
                "amount": "100",
                "lang": "en",
                "email": "depositor@example.com"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "deposit failed: {body}");
        body
    }

    #[tokio::test]
    async fn stellar_toml_has_required_fields() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .uri("/.well-known/stellar.toml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/plain");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        for field in [
            "SIGNING_KEY=",
            "TRANSFER_SERVER_SEP0024=",
            "KYC_SERVER=",
            "NETWORK_PASSPHRASE=",
            "WEB_AUTH_ENDPOINT=",
        ] {
            assert!(text.contains(field), "stellar.toml missing {field}: {text}");
        }
    }

    #[tokio::test]
    async fn deposit_returns_interactive_url_and_pending_transaction() {
        let created = deposit().await;
        assert_eq!(created["type"], "interactive");
        let id = created["id"].as_str().unwrap();
        let token = created["token"].as_str().unwrap();
        let url = created["url"].as_str().unwrap();
        assert!(url.contains(&format!("transaction_id={id}")));
        assert!(url.contains(&format!("token={token}")));
        assert!(url.contains("/en/sep24/deposit"));

        let (status, body) = send(
            &format!("/sep24/transaction?id={id}&token={token}"),
            "GET",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["transaction"]["status"], "pending_user_transfer_start");
        assert_eq!(body["transaction"]["amount_in"], "100");
        assert_eq!(body["transaction"]["kyc_status"], "not_started");
        // The one-time token is never returned inside the transaction object.
        assert!(body["transaction"].get("token").is_none());
    }

    #[tokio::test]
    async fn transaction_status_transitions_to_completed() {
        let created = deposit().await;
        let id = created["id"].as_str().unwrap();
        let token = created["token"].as_str().unwrap();

        let (status, body) = send(
            &format!("/sep24/transactions/{id}/confirm_external"),
            "POST",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["transaction"]["status"], "pending_external");

        let (status, body) = send(
            &format!("/sep24/transactions/{id}/complete"),
            "POST",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["transaction"]["status"], "completed");
        assert_eq!(body["transaction"]["amount_out"], "100");
        // Default signer supplies a clearly-labelled placeholder string.
        assert_eq!(
            body["transaction"]["signed_transaction"],
            "BASE64_XDR_PLACEHOLDER"
        );

        let (status, body) = send(
            &format!("/sep24/transaction?id={id}&token={token}"),
            "GET",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["transaction"]["status"], "completed");
    }

    #[tokio::test]
    async fn list_by_account_and_bad_token() {
        let created = deposit().await;
        let id = created["id"].as_str().unwrap();
        let token = created["token"].as_str().unwrap();

        let (status, body) = send(
            &format!("/sep24/transactions?account={ACCOUNT}&token={token}"),
            "GET",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let items = body["transactions"].as_array().unwrap();
        assert!(items.iter().any(|tx| tx["id"] == id));

        let (status, _) = send(
            &format!("/sep24/transaction?id={id}&token=wrong-token"),
            "GET",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, _) = send("/sep24/transaction?id=missing", "GET", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = send(
            "/sep24/transactions/deposit/interactive",
            "POST",
            Some(json!({ "account": "not-a-key", "asset_code": "USDC" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = send(
            "/sep24/transactions?account=not-a-key",
            "GET",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn kyc_flow_sets_status_and_needs_review_without_service() {
        // Ensure the compliance hook takes its "record intent" branch even if
        // a developer has the variable set in their shell.
        std::env::remove_var("COMPLIANCE_SERVICE_URL");
        std::env::remove_var("SEP24_TEST_MODE");
        let created = deposit().await;
        let id = created["id"].as_str().unwrap();
        let token = created["token"].as_str().unwrap();

        let (status, body) = send(
            &format!("/sep24/transactions/{id}/kyc"),
            "POST",
            Some(json!({
                "first_name": "Ada",
                "last_name": "Lovelace",
                "dob": "1815-12-10",
                "country": "GB",
                "id_type": "passport",
                "id_number": "P123",
                "tax_id": "GB123"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["allowlist"], "needs_review");
        assert_eq!(body["transaction"]["kyc_status"], "needs_review");
        assert_eq!(body["transaction"]["first_name"], "Ada");

        let (status, body) = send(
            &format!("/sep24/transaction?id={id}&token={token}"),
            "GET",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["transaction"]["last_name"], "Lovelace");
    }

    #[tokio::test]
    async fn reverify_marks_overdue_kyc_approvals() {
        std::env::remove_var("COMPLIANCE_SERVICE_URL");
        std::env::remove_var("SEP24_TEST_MODE");
        std::env::set_var("SEP24_REVERIFY_INTERVAL_DAYS", "30");
        let created = deposit().await;
        let id = created["id"].as_str().unwrap();

        // Simulate an approval from 60 days ago using the module-private
        // helpers; no public "approve" endpoint is part of SEP-24.
        let old = (Utc::now() - ChronoDuration::days(60)).to_rfc3339();
        update(&id, |tx| {
            tx.kyc_status = "approved".to_string();
            tx.kyc_verified_at = Some(old);
        });

        let (status, body) = send("/sep24/admin/reverify/due", "GET", None).await;
        assert_eq!(status, StatusCode::OK);
        let due = body["transactions"].as_array().unwrap();
        assert!(due.iter().any(|tx| tx["id"] == id));

        let (status, body) = send("/sep24/admin/reverify", "POST", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["marked"], 1);
        assert!(body["ids"].as_array().unwrap().iter().any(|v| v == id));

        assert_eq!(lookup(&id).unwrap().kyc_status, "reverification_required");
        std::env::remove_var("SEP24_REVERIFY_INTERVAL_DAYS");
    }
}
