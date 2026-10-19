use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::Rng;
use reqwest::header::RETRY_AFTER;
use reqwest::StatusCode;
use serde::Deserialize;
use stellar_xdr::curr as xdr;
use stellar_xdr::curr::{Limits, ReadXdr};

use crate::indexer::{
    build_invoke_envelope, classify_rpc_error, diagnostics, scval_to_json, IndexError,
    MAX_READ_ATTEMPTS, RETRY_BASE_DELAY, RETRY_MAX_DELAY,
};

const HEALTH_WINDOW: usize = 100;
const ERROR_RATE_THRESHOLD: f64 = 0.02;
const LATENCY_THRESHOLD: Duration = Duration::from_millis(1_000);
const HEALTH_PROBE_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct RpcEnvelope {
    result: Option<SimulateResult>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    message: String,
}

#[derive(Deserialize)]
struct SimulateResult {
    #[serde(default)]
    results: Vec<SimResultEntry>,
    #[serde(default)]
    error: Option<String>,
    #[serde(rename = "latestLedger", default)]
    latest_ledger: u32,
    #[serde(default)]
    events: Vec<String>,
}

#[derive(Deserialize)]
struct SimResultEntry {
    xdr: String,
}

#[derive(Debug)]
pub(crate) struct ReadOutcome {
    pub value: serde_json::Value,
    pub latest_ledger: u32,
    pub diagnostics: Vec<crate::models::DiagnosticEventRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

struct Endpoint {
    url: String,
    state: CircuitState,
    outcomes: VecDeque<bool>,
    consecutive_failures: u32,
    last_latency: Duration,
    last_ledger: u32,
    next_attempt: Instant,
}

impl Endpoint {
    fn error_rate(&self) -> f64 {
        if self.outcomes.is_empty() {
            return 0.0;
        }
        let failures = self.outcomes.iter().filter(|success| !**success).count();
        failures as f64 / self.outcomes.len() as f64
    }

    fn score(&self, highest_ledger: u32) -> u8 {
        if self.state != CircuitState::Closed || self.is_unhealthy() {
            return 0;
        }
        let latency_factor =
            1.0 - self.last_latency.as_secs_f64() / (2.0 * LATENCY_THRESHOLD.as_secs_f64());
        let sync_lag = highest_ledger.saturating_sub(self.last_ledger);
        let sync_factor = 1.0 / (1.0 + sync_lag as f64 / 1_000.0);
        let score = 100.0 * (1.0 - self.error_rate()) * latency_factor * sync_factor;
        score.clamp(0.0, 100.0).round() as u8
    }

    fn is_unhealthy(&self) -> bool {
        self.error_rate() > ERROR_RATE_THRESHOLD || self.last_latency > LATENCY_THRESHOLD
    }

    fn record_outcome(&mut self, success: bool) {
        if self.outcomes.len() == HEALTH_WINDOW {
            self.outcomes.pop_front();
        }
        self.outcomes.push_back(success);
    }

    fn open(&mut self) {
        self.state = CircuitState::Open;
        let exponent = self.consecutive_failures.saturating_sub(1).min(6);
        let backoff_secs = 1_u64 << exponent;
        self.next_attempt = Instant::now() + Duration::from_secs(backoff_secs);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointHealth {
    pub url: String,
    pub health_score: u8,
    pub error_rate_percent: u8,
    pub last_latency: Duration,
    pub latest_ledger: u32,
    pub circuit_open: bool,
}

fn publish_health(index: usize, score: u8) {
    metrics::gauge!("rwa_rpc_endpoint_health_score", "endpoint" => index.to_string())
        .set(f64::from(score));
}

#[derive(Clone)]
pub(crate) struct RpcClient {
    http: reqwest::Client,
    endpoints: Arc<Mutex<Vec<Endpoint>>>,
    source: String,
}

impl RpcClient {
    pub fn new(urls: Vec<String>, source: String) -> Self {
        let endpoints = urls
            .into_iter()
            .map(|url| Endpoint {
                url,
                state: CircuitState::Closed,
                outcomes: VecDeque::with_capacity(HEALTH_WINDOW),
                consecutive_failures: 0,
                last_latency: Duration::from_millis(100),
                last_ledger: 0,
                next_attempt: Instant::now(),
            })
            .collect();
        RpcClient {
            http: reqwest::Client::new(),
            endpoints: Arc::new(Mutex::new(endpoints)),
            source,
        }
    }

    async fn select_endpoint(&self) -> Result<usize, IndexError> {
        loop {
            let (selected, wait) = {
                let mut endpoints = self.endpoints.lock().unwrap();
                if endpoints.is_empty() {
                    return Err(IndexError::Rpc("no Soroban RPC endpoints configured".into()));
                }
                let now = Instant::now();
                let highest_ledger = endpoints
                    .iter()
                    .map(|endpoint| endpoint.last_ledger)
                    .max()
                    .unwrap_or_default();
                let healthy: Vec<(usize, u64)> = endpoints
                    .iter()
                    .enumerate()
                    .filter_map(|(index, endpoint)| {
                        (endpoint.state == CircuitState::Closed && !endpoint.is_unhealthy())
                            .then(|| (index, u64::from(endpoint.score(highest_ledger)).max(1)))
                    })
                    .collect();

                if !healthy.is_empty() {
                    let total: u64 = healthy.iter().map(|(_, score)| score).sum();
                    let mut rng = rand::rng();
                    let mut pick = rng.random_range(0..total);
                    let selected = healthy
                        .into_iter()
                        .find_map(|(index, score)| {
                            if pick < score {
                                Some(index)
                            } else {
                                pick -= score;
                                None
                            }
                        })
                        .unwrap_or(0);
                    (Some(selected), None)
                } else if let Some((index, _)) = endpoints
                    .iter()
                    .enumerate()
                    .filter(|(_, endpoint)| {
                        endpoint.state == CircuitState::Open && now >= endpoint.next_attempt
                    })
                    .min_by_key(|(_, endpoint)| endpoint.next_attempt)
                {
                    endpoints[index].state = CircuitState::HalfOpen;
                    (Some(index), None)
                } else {
                    let next_open = endpoints
                        .iter()
                        .filter(|endpoint| endpoint.state == CircuitState::Open)
                        .map(|endpoint| endpoint.next_attempt)
                        .min();
                    let probe_in_flight = endpoints
                        .iter()
                        .any(|endpoint| endpoint.state == CircuitState::HalfOpen);
                    let wait = match (next_open, probe_in_flight) {
                        (Some(next), true) => (next - now).min(Duration::from_millis(50)),
                        (Some(next), false) => next - now,
                        (None, true) => Duration::from_millis(50),
                        (None, false) => Duration::from_millis(50),
                    };
                    (None, Some(wait.max(Duration::from_millis(1))))
                }
            };
            if let Some(index) = selected {
                return Ok(index);
            }
            tokio::time::sleep(wait.expect("selection either returns an endpoint or a delay")).await;
        }
    }

    fn record_success(&self, idx: usize, latency: Duration, ledger: u32) {
        let mut endpoints = self.endpoints.lock().unwrap();
        let highest_ledger = endpoints
            .iter()
            .map(|endpoint| endpoint.last_ledger)
            .max()
            .unwrap_or_default()
            .max(ledger);
        if let Some(ep) = endpoints.get_mut(idx) {
            if ep.state == CircuitState::HalfOpen {
                ep.outcomes.clear();
                ep.consecutive_failures = 0;
            }
            ep.record_outcome(true);
            ep.last_latency = latency;
            ep.last_ledger = ledger;
            if latency > LATENCY_THRESHOLD {
                ep.consecutive_failures = ep.consecutive_failures.saturating_add(1);
                ep.open();
            } else {
                ep.state = CircuitState::Closed;
                ep.consecutive_failures = 0;
            }
            let score = ep.score(highest_ledger);
            publish_health(idx, score);
        }
    }

    fn record_failure(&self, idx: usize) {
        let mut endpoints = self.endpoints.lock().unwrap();
        let highest_ledger = endpoints
            .iter()
            .map(|endpoint| endpoint.last_ledger)
            .max()
            .unwrap_or_default();
        if let Some(ep) = endpoints.get_mut(idx) {
            ep.record_outcome(false);
            ep.consecutive_failures = ep.consecutive_failures.saturating_add(1);
            if ep.state == CircuitState::HalfOpen || ep.is_unhealthy() {
                ep.open();
            }
            let score = ep.score(highest_ledger);
            publish_health(idx, score);
        }
    }

    pub fn endpoint_health(&self) -> Vec<EndpointHealth> {
        let endpoints = self.endpoints.lock().unwrap();
        let highest_ledger = endpoints
            .iter()
            .map(|endpoint| endpoint.last_ledger)
            .max()
            .unwrap_or_default();
        endpoints
            .iter()
            .map(|endpoint| EndpointHealth {
                url: endpoint.url.clone(),
                health_score: endpoint.score(highest_ledger),
                error_rate_percent: (endpoint.error_rate() * 100.0).ceil() as u8,
                last_latency: endpoint.last_latency,
                latest_ledger: endpoint.last_ledger,
                circuit_open: endpoint.state != CircuitState::Closed,
            })
            .collect()
    }

    pub async fn monitor(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(HEALTH_PROBE_INTERVAL);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let endpoint_count = self.endpoints.lock().unwrap().len();
                    for index in 0..endpoint_count {
                        if *shutdown.borrow() {
                            return;
                        }
                        self.probe_endpoint(index).await;
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    }

    async fn probe_endpoint(&self, index: usize) {
        let url = {
            let mut endpoints = self.endpoints.lock().unwrap();
            let Some(endpoint) = endpoints.get_mut(index) else {
                return;
            };
            match endpoint.state {
                CircuitState::HalfOpen => return,
                CircuitState::Open if Instant::now() < endpoint.next_attempt => return,
                CircuitState::Open => endpoint.state = CircuitState::HalfOpen,
                CircuitState::Closed => {}
            }
            endpoint.url.clone()
        };

        let started = Instant::now();
        let response = self
            .http
            .post(&url)
            .timeout(Duration::from_secs(5))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getHealth",
                "params": {},
            }))
            .send()
            .await;
        let result = async {
            let response = response?;
            let status = response.status();
            if !status.is_success() {
                return Err(IndexError::HttpStatus {
                    status: status.as_u16(),
                    body: response.text().await.unwrap_or_default(),
                });
            }
            let payload: serde_json::Value = response.json().await?;
            if let Some(error) = payload.get("error") {
                return Err(IndexError::Rpc(error.to_string()));
            }
            let health = payload
                .get("result")
                .ok_or_else(|| IndexError::Rpc("getHealth returned no result".into()))?;
            if health.get("status").and_then(serde_json::Value::as_str) != Some("healthy") {
                return Err(IndexError::Rpc("Soroban RPC health status is not healthy".into()));
            }
            health
                .get("latestLedger")
                .and_then(serde_json::Value::as_u64)
                .and_then(|ledger| u32::try_from(ledger).ok())
                .ok_or_else(|| IndexError::Rpc("getHealth returned no valid latestLedger".into()))
        }
        .await;

        match result {
            Ok(latest_ledger) => {
                self.record_success(index, started.elapsed(), latest_ledger);
            }
            Err(error) => {
                tracing::debug!(endpoint = %url, error = %error, "Soroban RPC health probe failed");
                self.record_failure(index);
            }
        }
    }

    pub async fn read(
        &self,
        contract: &str,
        method: &str,
        args: Vec<xdr::ScVal>,
    ) -> Result<ReadOutcome, IndexError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let idx = self.select_endpoint().await?;
            let url = {
                let endpoints = self.endpoints.lock().unwrap();
                endpoints[idx].url.clone()
            };
            let started = Instant::now();
            match self
                .read_once_inner(&url, contract, method, args.clone())
                .await
            {
                Ok(outcome) => {
                    let latency = started.elapsed();
                    metrics::histogram!("soroban_rpc_simulation_duration_seconds", "method" => method.to_string())
                        .record(latency.as_secs_f64());
                    self.record_success(idx, latency, outcome.latest_ledger);
                    return Ok(outcome);
                }
                Err(e) => {
                    metrics::counter!(
                        "rwa_rpc_failures_total",
                        "classification" => classify_rpc_error(&e),
                    )
                    .increment(1);
                    if e.is_transient() {
                        self.record_failure(idx);
                    }
                    if attempt < MAX_READ_ATTEMPTS && e.is_transient() {
                        let delay = retry_delay(attempt);
                        tracing::warn!(
                            contract,
                            method,
                            attempt,
                            url,
                            delay_ms = delay.as_millis() as u64,
                            error = %e,
                            "transient read error; retrying"
                        );
                        tokio::time::sleep(delay).await;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Issue #318: the hash of ledger `sequence`, via `getLedgers` with a
    /// one-ledger page starting at that sequence. The returned entry's
    /// sequence is checked, so a node that has pruned the ledger (or answers
    /// with a different one) is an error rather than a wrong commitment.
    pub async fn ledger_hash(&self, sequence: u32) -> Result<[u8; 32], IndexError> {
        #[derive(Deserialize)]
        struct LedgersEnvelope {
            result: Option<LedgersResult>,
            error: Option<RpcError>,
        }
        #[derive(Deserialize)]
        struct LedgersResult {
            #[serde(default)]
            ledgers: Vec<LedgerInfo>,
            #[serde(rename = "latestLedger", default)]
            latest_ledger: u32,
        }
        #[derive(Deserialize)]
        struct LedgerInfo {
            hash: String,
            sequence: u32,
        }

        let mut attempt = 0;
        loop {
            attempt += 1;
            let idx = self.select_endpoint().await?;
            let url = {
                let endpoints = self.endpoints.lock().unwrap();
                endpoints[idx].url.clone()
            };
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getLedgers",
                "params": { "startLedger": sequence, "pagination": { "limit": 1 } },
            });
            let started = Instant::now();
            let result = async {
                let response = self
                    .http
                    .post(&url)
                    .timeout(Duration::from_secs(10))
                    .json(&body)
                    .send()
                    .await?;
                let status = response.status();
                if !status.is_success() {
                    return Err(IndexError::HttpStatus {
                        status: status.as_u16(),
                        body: response.text().await.unwrap_or_default(),
                    });
                }
                let envelope: LedgersEnvelope = response.json().await?;
                if let Some(error) = envelope.error {
                    return Err(IndexError::Rpc(error.message));
                }
                let result = envelope
                    .result
                    .ok_or_else(|| IndexError::Rpc("getLedgers returned no result".into()))?;
                let latest_ledger = result.latest_ledger.max(sequence);
                let ledger = result
                    .ledgers
                    .into_iter()
                    .next()
                    .filter(|ledger| ledger.sequence == sequence)
                    .ok_or_else(|| {
                        IndexError::Rpc(format!("ledger {sequence} not returned by getLedgers"))
                    })?;
                let bytes = hex::decode(&ledger.hash)
                    .map_err(|error| IndexError::Rpc(format!("bad ledger hash: {error}")))?;
                let hash = bytes
                    .try_into()
                    .map_err(|_| IndexError::Rpc("ledger hash is not 32 bytes".into()))?;
                Ok((hash, latest_ledger))
            }
            .await;

            match result {
                Ok((hash, latest_ledger)) => {
                    self.record_success(idx, started.elapsed(), latest_ledger);
                    return Ok(hash);
                }
                Err(error) => {
                    if error.is_transient() {
                        self.record_failure(idx);
                    }
                    if attempt < MAX_READ_ATTEMPTS && error.is_transient() {
                        tokio::time::sleep(retry_delay(attempt)).await;
                    } else {
                        return Err(error);
                    }
                }
            }
        }
    }

    async fn read_once_inner(
        &self,
        url: &str,
        contract: &str,
        method: &str,
        args: Vec<xdr::ScVal>,
    ) -> Result<ReadOutcome, IndexError> {
        let envelope_b64 = build_invoke_envelope(&self.source, contract, method, args)?;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "simulateTransaction",
            "params": { "transaction": envelope_b64 },
        });

        let resp = self
            .http
            .post(url)
            .timeout(Duration::from_secs(10))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
            let headers = resp.headers().clone();
            let body = resp.text().await.unwrap_or_default();
            let retry_after = headers
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs);
            return Err(IndexError::RateLimited {
                status: status.as_u16(),
                retry_after,
                body,
            });
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(IndexError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }
        let resp: RpcEnvelope = resp.json().await?;
        if let Some(err) = resp.error {
            return Err(IndexError::Rpc(err.message));
        }
        let result = resp
            .result
            .ok_or_else(|| IndexError::Rpc("empty rpc result".into()))?;
        if let Some(sim_err) = result.error {
            return Err(IndexError::Rpc(sim_err));
        }
        let entry = result
            .results
            .first()
            .ok_or_else(|| IndexError::Rpc("no simulation result".into()))?;
        let scval = xdr::ScVal::from_xdr_base64(&entry.xdr, Limits::none())?;
        Ok(ReadOutcome {
            value: scval_to_json(&scval)?,
            latest_ledger: result.latest_ledger,
            diagnostics: diagnostics::parse_diagnostic_events(&result.events),
        })
    }
}

pub(crate) fn retry_delay(attempt: u32) -> Duration {
    let exp = RETRY_BASE_DELAY.saturating_mul(1u32 << (attempt - 1).min(4));
    let cap = exp.min(RETRY_MAX_DELAY);
    let mut rng = rand::rng();
    rng.random_range(Duration::ZERO..=cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_scores_are_bounded_and_penalize_lag_and_latency() {
        let mut endpoint = Endpoint {
            url: "https://rpc.example".into(),
            state: CircuitState::Closed,
            outcomes: VecDeque::new(),
            consecutive_failures: 0,
            last_latency: Duration::from_millis(100),
            last_ledger: 10_000,
            next_attempt: Instant::now(),
        };
        assert!(endpoint.score(10_000) <= 100);
        let synced_score = endpoint.score(10_000);
        assert!(endpoint.score(11_000) < synced_score);

        endpoint.last_latency = Duration::from_millis(1_001);
        assert_eq!(endpoint.score(10_000), 0);
        assert!(endpoint.is_unhealthy());
    }

    #[test]
    fn rolling_error_rate_only_opens_above_two_percent() {
        let mut endpoint = Endpoint {
            url: "https://rpc.example".into(),
            state: CircuitState::Closed,
            outcomes: std::iter::repeat(true).take(49).collect(),
            consecutive_failures: 0,
            last_latency: Duration::from_millis(100),
            last_ledger: 1,
            next_attempt: Instant::now(),
        };
        endpoint.record_outcome(false);
        assert!(!endpoint.is_unhealthy(), "one failure in 50 is exactly 2%");
        endpoint.record_outcome(false);
        assert!(endpoint.is_unhealthy(), "two failures in 51 exceed 2%");
    }

    #[tokio::test]
    async fn read_selection_avoids_open_nodes_and_returns_health_scores() {
        let client = RpcClient::new(
            vec![
                "https://unhealthy.example".into(),
                "https://healthy.example".into(),
            ],
            "source".into(),
        );
        client.record_failure(0);

        assert_eq!(client.select_endpoint().await.unwrap(), 1);
        let health = client.endpoint_health();
        assert_eq!(health[0].health_score, 0);
        assert!(health[0].circuit_open);
        assert!((0..=100).contains(&health[1].health_score));
    }

    #[test]
    fn failures_open_circuit_with_exponential_backoff() {
        let client = RpcClient::new(vec!["https://rpc.example".into()], "source".into());
        client.record_failure(0);
        let first_wait = client.endpoints.lock().unwrap()[0]
            .next_attempt
            .saturating_duration_since(Instant::now());
        assert!(first_wait <= Duration::from_secs(1));

        let mut endpoints = client.endpoints.lock().unwrap();
        endpoints[0].state = CircuitState::HalfOpen;
        drop(endpoints);
        client.record_failure(0);
        let second_wait = client.endpoints.lock().unwrap()[0]
            .next_attempt
            .saturating_duration_since(Instant::now());
        assert!(second_wait > Duration::from_secs(1));
        assert!(second_wait <= Duration::from_secs(2));
    }
}
