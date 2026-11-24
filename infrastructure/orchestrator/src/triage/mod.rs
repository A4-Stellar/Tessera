//! Automated P1 alert triage (issue #474).
//!
//! Grafana OnCall (or Alertmanager directly) forwards firing alerts to the
//! triage bot. For a **P1** alert the bot looks up the automated part of the
//! alert's runbook (selected by the `runbook_url` annotation, which points at
//! `docs/runbooks/<slug>.md`) and runs its steps in order:
//!
//! * check the PostgreSQL connection count,
//! * restart a stuck indexer pod,
//! * flush the Redis response cache.
//!
//! Every step runs under its own timeout and can be rolled back:
//!
//! 1. `prepare` captures whatever is needed to undo the step (the previous
//!    rollout annotation, a snapshot of the cache keys) *before* anything
//!    changes.
//! 2. `execute` performs the action under `tokio::time::timeout`.
//! 3. If `execute` fails or runs out of time, `rollback` restores the
//!    captured state, the remaining steps are skipped and the alert is left
//!    to the human escalation chain.
//!
//! Each step produces a structured [`StepReport`] (`{action, outcome,
//! duration_ms, next_step}`) that is posted to Slack, and every automated
//! action is written to the immutable audit trail: an *intent* record before
//! the step runs and a *result* record afterwards. The bot is fail-closed on
//! auditing: if the intent cannot be recorded, the step does not run.
//!
//! Alerts below P1 are acknowledged and left to the escalation chain. The same
//! alert (by fingerprint) is not re-triaged inside [`TriagePolicy::cooldown`],
//! so Alertmanager's repeat notifications do not restart the indexer in a
//! loop.

pub mod actions;
pub mod backends;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::time::{timeout, Instant};
use tracing::{info, warn};

/// Opaque data a step captures in `prepare` and receives back in `rollback`.
pub type UndoToken = serde_json::Value;

// ── Alerts ──────────────────────────────────────────────────────────────────

/// Incident priority. Only [`Priority::P1`] triggers automated remediation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Priority {
    P1,
    P2,
    P3,
    P4,
}

impl Priority {
    /// Reads the `priority` label (`P1`..`P4`) and falls back to `severity`:
    /// `critical`/`page` are P1, `warning` is P2, `ticket` is P3, anything
    /// else P4.
    pub fn from_labels(labels: &BTreeMap<String, String>) -> Priority {
        if let Some(p) = labels.get("priority") {
            match p.trim().to_ascii_uppercase().as_str() {
                "P1" => return Priority::P1,
                "P2" => return Priority::P2,
                "P3" => return Priority::P3,
                "P4" => return Priority::P4,
                _ => {}
            }
        }
        match labels
            .get("severity")
            .map(|s| s.trim().to_ascii_lowercase())
        {
            Some(s) if s == "critical" || s == "page" => Priority::P1,
            Some(s) if s == "warning" => Priority::P2,
            Some(s) if s == "ticket" => Priority::P3,
            _ => Priority::P4,
        }
    }
}

/// One alert as received from Alertmanager or Grafana OnCall.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    #[serde(default)]
    pub fingerprint: String,
}

impl Alert {
    pub fn name(&self) -> &str {
        self.labels
            .get("alertname")
            .map(String::as_str)
            .unwrap_or("unknown")
    }

    pub fn priority(&self) -> Priority {
        Priority::from_labels(&self.labels)
    }

    pub fn is_firing(&self) -> bool {
        self.status.is_empty() || self.status.eq_ignore_ascii_case("firing")
    }

    pub fn runbook_url(&self) -> Option<&str> {
        self.annotations
            .get("runbook_url")
            .or_else(|| self.annotations.get("runbook"))
            .map(String::as_str)
    }

    /// The runbook slug, i.e. the file stem of a `docs/runbooks/<slug>.md`
    /// link (an `#anchor` is ignored). `None` for links elsewhere.
    pub fn runbook_slug(&self) -> Option<String> {
        let url = self.runbook_url()?;
        let path = url.split(['#', '?']).next()?;
        let (_, rest) = path.rsplit_once("docs/runbooks/")?;
        let slug = rest.strip_suffix(".md").unwrap_or(rest);
        (!slug.is_empty() && !slug.contains('/')).then(|| slug.to_string())
    }

    /// A stable identity for de-duplication: the Alertmanager fingerprint, or
    /// the sorted label set when none was sent.
    pub fn identity(&self) -> String {
        if !self.fingerprint.is_empty() {
            return self.fingerprint.clone();
        }
        self.labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Alertmanager webhook payload (also what Grafana OnCall forwards for an
/// Alertmanager integration). Only the fields the bot needs.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AlertmanagerPayload {
    #[serde(default)]
    pub alerts: Vec<Alert>,
}

/// Grafana OnCall outgoing-webhook payload: the alert group's first alert
/// carries the original Alertmanager alert.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct OnCallPayload {
    #[serde(default)]
    pub alert_payload: Option<Alert>,
    #[serde(default)]
    pub alert_group: Option<serde_json::Value>,
}

// ── Steps ───────────────────────────────────────────────────────────────────

/// What a successful step did.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// A check passed; nothing needed changing.
    Healthy,
    /// A check found a problem it does not fix itself.
    Degraded,
    /// The step changed something to fix the problem.
    Remediated,
    /// The step decided not to act (for example nothing was stuck).
    Skipped,
    /// The step returned an error.
    Failed,
    /// The step ran out of time.
    TimedOut,
}

impl Outcome {
    pub fn is_failure(self) -> bool {
        matches!(self, Outcome::Failed | Outcome::TimedOut)
    }
}

/// Successful result of [`RunbookStep::execute`].
#[derive(Clone, Debug, PartialEq)]
pub struct StepSuccess {
    pub outcome: Outcome,
    pub detail: String,
}

impl StepSuccess {
    pub fn new(outcome: Outcome, detail: impl Into<String>) -> Self {
        Self {
            outcome,
            detail: detail.into(),
        }
    }
}

/// One automated runbook action.
#[async_trait]
pub trait RunbookStep: Send + Sync {
    /// Stable machine name, e.g. `restart_indexer_pod`.
    fn action(&self) -> &'static str;

    /// Upper bound for `execute`.
    fn timeout(&self) -> Duration;

    /// Capture what is needed to undo the step, before anything changes.
    /// Read-only steps return `Ok(None)`.
    async fn prepare(&self, _alert: &Alert) -> Result<Option<UndoToken>, String> {
        Ok(None)
    }

    async fn execute(&self, alert: &Alert) -> Result<StepSuccess, String>;

    /// Undo a failed or timed-out `execute`, given the `prepare` token.
    async fn rollback(&self, _undo: UndoToken) -> Result<(), String> {
        Ok(())
    }
}

/// The structured per-step result posted to Slack (issue #474 item 4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepReport {
    pub action: String,
    pub outcome: Outcome,
    pub duration_ms: u64,
    pub next_step: String,
    /// Human-readable detail. Not part of the four required keys but useful
    /// in the Slack thread.
    pub detail: String,
    /// Set when the step failed and its rollback was attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback: Option<RollbackOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum RollbackOutcome {
    /// Nothing to roll back (read-only step, or nothing was captured).
    NotNeeded,
    RolledBack,
    Failed(String),
}

/// Everything the bot did for one alert.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriageReport {
    pub alert: String,
    pub fingerprint: String,
    pub priority: Priority,
    pub runbook: Option<String>,
    pub runbook_url: Option<String>,
    pub steps: Vec<StepReport>,
    /// `true` when a step failed and the alert needs a human.
    pub escalated: bool,
    pub total_duration_ms: u64,
}

// ── Side channels ───────────────────────────────────────────────────────────

/// Posts triage results (Slack in production).
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn post(&self, report: &TriageReport) -> Result<(), String>;
}

/// Phase of an audit record.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditPhase {
    Intent,
    Result,
    Rollback,
}

/// One automated action for the immutable audit trail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub phase: AuditPhase,
    pub action: String,
    pub alert: String,
    pub fingerprint: String,
    pub runbook: Option<String>,
    pub details: serde_json::Value,
}

/// Appends to the audit trail (the API's hash-chained `/v1/audit/entries` in
/// production).
#[async_trait]
pub trait AuditSink: Send + Sync {
    async fn record(&self, record: AuditRecord) -> Result<(), String>;
}

// ── Runbooks ────────────────────────────────────────────────────────────────

/// The automated steps for one runbook.
#[derive(Clone)]
pub struct Runbook {
    pub slug: String,
    pub steps: Vec<Arc<dyn RunbookStep>>,
}

/// Maps runbook slugs to automated steps.
#[derive(Clone, Default)]
pub struct RunbookCatalog {
    runbooks: HashMap<String, Runbook>,
    fallback: Option<Runbook>,
}

impl RunbookCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_runbook(mut self, slug: &str, steps: Vec<Arc<dyn RunbookStep>>) -> Self {
        self.runbooks.insert(
            slug.to_string(),
            Runbook {
                slug: slug.to_string(),
                steps,
            },
        );
        self
    }

    /// Steps used for a P1 alert whose runbook has no automation.
    pub fn with_fallback(mut self, steps: Vec<Arc<dyn RunbookStep>>) -> Self {
        self.fallback = Some(Runbook {
            slug: "default".into(),
            steps,
        });
        self
    }

    pub fn for_alert(&self, alert: &Alert) -> Option<&Runbook> {
        alert
            .runbook_slug()
            .and_then(|slug| self.runbooks.get(&slug))
            .or(self.fallback.as_ref())
    }

    /// The standard Tessera catalogue:
    ///
    /// | runbook | steps |
    /// |---|---|
    /// | `indexer-lag` | Postgres connections → restart indexer → flush cache |
    /// | `database-pool` | Postgres connections |
    /// | `api-errors`, `api-error-budget` | Postgres connections → flush cache |
    /// | `soroban-rpc` | Postgres connections → restart indexer |
    /// | anything else (P1) | Postgres connections |
    pub fn standard(
        postgres: Arc<dyn RunbookStep>,
        indexer: Arc<dyn RunbookStep>,
        cache: Arc<dyn RunbookStep>,
    ) -> Self {
        Self::new()
            .with_runbook(
                "indexer-lag",
                vec![postgres.clone(), indexer.clone(), cache.clone()],
            )
            .with_runbook("database-pool", vec![postgres.clone()])
            .with_runbook("api-errors", vec![postgres.clone(), cache.clone()])
            .with_runbook("api-error-budget", vec![postgres.clone(), cache])
            .with_runbook("soroban-rpc", vec![postgres.clone(), indexer])
            .with_fallback(vec![postgres])
    }
}

// ── Bot ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct TriagePolicy {
    /// The same alert is not triaged again for this long.
    pub cooldown: Duration,
    /// Time budget for a whole runbook. Steps that would start after it is
    /// spent are skipped.
    pub runbook_budget: Duration,
}

impl Default for TriagePolicy {
    fn default() -> Self {
        Self {
            cooldown: Duration::from_secs(10 * 60),
            // Issue #474: corrective action within 90 s of the P1 alert.
            runbook_budget: Duration::from_secs(90),
        }
    }
}

/// Why an alert was not triaged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason", content = "detail")]
pub enum NotTriaged {
    Resolved,
    BelowP1(Priority),
    NoRunbook,
    CoolingDown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageResult {
    Triaged(TriageReport),
    Ignored(NotTriaged),
}

pub struct TriageBot {
    catalog: RunbookCatalog,
    notifier: Arc<dyn Notifier>,
    audit: Arc<dyn AuditSink>,
    policy: TriagePolicy,
    recent: Mutex<HashMap<String, Instant>>,
}

impl TriageBot {
    pub fn new(
        catalog: RunbookCatalog,
        notifier: Arc<dyn Notifier>,
        audit: Arc<dyn AuditSink>,
        policy: TriagePolicy,
    ) -> Self {
        Self {
            catalog,
            notifier,
            audit,
            policy,
            recent: Mutex::new(HashMap::new()),
        }
    }

    /// Triage every alert of an Alertmanager webhook payload.
    pub async fn handle_alertmanager(&self, payload: AlertmanagerPayload) -> Vec<TriageResult> {
        let mut out = Vec::with_capacity(payload.alerts.len());
        for alert in payload.alerts {
            out.push(self.handle(alert).await);
        }
        out
    }

    /// Triage the alert carried by a Grafana OnCall outgoing webhook.
    pub async fn handle_oncall(&self, payload: OnCallPayload) -> Option<TriageResult> {
        match payload.alert_payload {
            Some(alert) => Some(self.handle(alert).await),
            None => None,
        }
    }

    pub async fn handle(&self, alert: Alert) -> TriageResult {
        if !alert.is_firing() {
            return TriageResult::Ignored(NotTriaged::Resolved);
        }
        let priority = alert.priority();
        if priority != Priority::P1 {
            return TriageResult::Ignored(NotTriaged::BelowP1(priority));
        }
        let Some(runbook) = self.catalog.for_alert(&alert).cloned() else {
            return TriageResult::Ignored(NotTriaged::NoRunbook);
        };
        if !self.claim(&alert.identity()) {
            return TriageResult::Ignored(NotTriaged::CoolingDown);
        }

        let report = self.run(&alert, &runbook).await;
        if let Err(error) = self.notifier.post(&report).await {
            warn!(alert = alert.name(), %error, "failed to post triage report");
        }
        TriageResult::Triaged(report)
    }

    /// Records the alert as being triaged now, unless it already was within
    /// the cooldown.
    fn claim(&self, identity: &str) -> bool {
        let now = Instant::now();
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.retain(|_, at| now.duration_since(*at) < self.policy.cooldown);
        if recent.contains_key(identity) {
            return false;
        }
        recent.insert(identity.to_string(), now);
        true
    }

    async fn run(&self, alert: &Alert, runbook: &Runbook) -> TriageReport {
        let started = Instant::now();
        let deadline = started + self.policy.runbook_budget;
        let escalate = format!(
            "escalated to on-call; follow {}",
            alert.runbook_url().unwrap_or("the alert's runbook")
        );
        info!(alert = alert.name(), runbook = %runbook.slug, "running automated triage");

        let mut steps: Vec<StepReport> = Vec::with_capacity(runbook.steps.len());
        let mut escalated = false;

        for (index, step) in runbook.steps.iter().enumerate() {
            let following = runbook
                .steps
                .get(index + 1)
                .map(|s| s.action().to_string())
                .unwrap_or_else(|| "monitor alert until it resolves".into());

            if escalated {
                steps.push(skipped(
                    step.action(),
                    "skipped after an earlier step failed",
                    &escalate,
                ));
                continue;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                escalated = true;
                steps.push(skipped(
                    step.action(),
                    "runbook time budget spent",
                    &escalate,
                ));
                continue;
            }

            let report = self
                .run_step(
                    alert,
                    &runbook.slug,
                    step.as_ref(),
                    remaining,
                    following,
                    &escalate,
                )
                .await;
            escalated = report.outcome.is_failure();
            steps.push(report);
        }

        TriageReport {
            alert: alert.name().to_string(),
            fingerprint: alert.identity(),
            priority: alert.priority(),
            runbook: Some(runbook.slug.clone()),
            runbook_url: alert.runbook_url().map(str::to_string),
            steps,
            escalated,
            total_duration_ms: millis(started.elapsed()),
        }
    }

    async fn run_step(
        &self,
        alert: &Alert,
        runbook: &str,
        step: &dyn RunbookStep,
        remaining: Duration,
        following: String,
        escalate: &str,
    ) -> StepReport {
        let action = step.action();
        let started = Instant::now();
        let limit = step.timeout().min(remaining);
        let record = |phase, details| AuditRecord {
            phase,
            action: action.to_string(),
            alert: alert.name().to_string(),
            fingerprint: alert.identity(),
            runbook: Some(runbook.to_string()),
            details,
        };

        // Fail closed: an action that cannot be audited is not taken.
        if let Err(error) = self
            .audit
            .record(record(
                AuditPhase::Intent,
                serde_json::json!({ "timeout_ms": millis(limit) }),
            ))
            .await
        {
            return StepReport {
                action: action.into(),
                outcome: Outcome::Failed,
                duration_ms: millis(started.elapsed()),
                next_step: escalate.into(),
                detail: format!("not run: audit trail unavailable ({error})"),
                rollback: Some(RollbackOutcome::NotNeeded),
            };
        }

        // `prepare` shares the step's time limit.
        let prepared = match timeout(limit, step.prepare(alert)).await {
            Ok(Ok(token)) => token,
            Ok(Err(error)) => {
                return self
                    .finish(
                        record,
                        action,
                        started,
                        Outcome::Failed,
                        format!("prepare failed: {error}"),
                        escalate,
                        Some(RollbackOutcome::NotNeeded),
                    )
                    .await;
            }
            Err(_) => {
                return self
                    .finish(
                        record,
                        action,
                        started,
                        Outcome::TimedOut,
                        "timed out while preparing".into(),
                        escalate,
                        Some(RollbackOutcome::NotNeeded),
                    )
                    .await;
            }
        };

        let left = limit.saturating_sub(started.elapsed());
        let (outcome, detail) = match timeout(left, step.execute(alert)).await {
            Ok(Ok(success)) => (success.outcome, success.detail),
            Ok(Err(error)) => (Outcome::Failed, error),
            Err(_) => (
                Outcome::TimedOut,
                format!("no result within {} ms", millis(limit)),
            ),
        };

        if !outcome.is_failure() {
            return self
                .finish(record, action, started, outcome, detail, &following, None)
                .await;
        }

        let rollback = match prepared {
            None => RollbackOutcome::NotNeeded,
            Some(token) => match step.rollback(token).await {
                Ok(()) => RollbackOutcome::RolledBack,
                Err(error) => RollbackOutcome::Failed(error),
            },
        };
        if rollback != RollbackOutcome::NotNeeded {
            let _ = self
                .audit
                .record(record(
                    AuditPhase::Rollback,
                    serde_json::to_value(&rollback).unwrap_or_default(),
                ))
                .await;
        }
        self.finish(
            record,
            action,
            started,
            outcome,
            detail,
            escalate,
            Some(rollback),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        record: impl Fn(AuditPhase, serde_json::Value) -> AuditRecord,
        action: &str,
        started: Instant,
        outcome: Outcome,
        detail: String,
        next_step: &str,
        rollback: Option<RollbackOutcome>,
    ) -> StepReport {
        let report = StepReport {
            action: action.into(),
            outcome,
            duration_ms: millis(started.elapsed()),
            next_step: next_step.into(),
            detail,
            rollback,
        };
        if let Err(error) = self
            .audit
            .record(record(
                AuditPhase::Result,
                serde_json::to_value(&report).unwrap_or_default(),
            ))
            .await
        {
            warn!(action, %error, "failed to audit step result");
        }
        report
    }
}

fn skipped(action: &str, detail: &str, next_step: &str) -> StepReport {
    StepReport {
        action: action.into(),
        outcome: Outcome::Skipped,
        duration_ms: 0,
        next_step: next_step.into(),
        detail: detail.into(),
        rollback: None,
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(labels: &[(&str, &str)], runbook: &str) -> Alert {
        Alert {
            status: "firing".into(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            annotations: [("runbook_url".to_string(), runbook.to_string())].into(),
            fingerprint: String::new(),
        }
    }

    #[test]
    fn priority_prefers_label_then_severity() {
        let l = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        assert_eq!(
            Priority::from_labels(&l(&[("priority", "p1")])),
            Priority::P1
        );
        assert_eq!(
            Priority::from_labels(&l(&[("priority", "P3"), ("severity", "critical")])),
            Priority::P3
        );
        assert_eq!(
            Priority::from_labels(&l(&[("severity", "page")])),
            Priority::P1
        );
        assert_eq!(
            Priority::from_labels(&l(&[("severity", "warning")])),
            Priority::P2
        );
        assert_eq!(Priority::from_labels(&l(&[])), Priority::P4);
    }

    #[test]
    fn runbook_slug_is_the_docs_runbooks_file_stem() {
        let base = "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks";
        assert_eq!(
            alert(&[], &format!("{base}/indexer-lag.md"))
                .runbook_slug()
                .as_deref(),
            Some("indexer-lag")
        );
        assert_eq!(
            alert(&[], &format!("{base}/api-error-budget.md#fast-burn"))
                .runbook_slug()
                .as_deref(),
            Some("api-error-budget")
        );
        assert_eq!(
            alert(&[], "https://example.com/README.md#runbook").runbook_slug(),
            None
        );
    }

    #[test]
    fn identity_falls_back_to_sorted_labels() {
        let a = alert(&[("b", "2"), ("alertname", "X")], "");
        assert_eq!(a.identity(), "alertname=X,b=2");
    }
}
