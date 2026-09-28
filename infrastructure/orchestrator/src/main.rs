use std::sync::Arc;

use tessera_orchestrator::config::{DnsConfig, OrchestratorConfig};
use tessera_orchestrator::dns::{AwsCredentials, CloudflareDns, Route53Dns};
use tessera_orchestrator::postgres::PgCluster;
use tessera_orchestrator::{DnsUpdater, FailoverController, Topology};
use tracing::info;

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("environment variable {name} is not set"))
}

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tessera_orchestrator=info".into()),
        )
        .json()
        .init();

    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("ORCHESTRATOR_CONFIG").ok())
        .unwrap_or_else(|| "orchestrator.toml".into());
    let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
    let config = OrchestratorConfig::from_toml(&text)?;
    let policy = config.to_policy_checked()?;

    let mut dsns = Vec::new();
    for node in std::iter::once(&config.primary).chain(&config.replicas) {
        dsns.push((node.name.clone(), env(&node.dsn_env)?));
    }
    let cluster =
        Arc::new(PgCluster::connect_lazy(dsns, policy.probe_timeout).map_err(|e| e.to_string())?);

    let dns: Arc<dyn DnsUpdater> = match &config.dns {
        DnsConfig::Route53 {
            hosted_zone_id,
            record_name,
            ttl,
        } => Arc::new(Route53Dns::new(
            hosted_zone_id,
            record_name.clone(),
            *ttl,
            AwsCredentials {
                access_key_id: env("AWS_ACCESS_KEY_ID")?,
                secret_access_key: env("AWS_SECRET_ACCESS_KEY")?,
                session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
            },
        )),
        DnsConfig::Cloudflare {
            zone_id,
            record_id,
            record_name,
            ttl,
        } => Arc::new(CloudflareDns::new(
            zone_id.clone(),
            record_id.clone(),
            record_name.clone(),
            env("CLOUDFLARE_API_TOKEN")?,
            *ttl,
        )),
    };

    let topology = Topology {
        primary: config.primary.node(),
        replicas: config.replicas.iter().map(|r| r.node()).collect(),
        fenced: vec![],
    };
    info!(primary = %topology.primary.name, replicas = topology.replicas.len(), "starting failover controller");

    let controller = FailoverController::new(topology, cluster.clone(), cluster, dns, policy);
    let final_topology = controller
        .run(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    info!(primary = %final_topology.primary.name, "shutting down");
    Ok(())
}
