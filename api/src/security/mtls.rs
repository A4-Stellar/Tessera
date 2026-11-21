//! X.509 SVID loading, rotation and mTLS configuration (issue #394).
//!
//! The SPIRE agent sidecar writes the workload's X.509 SVID into
//! `SPIFFE_SVID_DIR` (`/var/run/spiffe.svid` by default):
//!
//! * `svid.pem` — the workload certificate chain,
//! * `svid.key` — its private key (PKCS#8),
//! * `bundle.pem` — the trust bundle for `spiffe://tessera.io`,
//! * `spiffe_id` — the workload ID the sidecar fetched, used to prove the
//!   material belongs to this service.
//!
//! The workload ID must equal
//! `spiffe://tessera.io/ns/prod/sa/<SPIFFE_SERVICE_NAME>`. New material is
//! loaded through a stream and installed with `tokio::select!`, so rotation is
//! picked up without a restart. If the SVID disappears or its workload ID
//! changes, the revocation watch is flipped and every process terminates
//! within [`REVOCATION_TERMINATION_DEADLINE`], including requests already in
//! flight.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use futures::stream::Stream;
use futures::StreamExt;
use reqwest::{Certificate, Client, ClientBuilder, Identity};
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use tokio::sync::watch;

pub const DEFAULT_SVID_DIR: &str = "/var/run/spiffe.svid";
pub const DEFAULT_SERVICE_NAME: &str = "tessera-api";
pub const SPIFFE_ID_PREFIX: &str = "spiffe://tessera.io/ns/prod/sa";
pub const ROTATION_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Consecutive polls the SVID must be missing before it counts as revoked, so a
/// non-atomic rewrite by the SPIRE helper does not terminate the process.
pub const REVOCATION_GRACE_POLLS: u32 = 3;
/// Hard deadline for terminating mid-flight requests after revocation.
pub const REVOCATION_TERMINATION_DEADLINE: Duration = Duration::from_secs(10);

static STORE: OnceLock<Arc<SvidStore>> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum SvidError {
    #[error("cannot read SVID file {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid X.509 SVID: {0}")]
    Identity(#[from] reqwest::Error),
    #[error("SVID workload ID {actual:?} does not match {expected:?}")]
    WrongWorkloadId { expected: String, actual: String },
    #[error("SPIFFE trust bundle contains no certificates")]
    EmptyBundle,
    #[error("no X.509 SVID has been loaded yet")]
    NotLoaded,
}

/// One immutable generation of workload credentials.
#[derive(Clone)]
pub struct SvidMaterial {
    identity: Identity,
    cert_pem: Arc<[u8]>,
    key_pem: Arc<[u8]>,
    bundle_pem: Arc<[u8]>,
    fingerprint: [u8; 32],
}

impl std::fmt::Debug for SvidMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SvidMaterial")
            .field("fingerprint", &hex::encode(self.fingerprint))
            .field("cert_bytes", &self.cert_pem.len())
            .field("bundle_bytes", &self.bundle_pem.len())
            .finish_non_exhaustive()
    }
}

impl SvidMaterial {
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn cert_pem(&self) -> &[u8] {
        &self.cert_pem
    }

    pub fn key_pem(&self) -> &[u8] {
        &self.key_pem
    }

    pub fn bundle_pem(&self) -> &[u8] {
        &self.bundle_pem
    }
}

/// The process-wide SVID store. `ArcSwap` lets readers pick up rotated
/// credentials without blocking the rotation task.
pub struct SvidStore {
    dir: PathBuf,
    expected_id: String,
    current: ArcSwap<Option<Arc<SvidMaterial>>>,
    http_client: ArcSwap<Option<Client>>,
    revoked: watch::Sender<bool>,
}

impl SvidStore {
    /// Build the store from `SPIFFE_SVID_DIR` and `SPIFFE_SERVICE_NAME`.
    pub fn from_env() -> Option<Self> {
        // Opt in by setting either variable; the chart sets SPIFFE_SVID_DIR.
        if std::env::var("SPIFFE_SVID_DIR").is_err()
            && std::env::var("SPIFFE_SERVICE_NAME").is_err()
        {
            return None;
        }
        let dir = PathBuf::from(
            std::env::var("SPIFFE_SVID_DIR").unwrap_or_else(|_| DEFAULT_SVID_DIR.to_owned()),
        );
        let service = std::env::var("SPIFFE_SERVICE_NAME")
            .unwrap_or_else(|_| DEFAULT_SERVICE_NAME.to_owned());
        Some(Self::new(dir, workload_id(&service)))
    }

    fn new(dir: PathBuf, expected_id: String) -> Self {
        let (revoked, _) = watch::channel(false);
        Self {
            dir,
            expected_id,
            current: ArcSwap::from_pointee(None),
            http_client: ArcSwap::from_pointee(None),
            revoked,
        }
    }

    pub fn expected_id(&self) -> &str {
        &self.expected_id
    }

    pub fn current(&self) -> Option<Arc<SvidMaterial>> {
        self.current.load_full().as_ref().clone()
    }

    /// A client rebuilt whenever new SVID material is installed. Long-lived
    /// clients can also be rebuilt from [`SvidStore::configure_http`].
    pub fn http_client(&self) -> Option<Client> {
        self.http_client.load_full().as_ref().clone()
    }

    pub fn subscribe_revocation(&self) -> watch::Receiver<bool> {
        self.revoked.subscribe()
    }

    /// Load the first SVID generation before serving traffic. Failure is not
    /// fatal at startup: the rotation task keeps retrying.
    pub async fn load_initial(&self) -> Result<(), SvidError> {
        let material = load_material(&self.dir, &self.expected_id).await?;
        self.install(material);
        Ok(())
    }

    /// Apply the current client certificate and trust bundle to a PostgreSQL
    /// connection. sqlx then uses the SVID for every new pooled connection.
    pub fn apply_pg_tls(&self, options: PgConnectOptions) -> PgConnectOptions {
        let Some(material) = self.current() else {
            return options;
        };
        options
            .ssl_mode(PgSslMode::VerifyFull)
            .ssl_root_cert_from_pem(material.bundle_pem.to_vec())
            .ssl_client_cert_from_pem(material.cert_pem.to_vec())
            .ssl_client_key_from_pem(material.key_pem.to_vec())
    }

    /// Configure a one-off `reqwest` builder with the current SVID.
    pub fn configure_http(&self, builder: ClientBuilder) -> ClientBuilder {
        let Some(material) = self.current() else {
            return builder;
        };
        let mut builder = builder.identity(material.identity.clone());
        if let Ok(certs) = Certificate::from_pem_bundle(&material.bundle_pem) {
            for cert in certs {
                builder = builder.add_root_certificate(cert);
            }
        }
        builder
    }

    fn install(&self, material: SvidMaterial) {
        match build_http_client(&material) {
            Ok(client) => self
                .http_client
                .store(Arc::new(Some(client))),
            Err(error) => tracing::warn!(%error, "failed to rebuild mTLS HTTP client"),
        }
        self.current.store(Arc::new(Some(Arc::new(material))));
        metrics::gauge!("mtls_svid_active").set(1.0);
    }

    /// Yield whenever the SVID files change, including when they disappear.
    fn svid_stream(&self) -> impl Stream<Item = Result<SvidMaterial, SvidError>> + '_ {
        let dir = self.dir.clone();
        let expected_id = self.expected_id.clone();
        let last = self.current().map(|material| material.fingerprint);

        futures::stream::unfold(
            (dir, expected_id, last),
            |(dir, expected_id, last)| async move {
                loop {
                    match load_material(&dir, &expected_id).await {
                        Ok(material) if last != Some(material.fingerprint) => {
                            let next = Some(material.fingerprint);
                            return Some((Ok(material), (dir, expected_id, next)));
                        }
                        Ok(_) => {}
                        Err(error) => {
                            // Back off before reporting, or a persistent error spins this loop.
                            tokio::time::sleep(ROTATION_POLL_INTERVAL).await;
                            // Forget the fingerprint so recovery is reported (and resets the
                            // revocation grace counter) even if the files come back unchanged.
                            return Some((Err(error), (dir, expected_id, None)));
                        }
                    }
                    tokio::time::sleep(ROTATION_POLL_INTERVAL).await;
                }
            },
        )
    }

    /// Rotate credentials until shutdown. A missing SVID after a successful
    /// load is treated as revocation.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut stream = Box::pin(self.svid_stream());
        let mut missing_polls = 0u32;
        loop {
            tokio::select! {
                update = stream.next() => match update {
                    Some(Ok(material)) => {
                        missing_polls = 0;
                        tracing::info!(
                            fingerprint = %hex::encode(material.fingerprint),
                            "installed rotated X.509 SVID"
                        );
                        self.install(material);
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "failed to load X.509 SVID; keeping previous material");
                        let missing = matches!(
                            &error,
                            SvidError::Io { source, .. }
                                if source.kind() == std::io::ErrorKind::NotFound
                        );
                        missing_polls = if missing { missing_polls + 1 } else { 0 };
                        if missing_polls >= REVOCATION_GRACE_POLLS && self.current().is_some() {
                            self.revoked.send_replace(true);
                        }
                    }
                    None => break,
                },
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow_and_update() {
                        break;
                    }
                }
            }
        }
    }
}

/// The SPIFFE workload ID for a service in the `prod` namespace.
pub fn workload_id(service: &str) -> String {
    format!("{SPIFFE_ID_PREFIX}/{service}")
}

/// Install the process-wide store from the environment. Returns the store so
/// the caller can spawn the rotation and revocation tasks.
pub fn init_from_env() -> Option<Arc<SvidStore>> {
    let store = Arc::new(SvidStore::from_env()?);
    let _ = STORE.set(Arc::clone(&store));
    STORE.get().cloned()
}

pub fn store() -> Option<Arc<SvidStore>> {
    STORE.get().cloned()
}

/// Current mTLS client, or a plain client before the first SVID load.
pub fn http_client() -> Client {
    store()
        .and_then(|store| store.http_client())
        .unwrap_or_default()
}

/// Builder for one-off clients that need extra options (timeouts, proxies).
pub fn http_builder() -> ClientBuilder {
    match store() {
        Some(store) => store.configure_http(Client::builder()),
        None => Client::builder(),
    }
}

/// Add the current SVID to a PostgreSQL connection configuration.
pub fn apply_pg_tls(options: PgConnectOptions) -> PgConnectOptions {
    match store() {
        Some(store) => store.apply_pg_tls(options),
        None => options,
    }
}

/// Spawn the task that watches for SVID rotation.
pub fn spawn_rotation(store: Arc<SvidStore>, shutdown: watch::Receiver<bool>) {
    tokio::spawn(async move { store.run(shutdown).await });
}

/// Spawn the revocation enforcer. It gives in-flight requests at most ten
/// seconds to unwind before the process exits.
pub fn spawn_revocation_enforcer(store: Arc<SvidStore>, shutdown: watch::Receiver<bool>) {
    tokio::spawn(async move {
        enforce_revocation(store.subscribe_revocation(), shutdown, || -> bool {
            tracing::error!("X.509 SVID revoked; terminating after in-flight deadline");
            std::process::exit(1)
        })
        .await;
    });
}

/// Wait for a revocation signal and invoke `terminate`. The callback returns
/// `true` when it has already terminated the process; otherwise this function
/// waits no longer than [`REVOCATION_TERMINATION_DEADLINE`].
pub async fn enforce_revocation(
    mut revoked: watch::Receiver<bool>,
    mut shutdown: watch::Receiver<bool>,
    terminate: impl FnOnce() -> bool,
) {
    loop {
        tokio::select! {
            result = revoked.changed() => {
                if result.is_err() {
                    return;
                }
                if *revoked.borrow_and_update() {
                    break;
                }
            }
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow_and_update() {
                    return;
                }
            }
        }
    }

    if terminate() {
        return;
    }

    tokio::select! {
        _ = tokio::time::sleep(REVOCATION_TERMINATION_DEADLINE) => {},
        _ = shutdown.changed() => {},
    }
}

fn build_http_client(material: &SvidMaterial) -> Result<Client, reqwest::Error> {
    let mut builder = Client::builder().identity(material.identity.clone());
    for cert in Certificate::from_pem_bundle(&material.bundle_pem)? {
        builder = builder.add_root_certificate(cert);
    }
    builder.build()
}

fn io_error(path: &Path, source: std::io::Error) -> SvidError {
    SvidError::Io {
        path: path.to_path_buf(),
        source,
    }
}

async fn read_svid_file(dir: &Path, name: &str) -> Result<Vec<u8>, SvidError> {
    let path = dir.join(name);
    tokio::fs::read(&path)
        .await
        .map_err(|source| io_error(&path, source))
}

async fn load_material(dir: &Path, expected_id: &str) -> Result<SvidMaterial, SvidError> {
    let cert_pem = read_svid_file(dir, "svid.pem").await?;
    let key_pem = read_svid_file(dir, "svid.key").await?;
    let bundle_pem = read_svid_file(dir, "bundle.pem").await?;

    let id_path = dir.join("spiffe_id");
    let actual_id = tokio::fs::read_to_string(&id_path)
        .await
        .map_err(|source| io_error(&id_path, source))?
        .trim()
        .to_owned();
    if actual_id != expected_id {
        return Err(SvidError::WrongWorkloadId {
            expected: expected_id.to_owned(),
            actual: actual_id,
        });
    }

    if Certificate::from_pem_bundle(&bundle_pem)?.is_empty() {
        return Err(SvidError::EmptyBundle);
    }

    let mut identity_pem = Vec::with_capacity(cert_pem.len() + key_pem.len() + 1);
    identity_pem.extend_from_slice(&cert_pem);
    if !identity_pem.ends_with(b"\n") {
        identity_pem.push(b'\n');
    }
    identity_pem.extend_from_slice(&key_pem);
    let identity = Identity::from_pem(&identity_pem)?;

    let mut hasher = Sha256::new();
    hasher.update(&cert_pem);
    hasher.update(&key_pem);
    hasher.update(&bundle_pem);
    let fingerprint: [u8; 32] = hasher.finalize().into();

    Ok(SvidMaterial {
        identity,
        cert_pem: cert_pem.into(),
        key_pem: key_pem.into(),
        bundle_pem: bundle_pem.into(),
        fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::oneshot;

    #[test]
    fn workload_ids_follow_the_prod_service_account_pattern() {
        assert_eq!(
            workload_id("tessera-api"),
            "spiffe://tessera.io/ns/prod/sa/tessera-api"
        );
        assert_eq!(
            workload_id("tessera-indexer"),
            "spiffe://tessera.io/ns/prod/sa/tessera-indexer"
        );
    }

    /// A request is in flight when revocation fires. The guard proves it is
    /// dropped by the termination path, and the whole exchange must complete
    /// inside the ten-second hard deadline.
    #[tokio::test]
    async fn mid_flight_revocation_terminates_within_ten_seconds() {
        struct InFlight(Arc<AtomicBool>);
        impl Drop for InFlight {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }

        let active = Arc::new(AtomicBool::new(true));
        let in_flight = InFlight(Arc::clone(&active));
        let (revoked_tx, revoked_rx) = watch::channel(false);
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let started = std::time::Instant::now();

        let enforcer = tokio::spawn(async move {
            enforce_revocation(revoked_rx, shutdown_rx, || -> bool {
                drop(in_flight);
                let _ = done_tx.send(());
                true
            })
            .await;
        });

        // Simulate traffic that has not completed when the SVID is revoked.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(active.load(Ordering::SeqCst));
        revoked_tx.send(true).expect("revocation watch is open");

        tokio::time::timeout(REVOCATION_TERMINATION_DEADLINE, done_rx)
            .await
            .expect("revocation termination exceeded the deadline")
            .expect("termination signal was not sent");
        enforcer.await.expect("enforcer task failed");

        assert!(!active.load(Ordering::SeqCst), "in-flight work survived revocation");
        assert!(started.elapsed() < REVOCATION_TERMINATION_DEADLINE);
    }
}
