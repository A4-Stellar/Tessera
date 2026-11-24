//! Chaos tests for the triage bot (issue #474).
//!
//! A simulated cluster stands in for Kubernetes, Redis and PostgreSQL, and
//! faults are injected into it: an indexer that stops advancing, a
//! replacement pod that never becomes ready, a Redis that fails mid-flush, an
//! audit trail that is down. A P1 alert is then delivered as the Alertmanager
//! webhook body that Grafana OnCall forwards, and the tests check what the bot
//! did and how long it took.
//!
//! The tokio clock is paused, so the 90 s bound is checked against simulated
//! time: pod start-up delays, poll intervals and step timeouts all advance
//! the same clock the bot measures with.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tessera_orchestrator::triage::actions::{
    CacheStore, CachedKey, ConnectionStats, ConnectionUsage, FlushRedisCache, PodHealth,
    PostgresConnectionCheck, RestartStuckIndexer, RolloutStatus, Workloads,
};
use tessera_orchestrator::triage::{
    AlertmanagerPayload, AuditPhase, AuditRecord, AuditSink, NotTriaged, Notifier, Outcome,
    RollbackOutcome, RunbookCatalog, TriageBot, TriagePolicy, TriageReport, TriageResult,
};
use tokio::time::Instant;

// ── Simulated cluster ───────────────────────────────────────────────────────

#[derive(Default)]
struct ClusterState {
    /// `kubectl.kubernetes.io/restartedAt` on the indexer pod template.
    annotation: Option<String>,
    /// Indexer pods are ready but stopped advancing (the lag alert's cause).
    indexer_stalled: bool,
    /// When a restart was requested, and how long the new pod takes to start.
    restart_requested_at: Option<Instant>,
    pod_startup: Duration,
    /// Fault: the replacement pod never becomes ready.
    replacement_never_ready: bool,
    /// Moment the cluster first had a healthy, advancing indexer again.
    recovered_at: Option<Instant>,
    patches: Vec<Option<String>>,

    cache: BTreeMap<String, String>,
    /// Fault: deleting from Redis fails after removing this many keys.
    fail_cache_delete_after: Option<usize>,

    connections: ConnectionUsage,
}

#[derive(Clone)]
struct Cluster(Arc<Mutex<ClusterState>>);

impl Cluster {
    fn new() -> Self {
        let mut s = ClusterState {
            annotation: Some("2026-11-01T00:00:00Z".into()),
            pod_startup: Duration::from_secs(25),
            connections: ConnectionUsage {
                in_use: 42,
                max: 97,
            },
            ..Default::default()
        };
        for i in 0..3 {
            s.cache
                .insert(format!("cache:v1:assets:{i}"), format!("stale-{i}"));
        }
        s.cache.insert("tessera:session:abc".into(), "keep".into());
        Cluster(Arc::new(Mutex::new(s)))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ClusterState> {
        self.0.lock().unwrap()
    }

    /// Advances the simulated rollout to the current instant.
    fn tick(&self) {
        let mut s = self.state();
        if let Some(at) = s.restart_requested_at {
            if !s.replacement_never_ready && Instant::now() >= at + s.pod_startup {
                s.indexer_stalled = false;
                if s.recovered_at.is_none() {
                    s.recovered_at = Some(at + s.pod_startup);
                }
            }
        }
    }
}

#[async_trait]
impl Workloads for Cluster {
    async fn pods(&self, _deployment: &str) -> Result<Vec<PodHealth>, String> {
        self.tick();
        // A stalled indexer still passes its readiness probe: only the lag
        // alert reveals it.
        Ok(vec![PodHealth {
            name: "tessera-indexer-7c9f-0".into(),
            ready: true,
            waiting_reason: None,
        }])
    }

    async fn restart_annotation(&self, _deployment: &str) -> Result<Option<String>, String> {
        Ok(self.state().annotation.clone())
    }

    async fn set_restart_annotation(&self, _d: &str, value: Option<&str>) -> Result<(), String> {
        let mut s = self.state();
        s.annotation = value.map(str::to_string);
        let patched = s.annotation.clone();
        s.patches.push(patched);
        s.restart_requested_at = Some(Instant::now());
        Ok(())
    }

    async fn rollout_status(&self, _deployment: &str) -> Result<RolloutStatus, String> {
        self.tick();
        let s = self.state();
        let started = s
            .restart_requested_at
            .is_some_and(|at| Instant::now() >= at + s.pod_startup);
        let ready = !s.replacement_never_ready && started;
        Ok(RolloutStatus {
            desired: 1,
            updated: 1,
            ready: u32::from(ready),
            observed: true,
        })
    }
}

#[async_trait]
impl CacheStore for Cluster {
    async fn keys(&self, pattern: &str, limit: usize) -> Result<Vec<String>, String> {
        let prefix = pattern.trim_end_matches('*');
        Ok(self
            .state()
            .cache
            .keys()
            .filter(|k| k.starts_with(prefix))
            .take(limit + 1)
            .cloned()
            .collect())
    }

    async fn dump(&self, keys: &[String]) -> Result<Vec<CachedKey>, String> {
        let s = self.state();
        Ok(keys
            .iter()
            .filter_map(|k| {
                s.cache.get(k).map(|v| CachedKey {
                    key: k.clone(),
                    dump_hex: hex::encode(v),
                    pttl_ms: 0,
                })
            })
            .collect())
    }

    async fn delete(&self, keys: &[String]) -> Result<u64, String> {
        let mut s = self.state();
        let mut n = 0;
        for k in keys {
            if s.fail_cache_delete_after == Some(n as usize) {
                return Err("READONLY You can't write against a read only replica.".into());
            }
            if s.cache.remove(k).is_some() {
                n += 1;
            }
        }
        Ok(n)
    }

    async fn restore(&self, entries: &[CachedKey]) -> Result<(), String> {
        let mut s = self.state();
        for e in entries {
            let v = String::from_utf8(hex::decode(&e.dump_hex).unwrap()).unwrap();
            s.cache.insert(e.key.clone(), v);
        }
        Ok(())
    }
}

#[async_trait]
impl ConnectionStats for Cluster {
    async fn connection_usage(&self) -> Result<ConnectionUsage, String> {
        Ok(self.state().connections)
    }
}

#[derive(Default)]
struct Slack(Mutex<Vec<TriageReport>>);

#[async_trait]
impl Notifier for Slack {
    async fn post(&self, report: &TriageReport) -> Result<(), String> {
        self.0.lock().unwrap().push(report.clone());
        Ok(())
    }
}

#[derive(Default)]
struct Audit {
    records: Mutex<Vec<AuditRecord>>,
    down: Mutex<bool>,
}

#[async_trait]
impl AuditSink for Audit {
    async fn record(&self, record: AuditRecord) -> Result<(), String> {
        if *self.down.lock().unwrap() {
            return Err("audit API unavailable".into());
        }
        self.records.lock().unwrap().push(record);
        Ok(())
    }
}

struct Harness {
    cluster: Cluster,
    slack: Arc<Slack>,
    audit: Arc<Audit>,
    bot: TriageBot,
}

fn harness() -> Harness {
    let cluster = Cluster::new();
    let slack = Arc::new(Slack::default());
    let audit = Arc::new(Audit::default());
    let indexer = RestartStuckIndexer::new(Arc::new(cluster.clone()), "tessera-indexer")
        .with_clock(|| "2026-11-24T12:00:00Z".to_string());
    let bot = TriageBot::new(
        RunbookCatalog::standard(
            Arc::new(PostgresConnectionCheck::new(Arc::new(cluster.clone()))),
            Arc::new(indexer),
            Arc::new(FlushRedisCache::new(Arc::new(cluster.clone()))),
        ),
        slack.clone(),
        audit.clone(),
        TriagePolicy::default(),
    );
    Harness {
        cluster,
        slack,
        audit,
        bot,
    }
}

/// The body Alertmanager (and Grafana OnCall's Alertmanager integration)
/// POSTs for the `HighIndexerLag` rule in `infrastructure/monitoring/alerts.yaml`.
fn p1_indexer_lag() -> AlertmanagerPayload {
    serde_json::from_value(serde_json::json!({
        "version": "4",
        "status": "firing",
        "receiver": "grafana-oncall",
        "alerts": [{
            "status": "firing",
            "labels": { "alertname": "HighIndexerLag", "severity": "critical", "priority": "P1", "service": "indexer" },
            "annotations": {
                "summary": "Indexer is more than ten ledgers behind",
                "runbook_url": "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/indexer-lag.md"
            },
            "startsAt": "2026-11-24T12:00:00Z",
            "fingerprint": "6f1c0d2a9b7e4c11"
        }]
    }))
    .unwrap()
}

fn report(results: Vec<TriageResult>) -> TriageReport {
    match results.into_iter().next() {
        Some(TriageResult::Triaged(r)) => r,
        other => panic!("expected a triage report, got {other:?}"),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn p1_alert_restarts_a_stalled_indexer_within_90_seconds() {
    let h = harness();
    h.cluster.state().indexer_stalled = true;

    let fired_at = Instant::now();
    let report = report(h.bot.handle_alertmanager(p1_indexer_lag()).await);

    // Corrective action: the indexer was restarted and is advancing again,
    // well inside the 90 s budget measured from the alert.
    let s = h.cluster.state();
    let recovered = s.recovered_at.expect("indexer never recovered");
    assert!(!s.indexer_stalled);
    assert!(
        recovered - fired_at <= Duration::from_secs(90),
        "recovered after {:?}",
        recovered - fired_at
    );
    assert!(Instant::now() - fired_at <= Duration::from_secs(90));
    assert_eq!(s.patches, vec![Some("2026-11-24T12:00:00Z".to_string())]);

    // The runbook ran in order, and only the cache prefix was flushed.
    let steps: Vec<(&str, Outcome)> = report
        .steps
        .iter()
        .map(|s| (s.action.as_str(), s.outcome))
        .collect();
    assert_eq!(
        steps,
        vec![
            ("check_postgres_connections", Outcome::Healthy),
            ("restart_indexer_pod", Outcome::Remediated),
            ("flush_redis_cache", Outcome::Remediated),
        ]
    );
    assert_eq!(
        s.cache.keys().collect::<Vec<_>>(),
        vec!["tessera:session:abc"]
    );
    assert!(!report.escalated);
    assert_eq!(report.runbook.as_deref(), Some("indexer-lag"));
    assert_eq!(report.steps[1].next_step, "flush_redis_cache");
    assert_eq!(report.steps[2].next_step, "monitor alert until it resolves");
    drop(s);

    // Slack got the structured report.
    let posted = h.slack.0.lock().unwrap();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0], report);

    // Every action has an intent and a result in the audit trail.
    let audit = h.audit.records.lock().unwrap();
    let phases: Vec<(AuditPhase, &str)> =
        audit.iter().map(|r| (r.phase, r.action.as_str())).collect();
    assert_eq!(
        phases,
        vec![
            (AuditPhase::Intent, "check_postgres_connections"),
            (AuditPhase::Result, "check_postgres_connections"),
            (AuditPhase::Intent, "restart_indexer_pod"),
            (AuditPhase::Result, "restart_indexer_pod"),
            (AuditPhase::Intent, "flush_redis_cache"),
            (AuditPhase::Result, "flush_redis_cache"),
        ]
    );
    assert!(audit.iter().all(|r| r.fingerprint == "6f1c0d2a9b7e4c11"));
}

#[tokio::test(start_paused = true)]
async fn replacement_pod_that_never_becomes_ready_is_rolled_back_and_escalated() {
    let h = harness();
    {
        let mut s = h.cluster.state();
        s.indexer_stalled = true;
        s.replacement_never_ready = true;
    }

    let fired_at = Instant::now();
    let report = report(h.bot.handle_alertmanager(p1_indexer_lag()).await);

    let restart = &report.steps[1];
    assert_eq!(restart.action, "restart_indexer_pod");
    assert_eq!(restart.outcome, Outcome::TimedOut);
    assert_eq!(restart.rollback, Some(RollbackOutcome::RolledBack));
    assert!(restart.next_step.starts_with("escalated to on-call"));
    assert!(restart.next_step.ends_with("docs/runbooks/indexer-lag.md"));
    // The step timeout (60 s) bounds the attempt.
    assert!(Instant::now() - fired_at <= Duration::from_secs(61));

    // Rollback restored the previous pod-template annotation.
    let s = h.cluster.state();
    assert_eq!(
        s.patches,
        vec![
            Some("2026-11-24T12:00:00Z".into()),
            Some("2026-11-01T00:00:00Z".into())
        ]
    );
    assert_eq!(s.annotation.as_deref(), Some("2026-11-01T00:00:00Z"));

    // Nothing else ran after the failure, and the cache is untouched.
    assert_eq!(report.steps[2].outcome, Outcome::Skipped);
    assert_eq!(s.cache.len(), 4);
    assert!(report.escalated);
    drop(s);

    let audit = h.audit.records.lock().unwrap();
    assert!(audit
        .iter()
        .any(|r| r.phase == AuditPhase::Rollback && r.action == "restart_indexer_pod"));
    assert!(!audit.iter().any(|r| r.action == "flush_redis_cache"));
}

#[tokio::test(start_paused = true)]
async fn failed_cache_flush_restores_the_snapshot() {
    let h = harness();
    h.cluster.state().fail_cache_delete_after = Some(1);
    let alert = serde_json::json!({ "alerts": [{
        "status": "firing",
        "labels": { "alertname": "HighErrorRate", "severity": "critical" },
        "annotations": { "runbook_url": "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/api-errors.md" }
    }]});

    let report = report(
        h.bot
            .handle_alertmanager(serde_json::from_value(alert).unwrap())
            .await,
    );

    let flush = report.steps.last().unwrap();
    assert_eq!(flush.action, "flush_redis_cache");
    assert_eq!(flush.outcome, Outcome::Failed);
    assert_eq!(flush.rollback, Some(RollbackOutcome::RolledBack));
    let s = h.cluster.state();
    assert_eq!(s.cache.len(), 4, "every flushed key is restored");
    assert_eq!(s.cache["cache:v1:assets:0"], "stale-0");
    // This runbook never restarts the indexer.
    assert!(s.patches.is_empty());
}

#[tokio::test(start_paused = true)]
async fn no_action_is_taken_when_the_audit_trail_is_down() {
    let h = harness();
    h.cluster.state().indexer_stalled = true;
    *h.audit.down.lock().unwrap() = true;

    let report = report(h.bot.handle_alertmanager(p1_indexer_lag()).await);

    assert_eq!(report.steps[0].outcome, Outcome::Failed);
    assert!(report.steps[0].detail.contains("audit trail unavailable"));
    assert!(report.steps[1..]
        .iter()
        .all(|s| s.outcome == Outcome::Skipped));
    let s = h.cluster.state();
    assert!(s.patches.is_empty());
    assert_eq!(s.cache.len(), 4);
    assert!(report.escalated);
}

#[tokio::test(start_paused = true)]
async fn exhausted_connection_pool_is_reported_as_degraded_without_stopping_the_runbook() {
    let h = harness();
    h.cluster.state().connections = ConnectionUsage {
        in_use: 95,
        max: 97,
    };
    h.cluster.state().indexer_stalled = true;

    let report = report(h.bot.handle_alertmanager(p1_indexer_lag()).await);

    assert_eq!(report.steps[0].outcome, Outcome::Degraded);
    assert!(report.steps[0].detail.contains("95 of 97"));
    assert_eq!(report.steps[1].outcome, Outcome::Remediated);
    assert!(!report.escalated);
}

#[tokio::test(start_paused = true)]
async fn repeats_resolved_and_low_priority_alerts_are_not_triaged() {
    let h = harness();
    h.cluster.state().indexer_stalled = true;

    assert!(matches!(
        h.bot.handle_alertmanager(p1_indexer_lag()).await[0],
        TriageResult::Triaged(_)
    ));
    // Alertmanager re-sends the same firing alert on `repeat_interval`.
    assert_eq!(
        h.bot.handle_alertmanager(p1_indexer_lag()).await,
        vec![TriageResult::Ignored(NotTriaged::CoolingDown)]
    );
    assert_eq!(
        h.cluster.state().patches.len(),
        1,
        "the indexer is restarted once"
    );

    let mut resolved = p1_indexer_lag();
    resolved.alerts[0].status = "resolved".into();
    resolved.alerts[0].fingerprint = "other".into();
    assert_eq!(
        h.bot.handle_alertmanager(resolved).await,
        vec![TriageResult::Ignored(NotTriaged::Resolved)]
    );

    let mut warning = p1_indexer_lag();
    warning.alerts[0].fingerprint = "warn".into();
    warning.alerts[0]
        .labels
        .insert("priority".into(), "P2".into());
    assert!(matches!(
        h.bot.handle_alertmanager(warning).await[0],
        TriageResult::Ignored(NotTriaged::BelowP1(_))
    ));
    assert_eq!(h.slack.0.lock().unwrap().len(), 1);
}
