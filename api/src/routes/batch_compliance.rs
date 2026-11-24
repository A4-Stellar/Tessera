//! Cross-asset batch compliance check (issue #457).
//!
//! Pure request handling for `POST /v1/compliance/batch-check`: validation,
//! grouping by asset, per-contract circuit breaking, and NDJSON framing. The
//! Soroban `is_allowed` call is supplied through [`AllowedChecker`] so the
//! RPC transport stays in the indexer layer.

use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

/// Maximum number of checks accepted in one request.
pub const MAX_BATCH: usize = 1_000;
/// Error-rate threshold above which a contract's circuit opens.
pub const ERROR_RATE_THRESHOLD: f64 = 0.20;
/// Sliding window for error-rate measurement, and how long an open circuit stays open.
pub const CIRCUIT_WINDOW: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Deserialize)]
pub struct CheckRequest {
    pub from: String,
    pub to: String,
    pub asset_id: u64,
    pub amount: String,
}

#[derive(Debug, Deserialize)]
pub struct BatchRequest {
    pub checks: Vec<CheckRequest>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Allowed,
    Denied,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CheckResult {
    pub index: usize,
    pub asset_id: u64,
    pub status: CheckStatus,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BatchError {
    Empty,
    TooLarge(usize),
}

pub fn validate(req: &BatchRequest) -> Result<(), BatchError> {
    match req.checks.len() {
        0 => Err(BatchError::Empty),
        n if n > MAX_BATCH => Err(BatchError::TooLarge(n)),
        _ => Ok(()),
    }
}

/// Group check indices by asset, so each contract is simulated once per group.
pub fn group_by_asset(checks: &[CheckRequest]) -> BTreeMap<u64, Vec<usize>> {
    let mut groups: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (index, check) in checks.iter().enumerate() {
        groups.entry(check.asset_id).or_default().push(index);
    }
    groups
}

/// One NDJSON line (newline-terminated).
pub fn to_ndjson_line(result: &CheckResult) -> String {
    let mut line = serde_json::to_string(result).unwrap_or_default();
    line.push('\n');
    line
}

/// Performs one `is_allowed` simulation against the asset's compliance contract.
pub trait AllowedChecker: Send + Sync {
    fn is_allowed(
        &self,
        check: &CheckRequest,
    ) -> impl Future<Output = Result<bool, String>> + Send;
}

/// Sliding-window error tracker for one contract.
#[derive(Debug, Default)]
pub struct CircuitBreaker {
    outcomes: VecDeque<(Instant, bool)>,
    open_until: Option<Instant>,
}

impl CircuitBreaker {
    fn prune(&mut self, now: Instant) {
        while let Some((at, _)) = self.outcomes.front() {
            if now.duration_since(*at) > CIRCUIT_WINDOW {
                self.outcomes.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn record(&mut self, now: Instant, failed: bool) {
        self.prune(now);
        self.outcomes.push_back((now, failed));
    }

    /// Whether calls to this contract should be short-circuited right now.
    pub fn is_open(&mut self, now: Instant) -> bool {
        if let Some(until) = self.open_until {
            if now < until {
                return true;
            }
            self.open_until = None;
            self.outcomes.clear();
        }
        self.prune(now);
        let total = self.outcomes.len();
        if total == 0 {
            return false;
        }
        let failures = self.outcomes.iter().filter(|(_, failed)| *failed).count();
        if failures as f64 / total as f64 > ERROR_RATE_THRESHOLD {
            self.open_until = Some(now + CIRCUIT_WINDOW);
            return true;
        }
        false
    }
}

/// Response accounting returned alongside the streamed results.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BatchStats {
    pub rpc_calls: usize,
}

/// Run a validated batch and return one result per check, in request order,
/// together with the RPC call count (for `X-RPC-Calls`).
pub async fn run_batch<C: AllowedChecker>(
    checker: &C,
    checks: &[CheckRequest],
    breakers: &mut BTreeMap<u64, CircuitBreaker>,
) -> (Vec<CheckResult>, BatchStats) {
    let mut results: Vec<Option<CheckResult>> = vec![None; checks.len()];
    let mut stats = BatchStats::default();

    for (asset_id, indices) in group_by_asset(checks) {
        let breaker = breakers.entry(asset_id).or_default();
        for index in indices {
            let now = Instant::now();
            let status = if breaker.is_open(now) {
                CheckStatus::Unavailable
            } else {
                stats.rpc_calls += 1;
                match checker.is_allowed(&checks[index]).await {
                    Ok(true) => {
                        breaker.record(now, false);
                        CheckStatus::Allowed
                    }
                    Ok(false) => {
                        breaker.record(now, false);
                        CheckStatus::Denied
                    }
                    Err(_) => {
                        breaker.record(now, true);
                        CheckStatus::Unavailable
                    }
                }
            };
            results[index] = Some(CheckResult {
                index,
                asset_id,
                status,
            });
        }
    }

    (results.into_iter().flatten().collect(), stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Result<bool, String>);

    impl AllowedChecker for Fixed {
        fn is_allowed(
            &self,
            _check: &CheckRequest,
        ) -> impl Future<Output = Result<bool, String>> + Send {
            let out = self.0.clone();
            async move { out }
        }
    }

    fn check(asset_id: u64) -> CheckRequest {
        CheckRequest {
            from: "GA".into(),
            to: "GB".into(),
            asset_id,
            amount: "1".into(),
        }
    }

    #[test]
    fn rejects_empty_and_oversized_batches() {
        assert_eq!(validate(&BatchRequest { checks: vec![] }), Err(BatchError::Empty));
        let big = BatchRequest {
            checks: (0..=MAX_BATCH as u64).map(check).collect(),
        };
        assert_eq!(validate(&big), Err(BatchError::TooLarge(MAX_BATCH + 1)));
    }

    #[test]
    fn groups_indices_by_asset() {
        let checks = vec![check(1), check(2), check(1)];
        let groups = group_by_asset(&checks);
        assert_eq!(groups[&1], vec![0, 2]);
        assert_eq!(groups[&2], vec![1]);
    }

    #[tokio::test]
    async fn results_keep_request_order_and_count_rpc_calls() {
        let checks = vec![check(1), check(2), check(1)];
        let mut breakers = BTreeMap::new();
        let (results, stats) = run_batch(&Fixed(Ok(true)), &checks, &mut breakers).await;
        let indices: Vec<usize> = results.iter().map(|r| r.index).collect();
        assert_eq!(indices, vec![0, 1, 2]);
        assert!(results.iter().all(|r| r.status == CheckStatus::Allowed));
        assert_eq!(stats.rpc_calls, 3);
    }

    #[tokio::test]
    async fn failing_contract_opens_circuit_and_reports_unavailable() {
        let checks: Vec<_> = (0..10).map(|_| check(9)).collect();
        let mut breakers = BTreeMap::new();
        let (results, stats) =
            run_batch(&Fixed(Err("timeout".into())), &checks, &mut breakers).await;
        assert!(results.iter().all(|r| r.status == CheckStatus::Unavailable));
        assert!(stats.rpc_calls < checks.len(), "open circuit must skip RPC calls");
    }

    #[test]
    fn ndjson_line_is_newline_terminated_json() {
        let line = to_ndjson_line(&CheckResult {
            index: 0,
            asset_id: 4,
            status: CheckStatus::Denied,
        });
        assert_eq!(line, "{\"index\":0,\"asset_id\":4,\"status\":\"denied\"}\n");
    }
}
