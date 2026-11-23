//! Grafana OnCall Integration & Automated P1 Triage Runbook Executor (Issue #474).
//!
//! Implements:
//! 1. **Runbook URL Validation**: Verifies that every Prometheus alert carries a
//!    `runbook_url` annotation pointing to `docs/runbooks/*.md`.
//! 2. **Grafana OnCall Escalation Chain**:
//!    `notify(channel)` (0 min) -> `5 min` -> `notify(on-call engineer)` -> `15 min` -> `notify(manager)`.
//! 3. **Automated P1 Triage Runbook Execution**:
//!    Executes `check_postgres_connection_count`, `restart_stuck_indexer_pod`, and
//!    `flush_redis_cache` with per-step `tokio::time::timeout` and automatic
//!    `rollback_step` on failure or timeout.
//! 4. **Structured Slack Output**: Posts `{action, outcome, duration_ms, next_step}`
//!    for every executed runbook step.
//! 5. **Immutable Audit Trail**: Appends every triage action, rollback, Slack post,
//!    and escalation event into a SHA-256 hash-chained immutable audit log.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

/// Maximum allowed end-to-end SLA for P1 automated triage remediation (90 seconds).
pub const P1_TRIAGE_SLA: Duration = Duration::from_secs(90);
/// Default timeout for a single automated runbook step (15 seconds).
pub const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// Genesis hash for the first entry in the SHA-256 hash-chained immutable audit log.
pub const AUDIT_GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, thiserror::Error)]
pub enum TriageError {
    #[error("alert '{alert_name}' is missing a valid runbook_url pointing to docs/runbooks/")]
    InvalidRunbookUrl { alert_name: String },
    #[error("runbook step '{action}' timed out after {timeout_ms}ms")]
    StepTimeout { action: String, timeout_ms: u64 },
    #[error("runbook step '{action}' failed: {reason}")]
    StepFailed { action: String, reason: String },
    #[error("Slack notification failed: {0}")]
    SlackError(String),
}

// ---------------------------------------------------------------------------
// 1. Prometheus Alert & Runbook URL Validation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AlertPriority {
    P1,
    P2,
    P3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrometheusAlert {
    pub alertname: String,
    pub severity: String,
    pub priority: AlertPriority,
    pub labels: HashMap<String, String>,
    pub annotations: HashMap<String, String>,
}

impl PrometheusAlert {
    /// Extracts and validates the `runbook_url` annotation, ensuring it points
    /// to `docs/runbooks/<file>.md`.
    pub fn validate_runbook_url(&self) -> Result<String, TriageError> {
        let raw = self
            .annotations
            .get("runbook_url")
            .or_else(|| self.annotations.get("runbook"))
            .map(String::as_str)
            .unwrap_or("");

        if let Some(idx) = raw.find("docs/runbooks/") {
            let rel = &raw[idx..];
            if rel.ends_with(".md") && rel.len() > "docs/runbooks/.md".len() {
                return Ok(rel.to_string());
            }
        }

        Err(TriageError::InvalidRunbookUrl {
            alert_name: self.alertname.clone(),
        })
    }

    pub fn is_p1(&self) -> bool {
        self.priority == AlertPriority::P1 || self.severity.eq_ignore_ascii_case("critical")
    }
}

// ---------------------------------------------------------------------------
// 2. Grafana OnCall Escalation Chain (0m -> 5m -> 15m)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EscalationStep {
    pub stage_index: usize,
    pub action: String,
    pub target: String,
    pub delay_seconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EscalationChain {
    pub steps: Vec<EscalationStep>,
}

impl Default for EscalationChain {
    fn default() -> Self {
        Self {
            steps: vec![
                EscalationStep {
                    stage_index: 1,
                    action: "notify(channel)".to_string(),
                    target: "#tessera-incidents".to_string(),
                    delay_seconds: 0,
                },
                EscalationStep {
                    stage_index: 2,
                    action: "notify(on-call engineer)".to_string(),
                    target: "primary-sre-schedule".to_string(),
                    delay_seconds: 5 * 60, // 5 minutes (300s)
                },
                EscalationStep {
                    stage_index: 3,
                    action: "notify(manager)".to_string(),
                    target: "engineering-manager".to_string(),
                    delay_seconds: 15 * 60, // 15 minutes (900s)
                },
            ],
        }
    }
}

impl EscalationChain {
    /// Returns all escalation stages that should have fired at `elapsed` since
    /// alert onset when the incident is still unresolved.
    pub fn due_steps(&self, elapsed: Duration, resolved: bool) -> Vec<EscalationStep> {
        if resolved {
            // Even if resolved quickly, the initial 0m channel notification always fires.
            return self
                .steps
                .iter()
                .filter(|s| s.delay_seconds == 0)
                .cloned()
                .collect();
        }
        let elapsed_secs = elapsed.as_secs();
        self.steps
            .iter()
            .filter(|s| elapsed_secs >= s.delay_seconds)
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// 3. Automated Runbook Actions, Timeouts & Rollbacks
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunbookAction {
    CheckPostgresConnectionCount,
    RestartStuckIndexerPod,
    FlushRedisCache,
}

impl RunbookAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunbookAction::CheckPostgresConnectionCount => "check_postgres_connection_count",
            RunbookAction::RestartStuckIndexerPod => "restart_stuck_indexer_pod",
            RunbookAction::FlushRedisCache => "flush_redis_cache",
        }
    }

    pub fn rollback_name(&self) -> &'static str {
        match self {
            RunbookAction::CheckPostgresConnectionCount => "restore_postgres_pool_limits",
            RunbookAction::RestartStuckIndexerPod => "rollback_indexer_pod_deployment",
            RunbookAction::FlushRedisCache => "restore_redis_cache_snapshot",
        }
    }

    /// Maps a validated runbook path (`docs/runbooks/*.md`) and P1 alert to its
    /// ordered sequence of automated remediation steps.
    pub fn plan_for_runbook(runbook_rel_path: &str) -> Vec<RunbookAction> {
        if runbook_rel_path.ends_with("database-pool.md") {
            vec![
                RunbookAction::CheckPostgresConnectionCount,
                RunbookAction::FlushRedisCache,
            ]
        } else if runbook_rel_path.ends_with("indexer-lag.md")
            || runbook_rel_path.ends_with("soroban-rpc.md")
        {
            vec![
                RunbookAction::CheckPostgresConnectionCount,
                RunbookAction::RestartStuckIndexerPod,
                RunbookAction::FlushRedisCache,
            ]
        } else {
            vec![
                RunbookAction::CheckPostgresConnectionCount,
                RunbookAction::FlushRedisCache,
                RunbookAction::RestartStuckIndexerPod,
            ]
        }
    }
}

/// Pre-execution state snapshot captured before a runbook action so `rollback_step`
/// can deterministically restore the prior system state on timeout or error.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepRollbackSnapshot {
    pub action: RunbookAction,
    pub metadata: HashMap<String, String>,
}

#[async_trait]
pub trait RunbookStepExecutor: Send + Sync {
    /// Captures a pre-step snapshot and executes the automated runbook action.
    async fn execute_step(&self, action: RunbookAction) -> Result<StepRollbackSnapshot, String>;

    /// Rolls back a failed or timed-out runbook action using the pre-step snapshot.
    async fn rollback_step(
        &self,
        action: RunbookAction,
        snapshot: Option<&StepRollbackSnapshot>,
    ) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// 4. Structured Slack Output (`{action, outcome, duration_ms, next_step}`)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TriageSlackPayload {
    pub action: String,
    pub outcome: String,
    pub duration_ms: u64,
    pub next_step: String,
}

#[async_trait]
pub trait SlackNotifier: Send + Sync {
    async fn post_triage_update(&self, payload: &TriageSlackPayload) -> Result<(), String>;
}

/// Production HTTP webhook Slack notifier using `reqwest`.
pub struct WebhookSlackNotifier {
    http: Client,
    webhook_url: String,
}

impl WebhookSlackNotifier {
    pub fn new(webhook_url: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            webhook_url: webhook_url.into(),
        }
    }
}

#[async_trait]
impl SlackNotifier for WebhookSlackNotifier {
    async fn post_triage_update(&self, payload: &TriageSlackPayload) -> Result<(), String> {
        let resp = self
            .http
            .post(&self.webhook_url)
            .json(payload)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("Slack webhook returned HTTP {}", resp.status()));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 5. SHA-256 Hash-Chained Immutable Audit Trail
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TriageAuditEntry {
    pub sequence: u64,
    pub timestamp: String,
    pub alert_name: String,
    pub event_type: String,
    pub action: String,
    pub outcome: String,
    pub duration_ms: u64,
    pub next_step: String,
    pub prev_hash: String,
    pub entry_hash: String,
}

impl TriageAuditEntry {
    fn compute_hash(
        sequence: u64,
        timestamp: &str,
        alert_name: &str,
        event_type: &str,
        action: &str,
        outcome: &str,
        duration_ms: u64,
        next_step: &str,
        prev_hash: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(format!(
            "{sequence}|{timestamp}|{alert_name}|{event_type}|{action}|{outcome}|{duration_ms}|{next_step}|{prev_hash}"
        ));
        hex::encode(hasher.finalize())
    }
}

#[derive(Default)]
pub struct ImmutableAuditTrail {
    entries: Mutex<Vec<TriageAuditEntry>>,
}

impl ImmutableAuditTrail {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    pub async fn append(
        &self,
        alert_name: &str,
        event_type: &str,
        action: &str,
        outcome: &str,
        duration_ms: u64,
        next_step: &str,
    ) -> TriageAuditEntry {
        let mut guard = self.entries.lock().await;
        let sequence = (guard.len() as u64) + 1;
        let prev_hash = guard
            .last()
            .map(|e| e.entry_hash.clone())
            .unwrap_or_else(|| AUDIT_GENESIS_HASH.to_string());
        let timestamp = Utc::now().to_rfc3339();
        let entry_hash = TriageAuditEntry::compute_hash(
            sequence,
            &timestamp,
            alert_name,
            event_type,
            action,
            outcome,
            duration_ms,
            next_step,
            &prev_hash,
        );
        let entry = TriageAuditEntry {
            sequence,
            timestamp,
            alert_name: alert_name.to_string(),
            event_type: event_type.to_string(),
            action: action.to_string(),
            outcome: outcome.to_string(),
            duration_ms,
            next_step: next_step.to_string(),
            prev_hash,
            entry_hash,
        };
        guard.push(entry.clone());
        entry
    }

    pub async fn entries(&self) -> Vec<TriageAuditEntry> {
        self.entries.lock().await.clone()
    }

    /// Cryptographically verifies that the entire audit trail is intact and
    /// unmodified from `AUDIT_GENESIS_HASH` through the latest entry.
    pub async fn verify_integrity(&self) -> bool {
        let guard = self.entries.lock().await;
        let mut expected_prev = AUDIT_GENESIS_HASH.to_string();
        for (idx, entry) in guard.iter().enumerate() {
            if entry.sequence != (idx as u64) + 1 {
                return false;
            }
            if entry.prev_hash != expected_prev {
                return false;
            }
            let recomputed = TriageAuditEntry::compute_hash(
                entry.sequence,
                &entry.timestamp,
                &entry.alert_name,
                &entry.event_type,
                &entry.action,
                &entry.outcome,
                entry.duration_ms,
                &entry.next_step,
                &entry.prev_hash,
            );
            if entry.entry_hash != recomputed {
                return false;
            }
            expected_prev = entry.entry_hash.clone();
        }
        true
    }
}

// ---------------------------------------------------------------------------
// 6. Automated P1 Triage Bot
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TriageExecutionReport {
    pub alert_name: String,
    pub runbook_url: String,
    pub remediated: bool,
    pub rolled_back: bool,
    pub total_duration_ms: u64,
    pub completed_within_90s_sla: bool,
    pub slack_messages: Vec<TriageSlackPayload>,
    pub escalations_triggered: Vec<EscalationStep>,
}

pub struct AutomatedTriageBot {
    executor: Arc<dyn RunbookStepExecutor>,
    slack: Arc<dyn SlackNotifier>,
    audit: Arc<ImmutableAuditTrail>,
    escalation_chain: EscalationChain,
    step_timeout: Duration,
}

impl AutomatedTriageBot {
    pub fn new(
        executor: Arc<dyn RunbookStepExecutor>,
        slack: Arc<dyn SlackNotifier>,
        audit: Arc<ImmutableAuditTrail>,
    ) -> Self {
        Self {
            executor,
            slack,
            audit,
            escalation_chain: EscalationChain::default(),
            step_timeout: DEFAULT_STEP_TIMEOUT,
        }
    }

    pub fn with_step_timeout(mut self, step_timeout: Duration) -> Self {
        self.step_timeout = step_timeout;
        self
    }

    pub fn audit_trail(&self) -> &Arc<ImmutableAuditTrail> {
        &self.audit
    }

    /// Executes the automated triage runbook for a P1 Prometheus alert:
    /// - Validates `runbook_url` (`docs/runbooks/*.md`).
    /// - Fires the 0m `notify(channel)` escalation stage.
    /// - Runs each planned `RunbookAction` with `step_timeout` and automatic rollback.
    /// - Posts structured `{action, outcome, duration_ms, next_step}` messages to Slack.
    /// - Records every action in the SHA-256 hash-chained `ImmutableAuditTrail`.
    pub async fn handle_alert(
        &self,
        alert: &PrometheusAlert,
    ) -> Result<TriageExecutionReport, TriageError> {
        let triage_started = Instant::now();
        let runbook_url = alert.validate_runbook_url()?;

        // Stage 1 (0m): notify(channel)
        let mut escalations_triggered = self.escalation_chain.due_steps(Duration::ZERO, false);
        for esc in &escalations_triggered {
            self.audit
                .append(
                    &alert.alertname,
                    "escalation",
                    &esc.action,
                    "notified",
                    0,
                    "start_automated_triage",
                )
                .await;
        }

        let actions = if alert.is_p1() {
            RunbookAction::plan_for_runbook(&runbook_url)
        } else {
            vec![RunbookAction::CheckPostgresConnectionCount]
        };

        let mut slack_messages = Vec::with_capacity(actions.len());
        let mut remediated = true;
        let mut rolled_back = false;

        for (idx, action) in actions.iter().copied().enumerate() {
            let step_started = Instant::now();
            let next_default = actions
                .get(idx + 1)
                .map(|a| a.as_str().to_string())
                .unwrap_or_else(|| "verify_slo_recovery".to_string());

            let step_result =
                tokio::time::timeout(self.step_timeout, self.executor.execute_step(action)).await;

            let duration_ms = (step_started.elapsed().as_millis() as u64).max(1);

            match step_result {
                Ok(Ok(_snapshot)) => {
                    let msg = TriageSlackPayload {
                        action: action.as_str().to_string(),
                        outcome: "succeeded".to_string(),
                        duration_ms,
                        next_step: next_default.clone(),
                    };
                    let _ = self.slack.post_triage_update(&msg).await;
                    self.audit
                        .append(
                            &alert.alertname,
                            "runbook_step",
                            &msg.action,
                            &msg.outcome,
                            msg.duration_ms,
                            &msg.next_step,
                        )
                        .await;
                    slack_messages.push(msg);
                }
                Ok(Err(err_reason)) => {
                    remediated = false;
                    rolled_back = true;
                    let _ = self.executor.rollback_step(action, None).await;
                    let msg = TriageSlackPayload {
                        action: action.as_str().to_string(),
                        outcome: format!("failed_and_rolled_back ({})", action.rollback_name()),
                        duration_ms,
                        next_step: "notify(on-call engineer)".to_string(),
                    };
                    let _ = self.slack.post_triage_update(&msg).await;
                    self.audit
                        .append(
                            &alert.alertname,
                            "runbook_rollback",
                            &msg.action,
                            &format!("{}: {}", msg.outcome, err_reason),
                            msg.duration_ms,
                            &msg.next_step,
                        )
                        .await;
                    slack_messages.push(msg);
                    break;
                }
                Err(_elapsed) => {
                    remediated = false;
                    rolled_back = true;
                    let _ = self.executor.rollback_step(action, None).await;
                    let msg = TriageSlackPayload {
                        action: action.as_str().to_string(),
                        outcome: format!("timed_out_and_rolled_back ({})", action.rollback_name()),
                        duration_ms,
                        next_step: "notify(on-call engineer)".to_string(),
                    };
                    let _ = self.slack.post_triage_update(&msg).await;
                    self.audit
                        .append(
                            &alert.alertname,
                            "runbook_timeout_rollback",
                            &msg.action,
                            &msg.outcome,
                            msg.duration_ms,
                            &msg.next_step,
                        )
                        .await;
                    slack_messages.push(msg);
                    break;
                }
            }
        }

        if !remediated {
            // If automated triage had to roll back, prepare the 5m on-call engineer escalation
            let five_min_steps = self
                .escalation_chain
                .due_steps(Duration::from_secs(300), false);
            for step in five_min_steps {
                if !escalations_triggered
                    .iter()
                    .any(|s| s.stage_index == step.stage_index)
                {
                    self.audit
                        .append(
                            &alert.alertname,
                            "escalation",
                            &step.action,
                            "scheduled_after_rollback",
                            0,
                            "notify(manager)",
                        )
                        .await;
                    escalations_triggered.push(step);
                }
            }
        }

        let total_duration = triage_started.elapsed();
        let total_duration_ms = (total_duration.as_millis() as u64).max(1);
        let completed_within_90s_sla = total_duration <= P1_TRIAGE_SLA;

        Ok(TriageExecutionReport {
            alert_name: alert.alertname.clone(),
            runbook_url,
            remediated,
            rolled_back,
            total_duration_ms,
            completed_within_90s_sla,
            slack_messages,
            escalations_triggered,
        })
    }
}

// ---------------------------------------------------------------------------
// Unit & Chaos Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct RecordingSlack {
        messages: Mutex<Vec<TriageSlackPayload>>,
    }

    #[async_trait]
    impl SlackNotifier for RecordingSlack {
        async fn post_triage_update(&self, payload: &TriageSlackPayload) -> Result<(), String> {
            self.messages.lock().await.push(payload.clone());
            Ok(())
        }
    }

    struct SimulatedClusterExecutor {
        pg_active_connections: AtomicUsize,
        indexer_pod_stuck: AtomicBool,
        redis_cache_corrupted: AtomicBool,
        hang_on_restart: AtomicBool,
        rollbacks_executed: AtomicUsize,
    }

    impl SimulatedClusterExecutor {
        fn new_degraded() -> Arc<Self> {
            Arc::new(Self {
                pg_active_connections: AtomicUsize::new(98),
                indexer_pod_stuck: AtomicBool::new(true),
                redis_cache_corrupted: AtomicBool::new(true),
                hang_on_restart: AtomicBool::new(false),
                rollbacks_executed: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl RunbookStepExecutor for SimulatedClusterExecutor {
        async fn execute_step(
            &self,
            action: RunbookAction,
        ) -> Result<StepRollbackSnapshot, String> {
            match action {
                RunbookAction::CheckPostgresConnectionCount => {
                    let prev = self.pg_active_connections.swap(24, Ordering::SeqCst);
                    let mut metadata = HashMap::new();
                    metadata.insert("previous_connections".to_string(), prev.to_string());
                    Ok(StepRollbackSnapshot { action, metadata })
                }
                RunbookAction::RestartStuckIndexerPod => {
                    if self.hang_on_restart.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                    self.indexer_pod_stuck.store(false, Ordering::SeqCst);
                    Ok(StepRollbackSnapshot {
                        action,
                        metadata: HashMap::new(),
                    })
                }
                RunbookAction::FlushRedisCache => {
                    self.redis_cache_corrupted.store(false, Ordering::SeqCst);
                    Ok(StepRollbackSnapshot {
                        action,
                        metadata: HashMap::new(),
                    })
                }
            }
        }

        async fn rollback_step(
            &self,
            _action: RunbookAction,
            _snapshot: Option<&StepRollbackSnapshot>,
        ) -> Result<(), String> {
            self.rollbacks_executed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn sample_p1_alert() -> PrometheusAlert {
        let mut annotations = HashMap::new();
        annotations.insert(
            "summary".to_string(),
            "Indexer is more than ten ledgers behind".to_string(),
        );
        annotations.insert(
            "runbook_url".to_string(),
            "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/indexer-lag.md"
                .to_string(),
        );
        PrometheusAlert {
            alertname: "HighIndexerLag".to_string(),
            severity: "critical".to_string(),
            priority: AlertPriority::P1,
            labels: HashMap::new(),
            annotations,
        }
    }

    #[test]
    fn test_all_alerts_in_alerts_yaml_have_runbook_url_pointing_to_docs_runbooks() {
        let alerts_yaml =
            std::fs::read_to_string("../monitoring/alerts.yaml").expect("read alerts.yaml");
        let runbook_urls: Vec<&str> = alerts_yaml
            .lines()
            .filter_map(|line| line.trim().strip_prefix("runbook_url:"))
            .map(str::trim)
            .collect();

        assert!(
            runbook_urls.len() >= 4,
            "Expected at least 4 alerts with runbook_url in alerts.yaml, found {}",
            runbook_urls.len()
        );

        for url in runbook_urls {
            let idx = url
                .find("docs/runbooks/")
                .expect("runbook_url must reference docs/runbooks/");
            let rel_path = &url[idx..];
            let full_path = Path::new("../..").join(rel_path);
            assert!(
                full_path.exists(),
                "Referenced runbook file does not exist: {}",
                full_path.display()
            );
        }
    }

    #[test]
    fn test_escalation_chain_notify_channel_5m_oncall_15m_manager() {
        let chain = EscalationChain::default();
        assert_eq!(chain.steps.len(), 3);
        assert_eq!(chain.steps[0].action, "notify(channel)");
        assert_eq!(chain.steps[0].delay_seconds, 0);
        assert_eq!(chain.steps[1].action, "notify(on-call engineer)");
        assert_eq!(chain.steps[1].delay_seconds, 300);
        assert_eq!(chain.steps[2].action, "notify(manager)");
        assert_eq!(chain.steps[2].delay_seconds, 900);

        assert_eq!(chain.due_steps(Duration::from_secs(0), false).len(), 1);
        assert_eq!(chain.due_steps(Duration::from_secs(299), false).len(), 1);
        assert_eq!(chain.due_steps(Duration::from_secs(300), false).len(), 2);
        assert_eq!(chain.due_steps(Duration::from_secs(899), false).len(), 2);
        assert_eq!(chain.due_steps(Duration::from_secs(900), false).len(), 3);
    }

    #[tokio::test]
    async fn test_runbook_step_timeout_triggers_automatic_rollback_and_audit_log() {
        let cluster = SimulatedClusterExecutor::new_degraded();
        cluster.hang_on_restart.store(true, Ordering::SeqCst);
        let slack = Arc::new(RecordingSlack::default());
        let audit = Arc::new(ImmutableAuditTrail::new());

        let bot = AutomatedTriageBot::new(
            cluster.clone(),
            slack.clone(),
            Arc::clone(&audit),
        )
        .with_step_timeout(Duration::from_millis(50));

        let report = bot.handle_alert(&sample_p1_alert()).await.unwrap();
        assert!(!report.remediated);
        assert!(report.rolled_back);
        assert_eq!(cluster.rollbacks_executed.load(Ordering::SeqCst), 1);

        let last_slack = report.slack_messages.last().unwrap();
        assert_eq!(last_slack.action, "restart_stuck_indexer_pod");
        assert!(last_slack.outcome.contains("timed_out_and_rolled_back"));
        assert_eq!(last_slack.next_step, "notify(on-call engineer)");
        assert!(audit.verify_integrity().await);
    }

    #[tokio::test]
    async fn test_chaos_p1_alert_automated_triage_completes_within_90s() {
        let cluster = SimulatedClusterExecutor::new_degraded();
        let slack = Arc::new(RecordingSlack::default());
        let audit = Arc::new(ImmutableAuditTrail::new());

        let bot = AutomatedTriageBot::new(cluster.clone(), slack.clone(), Arc::clone(&audit));
        let alert = sample_p1_alert();

        let report = bot.handle_alert(&alert).await.unwrap();
        assert!(report.remediated);
        assert!(!report.rolled_back);
        assert!(report.completed_within_90s_sla);
        assert!(report.total_duration_ms < 90_000);

        // Verify all 3 corrective actions executed and restored cluster state
        assert_eq!(cluster.pg_active_connections.load(Ordering::SeqCst), 24);
        assert!(!cluster.indexer_pod_stuck.load(Ordering::SeqCst));
        assert!(!cluster.redis_cache_corrupted.load(Ordering::SeqCst));

        // Verify structured Slack payloads `{action, outcome, duration_ms, next_step}`
        assert_eq!(report.slack_messages.len(), 3);
        assert_eq!(
            report.slack_messages[0].action,
            "check_postgres_connection_count"
        );
        assert_eq!(report.slack_messages[0].outcome, "succeeded");
        assert_eq!(
            report.slack_messages[0].next_step,
            "restart_stuck_indexer_pod"
        );

        assert_eq!(report.slack_messages[1].action, "restart_stuck_indexer_pod");
        assert_eq!(report.slack_messages[1].outcome, "succeeded");
        assert_eq!(report.slack_messages[1].next_step, "flush_redis_cache");

        assert_eq!(report.slack_messages[2].action, "flush_redis_cache");
        assert_eq!(report.slack_messages[2].outcome, "succeeded");
        assert_eq!(report.slack_messages[2].next_step, "verify_slo_recovery");

        for msg in &report.slack_messages {
            let as_json = serde_json::to_value(msg).unwrap();
            assert!(as_json.get("action").is_some());
            assert!(as_json.get("outcome").is_some());
            assert!(as_json.get("duration_ms").is_some());
            assert!(as_json.get("next_step").is_some());
        }

        // Verify SHA-256 hash-chained immutable audit trail
        let entries = audit.entries().await;
        assert_eq!(entries.len(), 4); // 1 initial escalation + 3 runbook steps
        assert!(audit.verify_integrity().await);
    }
}
