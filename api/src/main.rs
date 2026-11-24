//! Stellar RWA API — a read-only REST index of tokenized real-world asset
//! activity on Stellar.
//!
//! The server starts the background indexer (which polls Soroban RPC every 10s)
//! and serves the current in-memory snapshot over HTTP. It holds no secrets,
//! signs nothing, and never mutates on-chain state. The `signer` module it also
//! ships is the HSM-backed signing path for the trusted jobs that *do* submit
//! transactions (dividends, rent, registry updates): the Stellar key stays
//! inside AWS KMS and only a signature ever leaves it.

mod analytics;
pub mod audit;
#[expect(
    dead_code,
    reason = "cache policy helpers are staged until response-cache integration"
)]
mod cache;
mod cluster;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "DbRouter::route/primary gain callers with the PostgreSQL persistence layer (#42)"
    )
)]
mod db;
mod healthcheck;
mod indexer;
mod middleware;
mod models;
mod routes;
pub mod rsocket;
mod security;
mod services;
pub mod storage;
pub mod tracing;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use indexer::{AppState, Config, Indexer};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use tokio::sync::watch;

/// Latency buckets (seconds) for `rwa_http_request_duration_seconds`.
/// `0.1` must stay: it is the boundary of the p99 < 100 ms SLO (issue #312).
const HTTP_LATENCY_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[tokio::main]
async fn main() {
    // Container health probe (#26): the distroless image has no curl/shell.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(healthcheck::run());
    }

    init_tracing();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "config validation failed; exiting");
            std::process::exit(1);
        }
    };
    // `tessera-api replay ...` (issue #104): admin backfill, then exit.
    if std::env::args().nth(1).as_deref() == Some("replay") {
        let rest: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(indexer::replay::run_cli(&rest, &config).await);
    }
    tracing::info!(
        rpc = ?config.rpc_urls,
        registry = %config.registry_id,
        "starting stellar-rwa-api"
    );

    // Issue #312: export request latency as a real histogram (not the
    // default summary) so Prometheus can aggregate it across replicas and
    // the SLO rules in `infrastructure/monitoring/` can count requests under
    // the 100 ms latency objective via the `le="0.1"` bucket.
    let metrics_handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full("rwa_http_request_duration_seconds".to_string()),
            HTTP_LATENCY_BUCKETS,
        )
        .expect("static latency buckets are non-empty")
        .install_recorder()
        .expect("failed to install Prometheus recorder");

    // Issue #7: `active_websocket_connections` gauge. The API currently only
    // serves plain HTTP (see the module doc above — it's a read-only REST
    // index), so this is wired up and registered at `0` rather than left
    // out entirely; it becomes live the moment a WebSocket handshake
    // handler is added, without a metric-name/dashboard-panel change.
    metrics::gauge!("active_websocket_connections").set(0.0);

    let mut state = AppState::new(config, metrics_handle);
    let audit_mmr_path = std::path::PathBuf::from(
        std::env::var("RWA_AUDIT_MMR_PATH").unwrap_or_else(|_| "./data/audit-mmr".into()),
    );
    if let Some(parent) = audit_mmr_path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            tracing::error!(%error, path = %parent.display(), "failed to create audit MMR directory");
            std::process::exit(1);
        }
    }
    match audit::AuditLog::with_rocksdb(&audit_mmr_path) {
        Ok(audit_log) => state.audit = Arc::new(audit_log),
        Err(error) => {
            tracing::error!(%error, path = %audit_mmr_path.display(), "failed to restore audit MMR");
            std::process::exit(1);
        }
    }

    // Issue #26: differential privacy for aggregate endpoints.
    match analytics::differential_privacy::PrivacyEngine::from_env() {
        Ok(engine) => state.privacy = Arc::new(engine),
        Err(e) => {
            tracing::error!(error = %e, "differential-privacy config validation failed; exiting");
            std::process::exit(1);
        }
    }

    // Shared shutdown flag: flipped once by `shutdown_signal` and observed
    // by the indexer's poll loop so it stops issuing new refresh cycles
    // once the process is terminating, rather than racing shutdown.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Issue #394: load the SPIFFE X.509 SVID for this workload. A missing
    // SVID is not fatal at startup — the rotation task keeps retrying — but a
    // revoked one terminates the process, including in-flight requests.
    match security::mtls::init_from_env() {
        Some(store) => {
            if let Err(error) = store.load_initial().await {
                tracing::warn!(%error, "no X.509 SVID available yet; rotation task will retry");
            }
            security::mtls::spawn_rotation(Arc::clone(&store), shutdown_rx.clone());
            security::mtls::spawn_revocation_enforcer(store, shutdown_rx.clone());
        }
        None => tracing::info!("SPIFFE SVID loading disabled; set SPIFFE_SVID_DIR to enable it"),
    }

    // Issue #159: only the elected leader polls Soroban RPC.
    state.node_role = cluster::leader_election::spawn(
        cluster::leader_election::ElectionConfig::from_env(),
        shutdown_rx.clone(),
    );

    // Issue #95: probe read replicas for availability and replication lag.
    match db::DbRouter::from_env() {
        Ok(Some(router)) => {
            let router = Arc::new(router);
            router.spawn_health_monitor(shutdown_rx.clone());
            if let Err(error) =
                sqlx::raw_sql(include_str!("db/migrations/0004_tenant_rate_limits.sql"))
                    .execute(router.primary())
                    .await
            {
                tracing::error!(%error, "tenant rate-limit schema initialization failed");
                std::process::exit(1);
            }
            // Issue #468: sandbox tables + FORCE ROW LEVEL SECURITY.
            if let Err(error) =
                sqlx::raw_sql(include_str!("db/migrations/0005_sandbox_tenant_rls.sql"))
                    .execute(router.primary())
                    .await
            {
                tracing::error!(%error, "sandbox RLS schema initialization failed");
                std::process::exit(1);
            }
            // Issue #469: EigenDA DA-proof records.
            if let Err(error) =
                sqlx::raw_sql(include_str!("db/migrations/0006_eigenda_da_proofs.sql"))
                    .execute(router.primary())
                    .await
            {
                tracing::error!(%error, "EigenDA schema initialization failed");
                std::process::exit(1);
            }
            // Issue #469: wire EigenDA when a disperser URL is configured.
            if std::env::var("EIGENDA_DISPERSER_URL").is_ok() {
                match indexer::eigenda::HttpDisperser::from_env() {
                    Ok(disperser) => {
                        state.da_disperser = Some(Arc::new(disperser));
                        state.da_store = Some(Arc::new(indexer::eigenda::PgProofStore::new(
                            router.primary().clone(),
                        )));
                        tracing::info!("EigenDA commitments enabled");
                    }
                    Err(error) => {
                        tracing::error!(%error, "EigenDA configuration invalid");
                        std::process::exit(1);
                    }
                }
            }
            // Issue #163: quarantine table behind `POST /v1/admin/dlq/retry`.
            state.dlq = indexer::dlq::DeadLetterQueue::open(router.primary().clone()).await;
            if let Some(dlq) = state.dlq.clone() {
                let store = indexer::replay::FileEventStore::from_env();
                let retry_worker = dlq.clone();
                let worker_shutdown = shutdown_rx.clone();
                tokio::spawn(async move {
                    retry_worker.run_worker(store, worker_shutdown).await;
                });

                let mut publisher_shutdown = shutdown_rx.clone();
                tokio::spawn(async move {
                    loop {
                        if *publisher_shutdown.borrow() {
                            break;
                        }
                        match indexer::pulsar_producer::PulsarProducer::connect(
                            indexer::pulsar_producer::PulsarConfig::from_env(),
                        )
                        .await
                        {
                            Ok(pulsar) => {
                                dlq.set_pulsar(pulsar).await;
                                break;
                            }
                            Err(error) => {
                                tracing::error!(%error, "DLQ poison-pill publisher could not connect to Pulsar; retrying");
                                tokio::select! {
                                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                                    changed = publisher_shutdown.changed() => {
                                        if changed.is_err() || *publisher_shutdown.borrow() {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::error!(error = %e, "database config validation failed; exiting");
            std::process::exit(1);
        }
    }

    // Issue #468: in sandbox mode expose a separate pool for RLS-scoped
    // reset/status endpoints. The role must not be BYPASSRLS/SUPERUSER, or the
    // tenant policies in 0005 would be silently bypassed.
    if std::env::var("RWA_SANDBOX_MODE")
        .ok()
        .is_some_and(|value| value == "true" || value == "1")
    {
        let url = std::env::var("RWA_SANDBOX_DATABASE_URL")
            .or_else(|_| std::env::var("RWA_DATABASE_URL"))
            .unwrap_or_default();
        match sqlx::postgres::PgPoolOptions::new().connect_lazy(&url) {
            Ok(pool) => {
                tracing::info!("sandbox mode enabled; RLS-scoped reset endpoints active");
                state.sandbox_db = Some(Arc::new(pool));
            }
            Err(error) => {
                tracing::error!(%error, "invalid sandbox database URL");
                std::process::exit(1);
            }
        }
    }

    // Spawn the indexer; it owns its own clone of the shared state.
    let indexer = Indexer::new(state.clone());
    tokio::spawn(async move { indexer.run(shutdown_rx).await });

    let app = routes::router(state).layer(tower_http::trace::TraceLayer::new_for_http());

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, %addr, "failed to bind");
            std::process::exit(1);
        }
    };
    tracing::info!(%addr, "listening");

    if let Err(e) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(shutdown_tx))
    .await
    {
        tracing::error!(error = %e, "server error");
        std::process::exit(1);
    }
    tracing::info!("shut down cleanly");
}

/// Resolve when the process receives Ctrl-C (SIGINT) or SIGTERM, for
/// graceful shutdown. Axum stops accepting new connections and lets
/// in-flight requests finish once this future resolves; we also flip
/// `shutdown_tx` so the indexer's poll loop halts rather than starting
/// another refresh cycle mid-shutdown.
async fn shutdown_signal(shutdown_tx: watch::Sender<bool>) {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => tracing::error!(error = %e, "failed to install SIGTERM handler"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received; finishing in-flight requests");
    let _ = shutdown_tx.send(true);
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("tessera_api=info,tower_http=warn"));
    tracing_subscriber::registry()
        .with(filter)
        // Issue #103: every log record is PII-scrubbed before it is written.
        .with(fmt::layer().with_writer(middleware::pii_scrubber::ScrubbingMakeWriter))
        .init();
}
pub mod events;
pub mod graphql;
