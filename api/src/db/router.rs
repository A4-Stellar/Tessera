//! Read/write routing across a PostgreSQL primary and its streaming read
//! replicas (issue #95).
//!
//! * [`DbRouter::route`] sends statements a hot standby can serve to a read
//!   replica, round-robin, and everything else to the primary.
//! * A background monitor probes each replica every [`PROBE_INTERVAL`] and
//!   takes it out of rotation while it is unreachable or more than
//!   [`MAX_REPLICATION_LAG`] behind the primary. With no replica in rotation,
//!   reads fall back to the primary.
//! * Each probe exports `db_replica_up`, `db_replica_lag_seconds` and
//!   `db_replica_probe_duration_seconds`, labelled by `replica` (`host:port`).
//!
//! Routing is O(1) while replicas are healthy, O(r) in the worst case for
//! r replicas, and takes no locks or allocations: health is one `AtomicBool`
//! per replica, written only by that replica's monitor task.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use tokio::sync::watch;

/// Replicas further behind the primary than this serve no reads.
pub const MAX_REPLICATION_LAG: Duration = Duration::from_secs(5);
const PROBE_INTERVAL: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Replication lag in seconds, as seen by the replica.
///
/// Once the replica has replayed everything it has received, it is caught up,
/// even if the primary has been idle and the last replayed commit is old.
/// Otherwise the lag is the age of the last replayed commit. A node that is
/// not in recovery reports 0. `NULL` (nothing replayed yet) is treated as
/// lagging.
const LAG_QUERY: &str = "SELECT CASE \
     WHEN NOT pg_is_in_recovery() THEN 0 \
     WHEN pg_last_wal_receive_lsn() = pg_last_wal_replay_lsn() THEN 0 \
     ELSE EXTRACT(EPOCH FROM now() - pg_last_xact_replay_timestamp()) \
     END::float8";

#[derive(Debug, thiserror::Error)]
pub enum DbConfigError {
    #[error("invalid database URL in {0}: {1}")]
    Url(&'static str, sqlx::Error),
    #[error("database URL in {0} must start with postgres:// or postgresql://")]
    Scheme(&'static str),
    #[error("RWA_DATABASE_REPLICA_URLS requires RWA_DATABASE_URL")]
    PrimaryRequired,
}

struct Replica {
    /// `host:port`, the metrics label. Never includes credentials.
    name: String,
    pool: PgPool,
    in_rotation: AtomicBool,
}

pub struct DbRouter {
    primary: PgPool,
    replicas: Vec<Replica>,
    next_replica: AtomicUsize,
}

impl DbRouter {
    /// Build the router from `RWA_DATABASE_URL` (primary) and the optional
    /// comma-separated `RWA_DATABASE_REPLICA_URLS`. Returns `None` when no
    /// database is configured. Pools connect lazily, so this does no I/O.
    pub fn from_env() -> Result<Option<Self>, DbConfigError> {
        let replica_urls = std::env::var("RWA_DATABASE_REPLICA_URLS").ok();
        let Ok(primary_url) = std::env::var("RWA_DATABASE_URL") else {
            return match replica_urls {
                Some(_) => Err(DbConfigError::PrimaryRequired),
                None => Ok(None),
            };
        };

        let primary = parse_url("RWA_DATABASE_URL", &primary_url)?;
        let replicas = replica_urls
            .iter()
            .flat_map(|urls| urls.split(','))
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(|url| parse_url("RWA_DATABASE_REPLICA_URLS", url))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Self::new(primary, replicas)))
    }

    fn new(primary: PgConnectOptions, replicas: Vec<PgConnectOptions>) -> Self {
        let replicas = replicas
            .into_iter()
            .map(|options| {
                let name = format!("{}:{}", options.get_host(), options.get_port());
                metrics::gauge!("db_replica_up", "replica" => name.clone()).set(0.0);
                Replica {
                    name,
                    pool: PgPoolOptions::new().connect_lazy_with(options),
                    // Out of rotation until the first probe proves it fresh.
                    in_rotation: AtomicBool::new(false),
                }
            })
            .collect();
        DbRouter {
            primary: PgPoolOptions::new().connect_lazy_with(primary),
            replicas,
            next_replica: AtomicUsize::new(0),
        }
    }

    /// The pool for `sql`: a read replica when a hot standby can serve the
    /// statement, else the primary.
    ///
    /// Only a `SELECT` without `INTO`, a row-locking clause (`FOR UPDATE` /
    /// `FOR SHARE` …) or a sequence call (`nextval` / `setval`) is routed
    /// to a replica, and only as a single statement; `INSERT`, `UPDATE`,
    /// `DELETE`, DDL and anything else go to the primary. A `SELECT` of a
    /// function a standby cannot run (one that writes, `pg_notify`,
    /// `txid_current` …) cannot be told apart from a read. Send it, and
    /// every transaction, to [`DbRouter::primary`] directly.
    pub fn route(&self, sql: &str) -> &PgPool {
        if is_replica_safe(sql) {
            self.replica()
        } else {
            &self.primary
        }
    }

    /// The primary pool, for writes and transactions.
    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    /// The next in-rotation replica, round-robin; the primary when none is.
    fn replica(&self) -> &PgPool {
        let count = self.replicas.len();
        if count == 0 {
            return &self.primary;
        }
        let start = self.next_replica.fetch_add(1, Ordering::Relaxed);
        (0..count)
            .map(|offset| &self.replicas[start.wrapping_add(offset) % count])
            .find(|replica| replica.in_rotation.load(Ordering::Relaxed))
            .map_or(&self.primary, |replica| &replica.pool)
    }

    /// Spawn one probe task per replica. The tasks stop when `shutdown`
    /// flips to `true`.
    pub fn spawn_health_monitor(self: &Arc<Self>, shutdown: watch::Receiver<bool>) {
        for index in 0..self.replicas.len() {
            let router = Arc::clone(self);
            let mut shutdown = shutdown.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(PROBE_INTERVAL);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = ticker.tick() => router.replicas[index].probe().await,
                        _ = shutdown.changed() => {
                            if *shutdown.borrow() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    }
}

impl Replica {
    async fn probe(&self) {
        let started = Instant::now();
        let outcome = tokio::time::timeout(
            PROBE_TIMEOUT,
            sqlx::query_scalar::<_, Option<f64>>(LAG_QUERY).fetch_one(&self.pool),
        )
        .await;
        let replica = self.name.clone();

        let lag = match outcome {
            Ok(Ok(lag)) => {
                metrics::gauge!("db_replica_up", "replica" => replica.clone()).set(1.0);
                metrics::histogram!("db_replica_probe_duration_seconds", "replica" => replica.clone())
                    .record(started.elapsed().as_secs_f64());
                if let Some(lag) = lag {
                    metrics::gauge!("db_replica_lag_seconds", "replica" => replica).set(lag);
                }
                lag
            }
            Ok(Err(error)) => {
                metrics::gauge!("db_replica_up", "replica" => replica).set(0.0);
                tracing::debug!(replica = %self.name, %error, "replica probe failed");
                None
            }
            Err(_) => {
                metrics::gauge!("db_replica_up", "replica" => replica).set(0.0);
                tracing::debug!(replica = %self.name, "replica probe timed out");
                None
            }
        };

        let routable = lag.is_some_and(|lag| lag <= MAX_REPLICATION_LAG.as_secs_f64());
        if self.in_rotation.swap(routable, Ordering::Relaxed) != routable {
            if routable {
                tracing::info!(replica = %self.name, "replica in read rotation");
            } else {
                tracing::warn!(replica = %self.name, ?lag, "replica out of read rotation; reads fall back");
            }
        }
    }
}

/// Parse a libpq connection URI. sqlx ignores the scheme, so check it here
/// rather than connect to a MySQL or Redis URL as if it were PostgreSQL.
fn parse_url(var: &'static str, url: &str) -> Result<PgConnectOptions, DbConfigError> {
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
        return Err(DbConfigError::Scheme(var));
    }
    PgConnectOptions::from_str(url).map_err(|e| DbConfigError::Url(var, e))
}

/// Whether a hot standby can run `sql` (see [`DbRouter::route`]). A single
/// allocation-free pass over the statement's words; a word match inside a
/// string literal only ever sends a read to the primary.
fn is_replica_safe(sql: &str) -> bool {
    // Later statements (`SELECT 1; DELETE …`) run on the same connection.
    if sql.trim_end().trim_end_matches(';').contains(';') {
        return false;
    }
    let mut words = sql
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty());
    if !words
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("select"))
    {
        return false;
    }
    let mut after_for = false;
    for word in words {
        let is = |keyword: &str| word.eq_ignore_ascii_case(keyword);
        if is("into") || is("nextval") || is("setval") {
            return false;
        }
        // FOR UPDATE | FOR NO KEY UPDATE | FOR SHARE | FOR KEY SHARE
        if after_for && (is("update") || is("share") || is("no") || is("key")) {
            return false;
        }
        after_for = is("for");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_plain_selects_to_replicas() {
        for sql in [
            "SELECT * FROM events WHERE asset_id = $1",
            "  select count(*) from holders",
            "(SELECT id FROM assets) UNION (SELECT id FROM archived_assets)",
            "SELECT name FROM assets WHERE platform = 'for'",
            "SELECT 1;",
        ] {
            assert!(is_replica_safe(sql), "{sql}");
        }
    }

    #[test]
    fn routes_writes_and_standby_unsafe_selects_to_primary() {
        for sql in [
            "INSERT INTO events (id) VALUES ($1)",
            "UPDATE assets SET valuation = $1 WHERE id = $2",
            "DELETE FROM holders WHERE asset_id = $1",
            "WITH moved AS (DELETE FROM a RETURNING *) SELECT * FROM moved",
            "CREATE TABLE t (id int)",
            "SELECT * FROM assets WHERE id = $1 FOR UPDATE",
            "select * from assets for no key update",
            "SELECT * FROM assets FOR SHARE SKIP LOCKED",
            "SELECT * FROM assets FOR KEY SHARE",
            "SELECT * INTO backup FROM assets",
            "SELECT nextval('events_id_seq')",
            "SELECT 1; DELETE FROM events",
            "",
        ] {
            assert!(!is_replica_safe(sql), "{sql}");
        }
    }

    #[test]
    fn rejects_non_postgres_urls() {
        assert!(parse_url("V", "postgresql://u@db:5433/rwa").is_ok());
        for url in ["mysql://u@db/rwa", "db:5432", ""] {
            assert!(matches!(
                parse_url("V", url),
                Err(DbConfigError::Scheme("V"))
            ));
        }
    }

    fn router(replicas: usize) -> DbRouter {
        let options = |port| PgConnectOptions::new().host("127.0.0.1").port(port);
        DbRouter::new(
            options(5432),
            (0..replicas as u16).map(|i| options(5433 + i)).collect(),
        )
    }

    fn served_by(router: &DbRouter, pool: &PgPool) -> Option<usize> {
        router
            .replicas
            .iter()
            .position(|replica| std::ptr::eq(&replica.pool, pool))
    }

    #[tokio::test]
    async fn round_robins_across_in_rotation_replicas() {
        let router = router(3);
        router
            .replicas
            .iter()
            .for_each(|r| r.in_rotation.store(true, Ordering::Relaxed));

        let served: Vec<_> = (0..6)
            .map(|_| served_by(&router, router.route("SELECT 1")))
            .collect();
        assert_eq!(
            served,
            [Some(0), Some(1), Some(2), Some(0), Some(1), Some(2)]
        );

        // Writes never touch a replica.
        assert!(std::ptr::eq(
            router.route("INSERT INTO t VALUES (1)"),
            router.primary()
        ));
    }

    #[tokio::test]
    async fn skips_replicas_out_of_rotation() {
        let router = router(3);
        router.replicas[1]
            .in_rotation
            .store(true, Ordering::Relaxed);
        for _ in 0..4 {
            assert_eq!(served_by(&router, router.route("SELECT 1")), Some(1));
        }
    }

    #[tokio::test]
    async fn falls_back_to_primary_without_a_routable_replica() {
        for router in [router(0), router(2)] {
            assert!(std::ptr::eq(router.route("SELECT 1"), router.primary()));
        }
    }

    #[tokio::test]
    async fn unreachable_replica_leaves_rotation_and_reports_down() {
        // Nothing listens on port 1.
        let options = PgConnectOptions::new().host("127.0.0.1").port(1);
        let router = DbRouter::new(options.clone(), vec![options]);
        router.replicas[0]
            .in_rotation
            .store(true, Ordering::Relaxed);

        router.replicas[0].probe().await;

        assert!(!router.replicas[0].in_rotation.load(Ordering::Relaxed));
        assert!(std::ptr::eq(router.route("SELECT 1"), router.primary()));
    }
}
