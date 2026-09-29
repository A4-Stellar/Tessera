//! Stellar RWA API — a read-only REST index of tokenized real-world asset
//! activity on Stellar.
//!
//! The server starts the background indexer (which polls Soroban RPC every 10s)
//! and serves the current in-memory snapshot over HTTP. It holds no secrets,
//! signs nothing, and never mutates on-chain state.

mod db;
mod healthcheck;
mod indexer;
mod models;
mod routes;

use std::net::SocketAddr;

use indexer::{AppState, Config, Indexer};
use metrics_exporter_prometheus::PrometheusBuilder;
use tokio::sync::watch;

#[tokio::main]
async fn main() {
    // Container health probe (#26): the distroless image has no curl/shell.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(healthcheck::run());
    }

    // Issue #71: schema lifecycle subcommands. These run against the database
    // and exit; they never start the HTTP server.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "migrate" => {
                std::process::exit(cli_migrate().await);
            }
            "migrate-down" => {
                let steps = args
                    .get(2)
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1);
                std::process::exit(cli_migrate_down(steps).await);
            }
            "migrate-status" => {
                std::process::exit(cli_migrate_status().await);
            }
            _ => {
                eprintln!(
                    "unknown subcommand {:?}; expected migrate, migrate-down [N], migrate-status, or no arguments to run the API server",
                    args[1]
                );
                std::process::exit(2);
            }
        }
    }

    init_tracing();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "config validation failed; exiting");
            std::process::exit(1);
        }
    };
    tracing::info!(
        rpc = %config.rpc_url,
        registry = %config.registry_id,
        "starting stellar-rwa-api"
    );

    let metrics_handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder");

    // Issue #7: `active_websocket_connections` gauge. The API currently only
    // serves plain HTTP (see the module doc above — it's a read-only REST
    // index), so this is wired up and registered at `0` rather than left
    // out entirely; it becomes live the moment a WebSocket handshake
    // handler is added, without a metric-name/dashboard-panel change.
    metrics::gauge!("active_websocket_connections").set(0.0);

    let state = AppState::new(config, metrics_handle);

    // Shared shutdown flag: flipped once by `shutdown_signal` and observed
    // by the indexer's poll loop so it stops issuing new refresh cycles
    // once the process is terminating, rather than racing shutdown.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

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

// ---------------------------------------------------------------------------
// Migration CLI (issue #71)
// ---------------------------------------------------------------------------

async fn cli_migrate() -> i32 {
    let migrations = db::embedded_migrations();
    let pool = match db::migrator::connect(None).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("migrate: {e}");
            return 1;
        }
    };
    let result =
        db::migrator::with_advisory_lock(&pool, db::migrator::up(&pool, &migrations)).await;
    match result {
        Ok((applied, blocked)) => {
            if applied.is_empty() && blocked.is_empty() {
                println!("migrate: database already up to date");
            }
            for v in &applied {
                println!("migrate: applied {:04}", v);
            }
            if !applied.is_empty() {
                println!("migrate: {} migration(s) applied", applied.len());
            }
            if let Some(v) = blocked.first() {
                // Expected during a rolling deploy: the expand phases are in,
                // the contract phase waits for the operator's go-ahead.
                eprintln!(
                    "migrate: stopped before contract (destructive) phase {:04}; \
                     export ALLOW_DESTRUCTIVE_MIGRATIONS=1 to confirm no old \
                     application version is running",
                    v
                );
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("migrate: {e}");
            1
        }
    }
}

async fn cli_migrate_down(steps: usize) -> i32 {
    let migrations = db::embedded_migrations();
    let pool = match db::migrator::connect(None).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("migrate-down: {e}");
            return 1;
        }
    };
    let result =
        db::migrator::with_advisory_lock(&pool, db::migrator::down(&pool, &migrations, steps))
            .await;
    match result {
        Ok(versions) => {
            for v in &versions {
                println!("migrate-down: reverted {:04}", v);
            }
            println!("migrate-down: {} migration(s) reverted", versions.len());
            0
        }
        Err(e) => {
            eprintln!("migrate-down: {e}");
            1
        }
    }
}

async fn cli_migrate_status() -> i32 {
    let migrations = db::embedded_migrations();
    let pool = match db::migrator::connect(None).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("migrate-status: {e}");
            return 1;
        }
    };
    // Read-only; the lock is not strictly needed but taking it keeps the
    // status output from racing an in-flight run.
    let result = db::migrator::with_advisory_lock(&pool, async {
        db::migrator::status(&pool, &migrations).await
    })
    .await;
    match result {
        Ok(state) => {
            println!("{:<6} {:<40} {:<10} PHASE", "VER", "NAME", "STATE");
            for (m, applied) in state {
                println!(
                    "{:<6} {:<40} {:<10} {}",
                    m.version,
                    m.name,
                    if applied { "applied" } else { "pending" },
                    if m.is_contract_phase {
                        "contract"
                    } else {
                        "expand"
                    }
                );
            }
            0
        }
        Err(e) => {
            eprintln!("migrate-status: {e}");
            1
        }
    }
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
        .with(fmt::layer())
        .init();
}
