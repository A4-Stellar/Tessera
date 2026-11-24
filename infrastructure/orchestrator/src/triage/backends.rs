//! Production backends for the triage bot: PostgreSQL (`sqlx`), the
//! Kubernetes API (in-cluster service account over `reqwest`), Redis, a Slack
//! incoming webhook and the Tessera API's hash-chained audit trail.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

use super::actions::{
    CacheStore, CachedKey, ConnectionStats, ConnectionUsage, PodHealth, RolloutStatus, Workloads,
};
use super::{AuditRecord, AuditSink, Notifier, Outcome, TriageReport};

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())
}

// ── PostgreSQL ──────────────────────────────────────────────────────────────

const CONNECTIONS_SQL: &str = "SELECT \
     (SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'client backend')::int8 AS in_use, \
     (current_setting('max_connections')::int8 \
        - current_setting('superuser_reserved_connections')::int8) AS max";

pub struct PgConnectionStats {
    pool: PgPool,
}

impl PgConnectionStats {
    /// A lazy single-connection pool: the check needs one connection even
    /// when the database is close to its limit.
    pub fn connect_lazy(dsn: &str, timeout: Duration) -> Result<Self, String> {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(timeout)
            .connect_lazy(dsn)
            .map_err(|e| e.to_string())?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl ConnectionStats for PgConnectionStats {
    async fn connection_usage(&self) -> Result<ConnectionUsage, String> {
        let row = sqlx::query(CONNECTIONS_SQL)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| e.to_string())?;
        let in_use: i64 = row.try_get("in_use").map_err(|e| e.to_string())?;
        let max: i64 = row.try_get("max").map_err(|e| e.to_string())?;
        Ok(ConnectionUsage {
            in_use: u32::try_from(in_use.max(0)).unwrap_or(u32::MAX),
            max: u32::try_from(max.max(0)).unwrap_or(u32::MAX),
        })
    }
}

// ── Kubernetes ──────────────────────────────────────────────────────────────

const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
const RESTARTED_AT: &str = "kubectl.kubernetes.io/restartedAt";

/// Talks to the Kubernetes API with the pod's service account. The account
/// needs `get`/`patch` on `deployments` and `list` on `pods` in `namespace`
/// (see `infrastructure/k8s/triage-bot/rbac.yaml`).
pub struct KubernetesWorkloads {
    client: reqwest::Client,
    base: String,
    token: String,
    namespace: String,
}

impl KubernetesWorkloads {
    pub fn new(
        base: String,
        token: String,
        namespace: String,
        ca_pem: Option<&[u8]>,
    ) -> Result<Self, String> {
        let mut builder = reqwest::Client::builder().timeout(HTTP_TIMEOUT);
        if let Some(pem) = ca_pem {
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(pem).map_err(|e| e.to_string())?,
            );
        }
        Ok(Self {
            client: builder.build().map_err(|e| e.to_string())?,
            base: base.trim_end_matches('/').to_string(),
            token,
            namespace,
        })
    }

    /// Configuration from the standard in-cluster environment.
    pub fn in_cluster() -> Result<Self, String> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .map_err(|_| "KUBERNETES_SERVICE_HOST is not set")?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
        let read = |f: &str| {
            std::fs::read(format!("{SA_DIR}/{f}")).map_err(|e| format!("{SA_DIR}/{f}: {e}"))
        };
        let token = String::from_utf8(read("token")?).map_err(|e| e.to_string())?;
        let namespace = std::env::var("TRIAGE_NAMESPACE").or_else(|_| {
            read("namespace").and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()))
        })?;
        let ca = read("ca.crt")?;
        Self::new(
            format!("https://{host}:{port}"),
            token.trim().to_string(),
            namespace.trim().to_string(),
            Some(&ca),
        )
    }

    fn deployment_url(&self, name: &str) -> String {
        format!(
            "{}/apis/apps/v1/namespaces/{}/deployments/{name}",
            self.base, self.namespace
        )
    }

    async fn get_json(&self, url: &str) -> Result<Value, String> {
        let resp = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GET {url}: HTTP {status}"));
        }
        resp.json().await.map_err(|e| e.to_string())
    }

    async fn deployment(&self, name: &str) -> Result<Value, String> {
        self.get_json(&self.deployment_url(name)).await
    }
}

/// `matchLabels` of a deployment as a label selector string.
fn label_selector(deployment: &Value) -> Result<String, String> {
    let labels = deployment
        .pointer("/spec/selector/matchLabels")
        .and_then(Value::as_object)
        .ok_or("deployment has no spec.selector.matchLabels")?;
    Ok(labels
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(","))
}

fn pod_health(pod: &Value) -> PodHealth {
    let name = pod
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let ready = pod
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|c| {
            c.iter()
                .any(|c| c["type"] == "Ready" && c["status"] == "True")
        });
    let waiting_reason = pod
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .and_then(|s| {
            s.iter()
                .find_map(|s| s.pointer("/state/waiting/reason").and_then(Value::as_str))
        })
        .map(str::to_string);
    PodHealth {
        name,
        ready,
        waiting_reason,
    }
}

fn rollout_status(deployment: &Value) -> RolloutStatus {
    let n = |p: &str| deployment.pointer(p).and_then(Value::as_u64).unwrap_or(0) as u32;
    let generation = deployment
        .pointer("/metadata/generation")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let observed = deployment
        .pointer("/status/observedGeneration")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    RolloutStatus {
        desired: deployment
            .pointer("/spec/replicas")
            .and_then(Value::as_u64)
            .unwrap_or(1) as u32,
        updated: n("/status/updatedReplicas"),
        ready: n("/status/readyReplicas"),
        observed: observed >= generation,
    }
}

#[async_trait]
impl Workloads for KubernetesWorkloads {
    async fn pods(&self, deployment: &str) -> Result<Vec<PodHealth>, String> {
        let selector = label_selector(&self.deployment(deployment).await?)?;
        let url = reqwest::Url::parse_with_params(
            &format!("{}/api/v1/namespaces/{}/pods", self.base, self.namespace),
            &[("labelSelector", selector)],
        )
        .map_err(|e| e.to_string())?;
        let list = self.get_json(url.as_str()).await?;
        Ok(list["items"]
            .as_array()
            .map(|items| items.iter().map(pod_health).collect())
            .unwrap_or_default())
    }

    async fn restart_annotation(&self, deployment: &str) -> Result<Option<String>, String> {
        Ok(self
            .deployment(deployment)
            .await?
            .pointer("/spec/template/metadata/annotations")
            .and_then(|a| a.get(RESTARTED_AT))
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    async fn set_restart_annotation(
        &self,
        deployment: &str,
        value: Option<&str>,
    ) -> Result<(), String> {
        // In a strategic merge patch `null` deletes the key.
        let patch = json!({ "spec": { "template": { "metadata": { "annotations": { RESTARTED_AT: value } } } } });
        let url = self.deployment_url(deployment);
        let resp = self
            .client
            .patch(&url)
            .bearer_auth(&self.token)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/strategic-merge-patch+json",
            )
            .body(patch.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("PATCH {url}: HTTP {}", resp.status()))
        }
    }

    async fn rollout_status(&self, deployment: &str) -> Result<RolloutStatus, String> {
        Ok(rollout_status(&self.deployment(deployment).await?))
    }
}

// ── Redis ───────────────────────────────────────────────────────────────────

pub struct RedisCache {
    client: redis::Client,
}

impl RedisCache {
    pub fn open(url: &str) -> Result<Self, String> {
        Ok(Self {
            client: redis::Client::open(url).map_err(|e| e.to_string())?,
        })
    }

    async fn conn(&self) -> Result<redis::aio::MultiplexedConnection, String> {
        self.client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| e.to_string())
    }
}

#[async_trait]
impl CacheStore for RedisCache {
    async fn keys(&self, pattern: &str, limit: usize) -> Result<Vec<String>, String> {
        let mut con = self.conn().await?;
        let mut cursor: u64 = 0;
        let mut keys = Vec::new();
        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(500)
                .query_async(&mut con)
                .await
                .map_err(|e| e.to_string())?;
            keys.extend(batch);
            if next == 0 || keys.len() > limit {
                break;
            }
            cursor = next;
        }
        keys.sort();
        keys.dedup();
        keys.truncate(limit + 1);
        Ok(keys)
    }

    async fn dump(&self, keys: &[String]) -> Result<Vec<CachedKey>, String> {
        let mut con = self.conn().await?;
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            let (payload, pttl): (Option<Vec<u8>>, i64) = redis::pipe()
                .cmd("DUMP")
                .arg(key)
                .cmd("PTTL")
                .arg(key)
                .query_async(&mut con)
                .await
                .map_err(|e| e.to_string())?;
            // A key that expired between SCAN and DUMP has nothing to restore.
            if let Some(payload) = payload {
                out.push(CachedKey {
                    key: key.clone(),
                    dump_hex: hex::encode(payload),
                    pttl_ms: u64::try_from(pttl).unwrap_or(0),
                });
            }
        }
        Ok(out)
    }

    async fn delete(&self, keys: &[String]) -> Result<u64, String> {
        let mut con = self.conn().await?;
        let mut deleted = 0;
        for chunk in keys.chunks(500) {
            let n: u64 = redis::cmd("UNLINK")
                .arg(chunk)
                .query_async(&mut con)
                .await
                .map_err(|e| e.to_string())?;
            deleted += n;
        }
        Ok(deleted)
    }

    async fn restore(&self, entries: &[CachedKey]) -> Result<(), String> {
        let mut con = self.conn().await?;
        for entry in entries {
            let payload = hex::decode(&entry.dump_hex).map_err(|e| e.to_string())?;
            let () = redis::cmd("RESTORE")
                .arg(&entry.key)
                .arg(entry.pttl_ms)
                .arg(payload)
                .arg("REPLACE")
                .query_async(&mut con)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

// ── Slack ───────────────────────────────────────────────────────────────────

/// Posts the report to a Slack incoming webhook. The message has a short
/// human summary plus the structured `{action, outcome, duration_ms,
/// next_step}` list as a JSON code block.
pub struct SlackWebhook {
    client: reqwest::Client,
    url: String,
}

impl SlackWebhook {
    pub fn new(url: String) -> Result<Self, String> {
        Ok(Self {
            client: http_client()?,
            url,
        })
    }
}

/// The Slack message body for a report.
pub fn slack_message(report: &TriageReport) -> Value {
    let icon = |o: Outcome| match o {
        Outcome::Healthy | Outcome::Remediated => ":white_check_mark:",
        Outcome::Degraded => ":warning:",
        Outcome::Skipped => ":fast_forward:",
        Outcome::Failed | Outcome::TimedOut => ":x:",
    };
    let headline = if report.escalated {
        format!(
            ":rotating_light: Automated triage for *{}* needs a human",
            report.alert
        )
    } else {
        format!(
            ":robot_face: Automated triage for *{}* finished",
            report.alert
        )
    };
    let lines = report
        .steps
        .iter()
        .map(|s| {
            format!(
                "{} `{}` {:?}: {} ({} ms)",
                icon(s.outcome),
                s.action,
                s.outcome,
                s.detail,
                s.duration_ms
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let structured: Vec<Value> = report
        .steps
        .iter()
        .map(|s| json!({ "action": s.action, "outcome": s.outcome, "duration_ms": s.duration_ms, "next_step": s.next_step }))
        .collect();
    let structured = serde_json::to_string_pretty(&structured).unwrap_or_default();
    let runbook = report.runbook_url.as_deref().unwrap_or("n/a");
    json!({
        "text": headline,
        "blocks": [
            { "type": "section", "text": { "type": "mrkdwn", "text": headline } },
            { "type": "section", "text": { "type": "mrkdwn", "text": lines } },
            { "type": "section", "text": { "type": "mrkdwn", "text": format!("```{structured}```") } },
            { "type": "context", "elements": [ { "type": "mrkdwn", "text": format!("runbook: {runbook} · {} ms total", report.total_duration_ms) } ] }
        ]
    })
}

#[async_trait]
impl Notifier for SlackWebhook {
    async fn post(&self, report: &TriageReport) -> Result<(), String> {
        let resp = self
            .client
            .post(&self.url)
            .json(&slack_message(report))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("Slack webhook: HTTP {}", resp.status()))
        }
    }
}

// ── Audit trail ─────────────────────────────────────────────────────────────

/// Appends to the API's immutable, hash-chained audit log via
/// `POST /v1/audit/entries`.
pub struct ApiAuditSink {
    client: reqwest::Client,
    url: String,
    api_key: Option<String>,
}

impl ApiAuditSink {
    pub fn new(api_base: &str, api_key: Option<String>) -> Result<Self, String> {
        Ok(Self {
            client: http_client()?,
            url: format!("{}/v1/audit/entries", api_base.trim_end_matches('/')),
            api_key,
        })
    }
}

/// The `POST /v1/audit/entries` body for a record.
pub fn audit_entry_body(record: &AuditRecord) -> Value {
    let phase = serde_json::to_value(record.phase).unwrap_or_default();
    json!({
        "action": {
            "type": "administrative_action",
            "action": format!("triage.{}.{}", phase.as_str().unwrap_or("result"), record.action),
            "details": record.details,
        },
        "actor": "tessera-triage-bot",
        "target": record.fingerprint,
        "payload": {
            "alert": record.alert,
            "runbook": record.runbook,
            "phase": record.phase,
        },
    })
}

#[async_trait]
impl AuditSink for ApiAuditSink {
    async fn record(&self, record: AuditRecord) -> Result<(), String> {
        let mut req = self.client.post(&self.url).json(&audit_entry_body(&record));
        if let Some(key) = &self.api_key {
            req = req.header("x-api-key", key);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("audit trail: HTTP {}", resp.status()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::{AuditPhase, Priority, StepReport};

    #[test]
    fn pod_health_reads_ready_condition_and_waiting_reason() {
        let pod = json!({
            "metadata": { "name": "indexer-0" },
            "status": {
                "conditions": [ { "type": "Ready", "status": "False" } ],
                "containerStatuses": [ { "state": { "waiting": { "reason": "CrashLoopBackOff" } } } ]
            }
        });
        assert_eq!(
            pod_health(&pod),
            PodHealth {
                name: "indexer-0".into(),
                ready: false,
                waiting_reason: Some("CrashLoopBackOff".into())
            }
        );
    }

    #[test]
    fn rollout_is_complete_only_when_observed_updated_and_ready() {
        let d = |gen: u64, obs: u64, upd: u64, ready: u64| {
            json!({
                "metadata": { "generation": gen },
                "spec": { "replicas": 2, "selector": { "matchLabels": { "app": "indexer" } } },
                "status": { "observedGeneration": obs, "updatedReplicas": upd, "readyReplicas": ready }
            })
        };
        assert!(rollout_status(&d(3, 3, 2, 2)).complete());
        assert!(!rollout_status(&d(3, 2, 2, 2)).complete());
        assert!(!rollout_status(&d(3, 3, 1, 2)).complete());
        assert_eq!(label_selector(&d(1, 1, 1, 1)).unwrap(), "app=indexer");
    }

    #[test]
    fn slack_message_carries_the_structured_step_list() {
        let report = TriageReport {
            alert: "HighIndexerLag".into(),
            fingerprint: "f".into(),
            priority: Priority::P1,
            runbook: Some("indexer-lag".into()),
            runbook_url: Some("docs/runbooks/indexer-lag.md".into()),
            steps: vec![StepReport {
                action: "restart_indexer_pod".into(),
                outcome: Outcome::Remediated,
                duration_ms: 1200,
                next_step: "flush_redis_cache".into(),
                detail: "restarted".into(),
                rollback: None,
            }],
            escalated: false,
            total_duration_ms: 1300,
        };
        let msg = slack_message(&report);
        let code = msg["blocks"][2]["text"]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(code.trim_matches('`')).unwrap();
        assert_eq!(
            parsed,
            json!([{ "action": "restart_indexer_pod", "outcome": "remediated", "duration_ms": 1200, "next_step": "flush_redis_cache" }])
        );
    }

    #[test]
    fn audit_body_matches_the_api_administrative_action_shape() {
        let body = audit_entry_body(&AuditRecord {
            phase: AuditPhase::Intent,
            action: "flush_redis_cache".into(),
            alert: "HighErrorRate".into(),
            fingerprint: "abc".into(),
            runbook: Some("api-errors".into()),
            details: json!({ "timeout_ms": 10000 }),
        });
        assert_eq!(body["action"]["type"], "administrative_action");
        assert_eq!(body["action"]["action"], "triage.intent.flush_redis_cache");
        assert_eq!(body["actor"], "tessera-triage-bot");
        assert_eq!(body["target"], "abc");
    }
}
