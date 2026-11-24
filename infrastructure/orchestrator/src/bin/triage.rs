//! `tessera-triage`: receives P1 alerts from Grafana OnCall / Alertmanager
//! and runs the automated part of their runbook (issue #474).
//!
//! Environment:
//!
//! | variable | default | meaning |
//! |---|---|---|
//! | `TRIAGE_LISTEN` | `0.0.0.0:8085` | listen address |
//! | `TRIAGE_WEBHOOK_TOKEN` | required | bearer token the webhooks must send |
//! | `TRIAGE_DATABASE_URL` | required | API PostgreSQL DSN (role with `pg_monitor`) |
//! | `TRIAGE_REDIS_URL` | required | Redis holding the API response cache |
//! | `TRIAGE_CACHE_PATTERN` | `cache:v1:*` | keys the cache flush may delete |
//! | `TRIAGE_INDEXER_DEPLOYMENT` | `tessera-indexer` | deployment restarted when stuck |
//! | `TRIAGE_SLACK_WEBHOOK_URL` | required | Slack incoming webhook |
//! | `TRIAGE_AUDIT_API_URL` | required | Tessera API base URL for `/v1/audit/entries` |
//! | `TRIAGE_AUDIT_API_KEY` | unset | sent as `x-api-key` to the audit API |

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use sha2::{Digest, Sha256};
use tessera_orchestrator::triage::actions::{
    FlushRedisCache, PostgresConnectionCheck, RestartStuckIndexer,
};
use tessera_orchestrator::triage::backends::{
    ApiAuditSink, KubernetesWorkloads, PgConnectionStats, RedisCache, SlackWebhook,
};
use tessera_orchestrator::triage::{
    AlertmanagerPayload, OnCallPayload, RunbookCatalog, TriageBot, TriagePolicy,
};
use tracing::info;

struct App {
    bot: TriageBot,
    token_digest: [u8; 32],
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("environment variable {name} is not set"))
}

/// Compares SHA-256 digests so the comparison time does not depend on how
/// much of the token matched.
fn authorized(app: &App, headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| <[u8; 32]>::from(Sha256::digest(t.as_bytes())) == app.token_digest)
}

async fn alertmanager(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(payload): Json<AlertmanagerPayload>,
) -> StatusCode {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED;
    }
    // Runbooks take up to 90 s; webhook senders time out sooner, so the work
    // continues in the background and the sender gets 202 immediately.
    tokio::spawn(async move {
        for result in app.bot.handle_alertmanager(payload).await {
            info!(result = %serde_json::to_string(&result).unwrap_or_default(), "triage finished");
        }
    });
    StatusCode::ACCEPTED
}

async fn oncall(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(payload): Json<OnCallPayload>,
) -> StatusCode {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED;
    }
    if payload.alert_payload.is_none() {
        return StatusCode::UNPROCESSABLE_ENTITY;
    }
    tokio::spawn(async move {
        if let Some(result) = app.bot.handle_oncall(payload).await {
            info!(result = %serde_json::to_string(&result).unwrap_or_default(), "triage finished");
        }
    });
    StatusCode::ACCEPTED
}

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tessera_orchestrator=info,triage=info".into()),
        )
        .json()
        .init();

    let postgres = Arc::new(PostgresConnectionCheck::new(Arc::new(
        PgConnectionStats::connect_lazy(&env("TRIAGE_DATABASE_URL")?, Duration::from_secs(3))?,
    )));
    let deployment =
        std::env::var("TRIAGE_INDEXER_DEPLOYMENT").unwrap_or_else(|_| "tessera-indexer".into());
    let indexer = Arc::new(RestartStuckIndexer::new(
        Arc::new(KubernetesWorkloads::in_cluster()?),
        deployment,
    ));
    let mut cache = FlushRedisCache::new(Arc::new(RedisCache::open(&env("TRIAGE_REDIS_URL")?)?));
    if let Ok(pattern) = std::env::var("TRIAGE_CACHE_PATTERN") {
        cache.pattern = pattern;
    }

    let bot = TriageBot::new(
        RunbookCatalog::standard(postgres, indexer, Arc::new(cache)),
        Arc::new(SlackWebhook::new(env("TRIAGE_SLACK_WEBHOOK_URL")?)?),
        Arc::new(ApiAuditSink::new(
            &env("TRIAGE_AUDIT_API_URL")?,
            std::env::var("TRIAGE_AUDIT_API_KEY").ok(),
        )?),
        TriagePolicy::default(),
    );
    let token = env("TRIAGE_WEBHOOK_TOKEN")?;
    if token.len() < 16 {
        return Err("TRIAGE_WEBHOOK_TOKEN must be at least 16 characters".into());
    }
    let app = Arc::new(App {
        bot,
        token_digest: Sha256::digest(token.as_bytes()).into(),
    });

    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/webhook/alertmanager", post(alertmanager))
        .route("/webhook/oncall", post(oncall))
        .with_state(app);

    let listen = std::env::var("TRIAGE_LISTEN").unwrap_or_else(|_| "0.0.0.0:8085".into());
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .map_err(|e| format!("binding {listen}: {e}"))?;
    info!(%listen, "triage bot listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}
