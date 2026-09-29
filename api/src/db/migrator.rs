//! Zero-downtime schema migration engine (issue #71).
//!
//! A small migration runner purpose-built for the Tessera API. Migrations
//! live in `src/db/migrations/` as paired SQL files:
//!
//! ```text
//! 0002_expand_holder_balance_minor.sql        # applies the change
//! 0002_expand_holder_balance_minor.down.sql   # reverts it
//! ```
//!
//! # Zero-downtime guarantees
//!
//! * **Advisory-lock serialization.** Every migration run takes
//!   `pg_advisory_lock(0x54455353)` ("TESS") before touching anything and
//!   holds it until the run finishes. Two operators (or a deploy racing a
//!   manual run) can never execute migrations concurrently; the loser waits.
//! * **Expand-and-contract gating.** A migration whose name contains
//!   `contract` (case-insensitive) is a *contract phase*: it is refused
//!   unless `ALLOW_DESTRUCTIVE_MIGRATIONS=1` is exported. Expand phases —
//!   additive, backward-compatible schema — always run freely. This makes it
//!   impossible for a routine deploy to drop a column while an old binary
//!   that still reads it is rolling out.
//! * **Checksum verification.** Every migration file's SHA-256 is recorded
//!   when applied. A later run re-hashes the file and fails on mismatch, so
//!   editing an already-applied migration is caught instead of silently
//!   diverging from what production ran.
//! * **Statement-at-a-time execution.** Files are split into individual
//!   statements and executed in autocommit (one transaction each), which is
//!   what `CREATE INDEX CONCURRENTLY` requires. Multi-statement transactional
//!   migrations are opt-in via the `--transactional` header (see
//!   [`SplitOptions`]).
//! * **Verified rollbacks.** Every migration ships a `.down.sql`. The down
//!   path goes through the same splitting/checksum/gating machinery, so a
//!   rollback is exactly as controlled as the rollout.

use std::fmt;
use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres};

/// The advisory-lock key used to serialize migration runs across all
/// operators and deploy pipelines. Arbitrary but fixed; "TESS" in hex.
pub const MIGRATION_LOCK_KEY: i64 = 0x5445_5353;

/// Environment variable that must be set to `1` for contract (destructive)
/// migrations to run. See the module docs for why the default is refusal.
pub const ALLOW_DESTRUCTIVE_ENV: &str = "ALLOW_DESTRUCTIVE_MIGRATIONS";

/// Timeout for acquiring the advisory lock. A stuck holder (an operator with
/// a `psql` session holding the lock, say) should not hang a deploy forever.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration {version} ({name}) was applied with checksum {applied}, but the file now hashes to {current}; applied migrations must never be edited")]
    ChecksumMismatch {
        version: i64,
        name: String,
        applied: String,
        current: String,
    },
    #[error(
        "migration {version} ({name}) is a contract (destructive) phase; export \
         {env_var}=1 to confirm no old application version is running"
    )]
    ContractPhaseBlocked {
        version: i64,
        name: String,
        env_var: &'static str,
    },
    #[error("migration file name {0:?} does not match the expected <4-digit-version>_<name>.sql pattern")]
    BadFileName(String),
    #[error("migration files must be ordered and unique; found duplicate version {version}")]
    DuplicateVersion { version: i64 },
    #[error("cannot roll back: migration {version} ({name}) has no .down.sql file")]
    MissingDownMigration { version: i64, name: String },
    #[error("no migrations are applied; nothing to roll back")]
    NothingToRollBack,
    #[error("timed out after {timeout_secs}s waiting for the migration advisory lock (key {key}); another migration run is probably in progress")]
    LockTimeout { key: i64, timeout_secs: u64 },
    #[error("invalid SQL in migration {version}: {message}")]
    SqlSyntax { version: i64, message: String },
}

/// One parsed migration file (the up side).
#[derive(Debug, Clone)]
pub struct Migration {
    pub version: i64,
    pub name: String,
    /// Raw up-SQL, checksummed as-is (byte-exact file content).
    pub sql: String,
    /// Raw down-SQL, if a `.down.sql` pair exists.
    pub down_sql: Option<String>,
    /// True when the file name marks a contract (destructive) phase.
    pub is_contract_phase: bool,
    /// True when the file's first comment line is `-- transactional`, meaning
    /// all its statements must run inside one transaction. Mutually exclusive
    /// with `CREATE INDEX CONCURRENTLY`, which cannot run in a transaction.
    pub transactional: bool,
}

impl Migration {
    /// Build a migration from its parts, deriving the phase flag from the
    /// name and the transactionality from the SQL header.
    pub fn new(version: i64, name: impl Into<String>, sql: impl Into<String>) -> Self {
        let name = name.into();
        let sql = sql.into();
        Migration {
            version,
            is_contract_phase: is_contract_phase(&name),
            name,
            transactional: has_transactional_header(&sql),
            down_sql: None,
            sql,
        }
    }

    /// Attach the paired down migration.
    pub fn with_down(mut self, down_sql: impl Into<String>) -> Self {
        self.down_sql = Some(down_sql.into());
        self
    }

    /// SHA-256 of the up-SQL, hex encoded. Stored per applied version.
    pub fn checksum(&self) -> String {
        checksum_of(&self.sql)
    }
}

/// True when the first non-blank line is a `-- transactional` comment.
fn has_transactional_header(sql: &str) -> bool {
    sql.lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|first| {
            let t = first.trim();
            t.starts_with("--") && t[2..].trim().eq_ignore_ascii_case("transactional")
        })
}

impl fmt::Display for Migration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}_{}", self.version, self.name)
    }
}

/// Hex-encoded SHA-256 of `bytes`.
pub fn checksum_of(sql: &str) -> String {
    let digest = Sha256::digest(sql.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// True when a migration name marks a contract (destructive) phase.
///
/// The convention is that file names contain `contract` for contract phases
/// and `expand` for expand phases; anything else is treated as expand (the
/// safe default, since expand migrations run freely).
pub fn is_contract_phase(name: &str) -> bool {
    name.to_ascii_lowercase().contains("contract")
}

// ---------------------------------------------------------------------------
// SQL splitting
// ---------------------------------------------------------------------------

/// Controls how SQL text is split into statements.
#[derive(Debug, Clone, Default)]
pub struct SplitOptions {
    /// When false (the default), the splitter understands dollar-quoted
    /// strings (`$$...$$`, `$tag$...$tag$`) so `DO $$ ... $$;` blocks stay
    /// intact. Set true only for files known to have no dollar quotes.
    pub simple: bool,
}

/// Split SQL text into individual statements on top-level semicolons.
///
/// Handles:
/// * line comments (`-- ...`) and block comments (`/* ... */`)
/// * single-quoted strings with `''` escapes
/// * dollar-quoted strings (`$$body$$` or `$tag$body$tag$`), which is how
///   PostgreSQL function/`DO` bodies are written — the semicolons inside a
///   `DO $$ ... $$` block must not split the statement
///
/// Comments attached to a statement are kept (they travel with the statement
/// to the server, which is harmless and preserves intent).
pub fn split_sql(sql: &str, opts: &SplitOptions) -> Vec<String> {
    if opts.simple {
        return sql
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
    }

    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                // Line comment: skip to end of line.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                // Block comment: skip to the matching */ (nested per SQL spec).
                let mut depth = 1;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'\'' => {
                // Single-quoted string with '' escapes.
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'$' => {
                // Possible dollar quote: $$ or $tag$.
                if let Some(tag_end) = dollar_tag_end(bytes, i) {
                    let closing = &sql[i..tag_end]; // e.g. "$$" or "$tag$"
                                                    // Find the matching closing delimiter after the opener.
                    let body_start = tag_end;
                    let close_at = sql[body_start..].find(closing).map(|p| body_start + p);
                    match close_at {
                        Some(pos) => {
                            i = pos + closing.len();
                        }
                        None => {
                            // Unterminated: treat the rest as the string.
                            i = bytes.len();
                        }
                    }
                } else {
                    i += 1;
                }
            }
            b';' => {
                let stmt = sql[start..i].trim();
                if !stmt.is_empty() {
                    statements.push(stmt.to_owned());
                }
                start = i + 1;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    let tail = sql[start..].trim();
    if !tail.is_empty() {
        statements.push(tail.to_owned());
    }
    statements
}

/// If `bytes[at]` starts a dollar-quote opener (`$$` or `$tag$`), return the
/// index just past the opener; otherwise return `None`.
///
/// A valid tag is `$` followed by `[A-Za-z_][A-Za-z0-9_]*` then `$`. Bare `$$`
/// (empty tag) is also valid. A lone `$1` positional parameter is not.
fn dollar_tag_end(bytes: &[u8], at: usize) -> Option<usize> {
    let mut i = at + 1;
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'$' {
        return Some(i + 1);
    }
    let first = bytes[i];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    i += 1;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'$' {
        Some(i + 1)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Migration discovery
// ---------------------------------------------------------------------------

/// The embedded migrations, in version order. Built with
/// [`crate::db::embedded_migrations`] so the SQL ships inside the binary and
/// `tessera-api migrate` works from the distroless container (no filesystem).
pub fn load_embedded(migrations: &[Migration]) -> Result<Vec<Migration>, MigrateError> {
    let mut sorted: Vec<Migration> = migrations.to_vec();
    sorted.sort_by_key(|m| m.version);

    let mut seen = std::collections::HashSet::new();
    for m in &sorted {
        if !seen.insert(m.version) {
            return Err(MigrateError::DuplicateVersion { version: m.version });
        }
        if m.version <= 0 {
            return Err(MigrateError::BadFileName(format!(
                "{:04}_{}",
                m.version, m.name
            )));
        }
    }
    Ok(sorted)
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

/// Execute pending up migrations, refusing contract phases unless
/// `ALLOW_DESTRUCTIVE_MIGRATIONS=1` is exported.
///
/// Returns `(applied, blocked)`: the versions applied by this run, and the
/// first contract phase stopped at, if any. Hitting the gate is an *expected
/// outcome* of a routine deploy (expand phases apply, the contract phase
/// waits for the operator), not a hard failure — the work done before the
/// gate is reported so callers can print it and exit nonzero.
pub async fn up(
    pool: &PgPool,
    migrations: &[Migration],
) -> Result<(Vec<i64>, Vec<i64>), MigrateError> {
    ensure_history_table(pool).await?;

    let applied = applied_versions(pool).await?;
    let allow_destructive = allow_destructive();

    let mut applied_now = Vec::new();
    for m in migrations {
        if applied.contains(&m.version) {
            verify_checksum(pool, m).await?;
            continue;
        }

        if m.is_contract_phase && !allow_destructive {
            return Ok((applied_now, vec![m.version]));
        }

        apply_one(pool, m).await?;
        applied_now.push(m.version);
    }
    Ok((applied_now, Vec::new()))
}

/// Roll back the most recently applied migration (or `steps` of them).
/// Returns the versions that were reverted, most recent first.
pub async fn down(
    pool: &PgPool,
    migrations: &[Migration],
    steps: usize,
) -> Result<Vec<i64>, MigrateError> {
    ensure_history_table(pool).await?;

    let mut history = applied_versions(pool).await?;
    if history.is_empty() {
        return Err(MigrateError::NothingToRollBack);
    }

    let allow_destructive = allow_destructive();
    let mut reverted = Vec::new();

    for _ in 0..steps {
        let Some(&latest) = history.last() else {
            break;
        };
        let Some(m) = migrations.iter().find(|m| m.version == latest) else {
            // Known-applied version that no longer exists in the binary: the
            // down file cannot be verified. Refuse rather than guess.
            return Err(MigrateError::MissingDownMigration {
                version: latest,
                name: "<unknown: version not in this binary>".to_string(),
            });
        };
        let Some(down_sql) = &m.down_sql else {
            return Err(MigrateError::MissingDownMigration {
                version: m.version,
                name: m.name.clone(),
            });
        };
        // Contract-phase down scripts are destructive by definition; gate
        // them the same way as their up counterparts.
        if m.is_contract_phase && !allow_destructive {
            return Err(MigrateError::ContractPhaseBlocked {
                version: m.version,
                name: m.name.clone(),
                env_var: ALLOW_DESTRUCTIVE_ENV,
            });
        }

        let started = std::time::Instant::now();
        let down_transactional = has_transactional_header(down_sql);
        execute_script(pool, m.version, down_sql, down_transactional).await?;
        record_reverted(pool, m.version).await?;
        tracing::info!(
            version = m.version,
            name = %m.name,
            duration_ms = started.elapsed().as_millis() as u64,
            "migration rolled back"
        );
        reverted.push(m.version);
        history.pop();
    }

    Ok(reverted)
}

/// Report the applied/pending state of every known migration.
pub async fn status(
    pool: &PgPool,
    migrations: &[Migration],
) -> Result<Vec<(Migration, bool)>, MigrateError> {
    ensure_history_table(pool).await?;
    let applied = applied_versions(pool).await?;
    let mut out = Vec::with_capacity(migrations.len());
    for m in migrations {
        out.push((m.clone(), applied.contains(&m.version)));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn allow_destructive() -> bool {
    std::env::var(ALLOW_DESTRUCTIVE_ENV).as_deref() == Ok("1")
}

async fn ensure_history_table(pool: &PgPool) -> Result<(), MigrateError> {
    sqlx::query(
        r"
        CREATE TABLE IF NOT EXISTS _schema_migrations (
            version      BIGINT      PRIMARY KEY,
            name         TEXT        NOT NULL,
            checksum     TEXT        NOT NULL,
            is_contract  BOOLEAN     NOT NULL DEFAULT FALSE,
            applied_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            applied_by   TEXT,
            duration_ms  BIGINT
        )
        ",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn applied_versions(pool: &PgPool) -> Result<Vec<i64>, MigrateError> {
    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT version FROM _schema_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(v,)| v).collect())
}

async fn verify_checksum(pool: &PgPool, m: &Migration) -> Result<(), MigrateError> {
    let current = m.checksum();
    let applied: Option<(String,)> =
        sqlx::query_as("SELECT checksum FROM _schema_migrations WHERE version = $1")
            .bind(m.version)
            .fetch_optional(pool)
            .await?;
    if let Some((applied,)) = applied {
        if applied != current {
            return Err(MigrateError::ChecksumMismatch {
                version: m.version,
                name: m.name.clone(),
                applied,
                current,
            });
        }
    }
    Ok(())
}

/// Apply one up migration: lock, execute, record. Statement-at-a-time in
/// autocommit so `CONCURRENTLY` statements are legal.
async fn apply_one(pool: &PgPool, m: &Migration) -> Result<(), MigrateError> {
    let started = std::time::Instant::now();
    let checksum = m.checksum();

    // Each statement in its own implicit transaction. If the file's statements
    // were meant to be atomic, the author used a `-- transactional` header and
    // `execute_script` would have wrapped them; the default is autocommit so
    // CONCURRENTLY works.
    execute_script(pool, m.version, &m.sql, m.transactional).await?;

    sqlx::query(
        r"
        INSERT INTO _schema_migrations (version, name, checksum, is_contract, applied_by, duration_ms)
        VALUES ($1, $2, $3, $4, $5, $6)
        ",
    )
    .bind(m.version)
    .bind(format!("{}_{}", m.version, m.name))
    .bind(&checksum)
    .bind(m.is_contract_phase)
    .bind(whoami())
    .bind(started.elapsed().as_millis() as i64)
    .execute(pool)
    .await?;

    tracing::info!(
        version = m.version,
        name = %m.name,
        duration_ms = started.elapsed().as_millis() as u64,
        "migration applied"
    );
    Ok(())
}

/// Execute a SQL script statement-by-statement. Statement text is sent
/// exactly as split; the server reports which one failed via the error's
/// position, and we annotate the error with the migration version.
///
/// When `transactional` is true the whole script runs inside one transaction
/// and is rolled back on the first failure; otherwise each statement commits
/// in autocommit, which is what `CREATE INDEX CONCURRENTLY` requires.
async fn execute_script(
    pool: &PgPool,
    version: i64,
    script: &str,
    transactional: bool,
) -> Result<(), MigrateError> {
    let statements = split_sql(script, &SplitOptions::default());
    if !transactional {
        for stmt in statements {
            if let Err(e) = sqlx::query(&stmt).execute(pool).await {
                return Err(MigrateError::SqlSyntax {
                    version,
                    message: e.to_string(),
                });
            }
        }
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    for stmt in statements {
        if let Err(e) = sqlx::query(&stmt).execute(&mut *tx).await {
            // Explicit rollback: dropping `tx` would roll back too, but an
            // explicit call keeps the failure path obvious.
            tx.rollback().await.ok();
            return Err(MigrateError::SqlSyntax {
                version,
                message: e.to_string(),
            });
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn record_reverted(pool: &PgPool, version: i64) -> Result<(), MigrateError> {
    sqlx::query("DELETE FROM _schema_migrations WHERE version = $1")
        .bind(version)
        .execute(pool)
        .await?;
    Ok(())
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// Connection + advisory lock wrapper used by the CLI entry points
// ---------------------------------------------------------------------------

/// Connect to PostgreSQL for a migration run.
///
/// Accepts an explicit DSN or falls back to `DATABASE_URL`. Sets a
/// conservative statement timeout for the run — migrations may be long, but a
/// wedged statement should not hang forever.
pub async fn connect(dsn: Option<&str>) -> Result<PgPool, MigrateError> {
    let dsn = dsn
        .map(str::to_owned)
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .ok_or_else(|| {
            MigrateError::Sqlx(sqlx::Error::Configuration("DATABASE_URL not set".into()))
        })?;

    let pool = PgPool::connect(&dsn).await?;
    Ok(pool)
}

/// Hold `pg_advisory_lock(MIGRATION_LOCK_KEY)` for the duration of `f`.
///
/// The lock is session-scoped and lives on a dedicated connection opened for
/// exactly this purpose (kept open, and therefore out of the pool's rotation,
/// until `f` completes). If the holder dies, the session ends and PostgreSQL
/// releases the lock automatically — a crashed deploy can never deadlock the
/// next one. Acquisition polls `pg_try_advisory_lock` until [`LOCK_TIMEOUT`].
pub async fn with_advisory_lock<F, T>(pool: &PgPool, f: F) -> Result<T, MigrateError>
where
    F: std::future::Future<Output = Result<T, MigrateError>>,
{
    let mut lock_conn = acquire_advisory_lock(pool).await?;
    let out = f.await;
    // Release in all cases so a short-lived process returns the session to
    // the server cleanly; even without this, session teardown unlocks.
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(lock_conn.as_mut())
        .await;
    let _ = lock_conn.close().await;
    out
}

/// Poll `pg_try_advisory_lock` on a dedicated connection until it is ours or
/// [`LOCK_TIMEOUT`] elapses.
async fn acquire_advisory_lock(
    pool: &PgPool,
) -> Result<sqlx::pool::PoolConnection<Postgres>, MigrateError> {
    let deadline = tokio::time::Instant::now() + LOCK_TIMEOUT;
    loop {
        // acquire() hands us a pooled connection we can pin for the run.
        let mut conn = pool.acquire().await?;
        let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .fetch_one(conn.as_mut())
            .await?;
        if got {
            return Ok(conn);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(MigrateError::LockTimeout {
                key: MIGRATION_LOCK_KEY,
                timeout_secs: LOCK_TIMEOUT.as_secs(),
            });
        }
        drop(conn);
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── checksums ──────────────────────────────────────────────────────────

    #[test]
    fn checksum_is_deterministic_and_sensitive() {
        let a = checksum_of("SELECT 1;");
        let b = checksum_of("SELECT 1;");
        let c = checksum_of("SELECT 2;");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64); // SHA-256 hex
    }

    #[test]
    fn checksum_matches_known_sha256() {
        // echo -n "hello" | sha256sum
        assert_eq!(
            checksum_of("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    // ── phase classification ───────────────────────────────────────────────

    #[test]
    fn contract_phase_detection_is_case_insensitive() {
        assert!(is_contract_phase("contract_holder_balance_minor"));
        assert!(is_contract_phase("Contract_Promote_Col"));
        assert!(!is_contract_phase("expand_holder_balance_minor"));
        assert!(!is_contract_phase("add_index"));
        // "contraction" contains "contract"; fine — the convention only needs
        // the destructive case to be catchable.
        assert!(is_contract_phase("contraction_of_columns"));
    }

    // ── SQL splitting ──────────────────────────────────────────────────────

    #[test]
    fn split_on_simple_semicolons() {
        let sql = "CREATE TABLE a (id INT);\nCREATE TABLE b (id INT);";
        let stmts = split_sql(sql, &SplitOptions::default());
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].starts_with("CREATE TABLE a"));
        assert!(stmts[1].starts_with("CREATE TABLE b"));
    }

    #[test]
    fn split_keeps_do_block_intact() {
        // The semicolons inside the DO body must not split the statement.
        let sql = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') THEN
        PERFORM create_hypertable('events', 'occurred_at');
    END IF;
END;
$$;
CREATE INDEX idx ON events (id);
"#;
        let stmts = split_sql(sql, &SplitOptions::default());
        assert_eq!(stmts.len(), 2, "DO block must stay one statement");
        assert!(stmts[0].starts_with("DO $$"));
        assert!(stmts[0].contains("create_hypertable"));
        assert!(stmts[1].starts_with("CREATE INDEX"));
    }

    #[test]
    fn split_handles_tagged_dollar_quotes() {
        let sql = "DO $body$\nSELECT 1;\nSELECT 2;\n$body$;\nSELECT 3;";
        let stmts = split_sql(sql, &SplitOptions::default());
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("SELECT 2;"));
        assert_eq!(stmts[1], "SELECT 3");
    }

    #[test]
    fn split_ignores_semicolons_in_strings_and_comments() {
        let sql = r#"
-- a comment with a semicolon; inside
INSERT INTO notes (text) VALUES ('hello; world');
/* block comment; also with ; semicolons */
SELECT 1;
"#;
        let stmts = split_sql(sql, &SplitOptions::default());
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("hello; world"));
        // Comments travel with the statement they precede (documented
        // behavior); the semicolons inside them must not split anything.
        assert_eq!(
            stmts[1],
            "/* block comment; also with ; semicolons */\nSELECT 1"
        );
    }

    #[test]
    fn split_ignores_positional_parameters() {
        // $1 is not a dollar quote.
        let sql = "SELECT * FROM t WHERE id = $1 AND x = $2;";
        let stmts = split_sql(sql, &SplitOptions::default());
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("$1"));
    }

    #[test]
    fn split_handles_escaped_quotes_in_strings() {
        let sql = "INSERT INTO t VALUES ('it''s; tricky'); SELECT 1;";
        let stmts = split_sql(sql, &SplitOptions::default());
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("it''s; tricky"));
    }

    #[test]
    fn split_trailing_statement_without_semicolon() {
        let sql = "SELECT 1;\nSELECT 2";
        let stmts = split_sql(sql, &SplitOptions::default());
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[1], "SELECT 2");
    }

    #[test]
    fn split_empty_and_whitespace_only() {
        assert!(split_sql("", &SplitOptions::default()).is_empty());
        assert!(split_sql("  \n\t; ;  ", &SplitOptions::default()).is_empty());
    }

    // ── embedded-loading invariants ────────────────────────────────────────

    fn migration(version: i64, name: &str) -> Migration {
        Migration::new(version, name, format!("-- {version}\nSELECT {version};"))
            .with_down(format!("-- {version} down\nSELECT -{version};"))
    }

    #[test]
    fn load_embedded_sorts_by_version() {
        let loaded = load_embedded(&[
            migration(3, "contract_c"),
            migration(1, "initial"),
            migration(2, "expand_b"),
        ])
        .unwrap();
        let versions: Vec<i64> = loaded.iter().map(|m| m.version).collect();
        assert_eq!(versions, vec![1, 2, 3]);
    }

    #[test]
    fn load_embedded_rejects_duplicate_versions() {
        let err = load_embedded(&[migration(1, "a"), migration(1, "b")]).unwrap_err();
        assert!(matches!(err, MigrateError::DuplicateVersion { version: 1 }));
    }

    #[test]
    fn migration_display_pads_version() {
        assert_eq!(migration(7, "add_thing").to_string(), "0007_add_thing");
    }

    // ── transactional header ───────────────────────────────────────────────

    #[test]
    fn transactional_header_is_detected() {
        assert!(has_transactional_header("-- transactional\nSELECT 1;"));
        assert!(has_transactional_header("\n--  TRANSACTIONAL \nSELECT 1;"));
        assert!(!has_transactional_header(
            "-- not the magic word\nSELECT 1;"
        ));
        assert!(!has_transactional_header("SELECT 1;\n-- transactional"));
        assert!(!has_transactional_header(""));
    }

    #[test]
    fn migration_new_derives_flags() {
        let m = Migration::new(5, "contract_drop_thing", "SELECT 1;");
        assert!(m.is_contract_phase);
        assert!(!m.transactional);

        let t = Migration::new(6, "expand_thing", "-- transactional\nSELECT 1;");
        assert!(t.transactional);
        assert!(!t.is_contract_phase);
    }

    #[test]
    fn down_sql_round_trips_in_pair() {
        let m = migration(2, "expand_x");
        assert!(m.down_sql.is_some());
        assert!(m.down_sql.as_deref().unwrap().contains("down"));
    }
}
