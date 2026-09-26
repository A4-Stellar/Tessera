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
    failures: u32,
    successes: u32,
    last_latency: Duration,
    last_ledger: u32,
    next_attempt: Instant,
}

impl Endpoint {
    fn score(&self) -> u64 {
        if self.state == CircuitState::Open {
            return 0;
        }
        let total = self.failures + self.successes;
        let error_rate = if total > 0 {
            self.failures as f64 / total as f64
        } else {
            0.0
        };
        let latency_ms = self.last_latency.as_millis().max(1) as f64;
        let ledger_score = self.last_ledger.max(1) as f64;
        let score = (ledger_score / latency_ms) * (1.0 - error_rate);
        (score * 1000.0) as u64
    }
}

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
                failures: 0,
                successes: 0,
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

    fn select_endpoint(&self) -> usize {
        let mut endpoints = self.endpoints.lock().unwrap();
        let now = Instant::now();
        for ep in endpoints.iter_mut() {
            if ep.state == CircuitState::Open && now >= ep.next_attempt {
                ep.state = CircuitState::HalfOpen;
            }
        }
        let mut total_score = 0;
        let mut scores = Vec::with_capacity(endpoints.len());
        for ep in endpoints.iter() {
            if ep.state == CircuitState::Open {
                scores.push(0);
                continue;
            }
            let score = ep.score().max(1);
            scores.push(score);
            total_score += score;
        }
        if total_score == 0 {
            let mut rng = rand::rng();
            return rng.random_range(0..endpoints.len());
        }
        let mut rng = rand::rng();
        let mut pick = rng.random_range(0..total_score);
        for (i, &score) in scores.iter().enumerate() {
            if score == 0 {
                continue;
            }
            if pick < score {
                return i;
            }
            pick -= score;
        }
        0
    }

    fn record_success(&self, idx: usize, latency: Duration, ledger: u32) {
        let mut endpoints = self.endpoints.lock().unwrap();
        if let Some(ep) = endpoints.get_mut(idx) {
            ep.state = CircuitState::Closed;
            ep.failures = 0;
            ep.successes = ep.successes.saturating_add(1);
            ep.last_latency = latency;
            ep.last_ledger = ep.last_ledger.max(ledger);
        }
    }

    fn record_failure(&self, idx: usize) {
        let mut endpoints = self.endpoints.lock().unwrap();
        if let Some(ep) = endpoints.get_mut(idx) {
            ep.failures = ep.failures.saturating_add(1);
            if ep.state == CircuitState::HalfOpen || ep.failures >= 3 {
                ep.state = CircuitState::Open;
                let backoff_secs = 2_u64.pow(ep.failures.min(6));
                ep.next_attempt = Instant::now() + Duration::from_secs(backoff_secs);
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
            let idx = self.select_endpoint();
            let url = {
                let endpoints = self.endpoints.lock().unwrap();
                endpoints[idx].url.clone()
            };
            let started = Instant::now();
            match self.read_once_inner(&url, contract, method, args.clone()).await {
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
                    self.record_failure(idx);
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

        let resp = self.http.post(url).json(&body).send().await?;
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
