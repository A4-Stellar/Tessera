//! Integration tests for the zero-downtime migration engine (issue #71).
//!
//! These exercise the real `tessera-api` CLI subcommands (`migrate`,
//! `migrate-down`, `migrate-status`) against a live PostgreSQL database —
//! the same binary a deploy pipeline would run — and verify the core
//! acceptance criterion: **concurrent read/write workloads keep running,
//! error-free and with bounded latency, while a migration executes.**
//!
//! Opt-in only (they need a real PostgreSQL instance):
//!
//! ```sh
//! RUN_MIGRATION_TESTS=1 \
//! DATABASE_URL=postgres://user:pass@localhost:5432/tessera_migrate_test \
//! cargo test --test migration_integration
//! ```
//!
//! `DATABASE_URL` must point at a **throwaway database** — the scenario
//! applies and rolls back the full migration set, destroying its data.
//! Without the opt-in env var every test here returns early, so a plain
//! `cargo test` (including CI's default job) never needs a database.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::postgres::PgPoolOptions;

/// Upper bound for any single application statement executed while a
/// migration runs. The expand phase is catalog-only plus
/// `CREATE INDEX CONCURRENTLY`, so live traffic should never block
/// meaningfully; the bound is generous for slow CI runners but still fails
/// if a migration takes a table-wide lock and parks the workload.
const MAX_STATEMENT_LATENCY: Duration = Duration::from_secs(5);

fn migration_db_url() -> Option<String> {
    if std::env::var("RUN_MIGRATION_TESTS").ok().as_deref() != Some("1") {
        return None;
    }
    std::env::var("DATABASE_URL").ok()
}

/// Run the real binary's migration CLI and capture its output.
fn run_cli(args: &[&str], allow_destructive: bool, db_url: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tessera-api"));
    cmd.args(args)
        .env("DATABASE_URL", db_url)
        .env("RUST_LOG", "error");
    if allow_destructive {
        cmd.env("ALLOW_DESTRUCTIVE_MIGRATIONS", "1");
    }
    cmd.output().expect("failed to spawn tessera-api binary")
}

fn assert_exit_ok(out: &Output, context: &str) {
    assert!(
        out.status.success(),
        "{context}: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn assert_exit_err(out: &Output, context: &str) {
    assert!(
        !out.status.success(),
        "{context}: expected nonzero exit, but succeeded\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout),
    );
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Seed one asset and a handful of holders with exact integer (stroop)
/// balances — the application stores `i128` stroop amounts (`assets.decimals`
/// defaults to 7), and the `NUMERIC(39)` columns are scale-0 by design.
/// Returns the seeded balance total as an exact decimal string (used later
/// to prove the contract phase preserves every value).
async fn seed_asset_and_holders(pool: &sqlx::PgPool) -> i64 {
    let asset_id: i64 = sqlx::query_scalar(
        "INSERT INTO assets (token_contract, issuer, name, symbol, asset_type, compliance_contract)
         VALUES ('CSEEDTEST', 'GSEEDTEST', 'Seed Asset', 'SEED', 'bond', 'CSEEDCOMPLIANCE')
         RETURNING id",
    )
    .fetch_one(pool)
    .await
    .expect("seed asset");

    // 100.5, 200.25, 50, 75.125, 300 asset units expressed in stroops.
    let balances = ["1005000000", "2002500000", "500000000", "751250000", "3000000000"];
    for (i, balance) in balances.iter().enumerate() {
        sqlx::query(
            "INSERT INTO holders (asset_id, address, balance) VALUES ($1, $2, $3::numeric)",
        )
        .bind(asset_id)
        .bind(format!("GSEEDHOLDER{i}"))
        .bind(*balance)
        .execute(pool)
        .await
        .expect("seed holder");
    }

    asset_id
}

/// A concurrent workload that hammers the database with writes
/// (`transactions`) and reads (`holders`, `transactions`) until stopped,
/// recording per-statement latency and any error. Writers deliberately
/// touch only columns that survive every phase of the expand/contract pair
/// — the dual-write pattern a real rolling deploy would use.
#[derive(Default)]
struct WorkloadReport {
    writes: usize,
    reads: usize,
    errors: usize,
    max_latency: Duration,
}

async fn run_workload(pool: sqlx::PgPool, asset_id: i64, stop: Arc<AtomicBool>) -> WorkloadReport {
    let mut handles = Vec::new();

    // 4 writer tasks on the `transactions` table.
    for w in 0..4 {
        let pool = pool.clone();
        let stop = stop.clone();
        handles.push(tokio::spawn(async move {
            let mut report = WorkloadReport::default();
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                let result = sqlx::query(
                    "INSERT INTO transactions
                         (asset_id, tx_hash, ledger_sequence, occurred_at, from_address, to_address, amount, tx_type)
                     VALUES ($1, $2, $3, NOW(), $4, $5, '10.5'::numeric, 'transfer')",
                )
                .bind(asset_id)
                .bind(format!("txhash-{w}-{n}"))
                .bind(1_000_000i64 + n as i64)
                .bind(format!("GFROM{w}"))
                .bind(format!("GTO{w}"))
                .execute(&pool)
                .await;
                match result {
                    Ok(_) => {
                        report.writes += 1;
                        report.max_latency = report.max_latency.max(started.elapsed());
                    }
                    Err(_) => report.errors += 1,
                }
                n += 1;
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            report
        }));
    }

    // 2 reader tasks on `holders` and `transactions`.
    for _ in 0..2 {
        let pool = pool.clone();
        let stop = stop.clone();
        handles.push(tokio::spawn(async move {
            let mut report = WorkloadReport::default();
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                let holders: Result<i64, _> =
                    sqlx::query_scalar("SELECT COUNT(*) FROM holders WHERE asset_id = $1")
                        .bind(asset_id)
                        .fetch_one(&pool)
                        .await;
                let txs: Result<i64, _> =
                    sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE asset_id = $1")
                        .bind(asset_id)
                        .fetch_one(&pool)
                        .await;
                match (holders, txs) {
                    (Ok(_), Ok(_)) => {
                        report.reads += 1;
                        report.max_latency = report.max_latency.max(started.elapsed());
                    }
                    _ => report.errors += 1,
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            report
        }));
    }

    let mut total = WorkloadReport::default();
    for handle in handles {
        let report = handle.await.expect("workload task panicked");
        total.writes += report.writes;
        total.reads += report.reads;
        total.errors += report.errors;
        total.max_latency = total.max_latency.max(report.max_latency);
    }
    total
}

#[tokio::test(flavor = "multi_thread")]
async fn migration_lifecycle_with_concurrent_workload() {
    let Some(db_url) = migration_db_url() else {
        eprintln!(
            "skipping migration_lifecycle_with_concurrent_workload: \
             set RUN_MIGRATION_TESTS=1 and DATABASE_URL (empty throwaway DB)"
        );
        return;
    };

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&db_url)
        .await
        .expect("connect to the throwaway migration database");

    // Best-effort reset of any previous partial run; ignored when the
    // history table does not exist yet.
    let _ = run_cli(&["migrate-down", "99"], true, &db_url);

    // ── 1. Expand phases run freely; the contract phase is gated ──────────
    let out = run_cli(&["migrate"], false, &db_url);
    assert_exit_err(
        &out,
        "migrate must refuse the contract phase without the flag",
    );
    let out_stdout = stdout(&out);
    assert!(
        out_stdout.contains("applied 0001") && out_stdout.contains("applied 0002"),
        "expand phases should apply freely; stdout was:\n{out_stdout}"
    );
    assert!(
        stderr(&out).contains("contract (destructive) phase"),
        "refusal should explain the gate; stderr was:\n{}",
        stderr(&out)
    );

    // ── 2. Seed data through the expand-phase schema ──────────────────────
    let asset_id = seed_asset_and_holders(&pool).await;

    // ── 3. Roll 0002 back and re-apply it WHILE traffic is live ───────────
    let stop = Arc::new(AtomicBool::new(false));
    let workload = tokio::spawn(run_workload(pool.clone(), asset_id, stop.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await; // let traffic ramp up

    let out = run_cli(&["migrate-down", "1"], false, &db_url);
    assert_exit_ok(&out, "migrate-down of an expand phase under live traffic");
    assert!(
        stdout(&out).contains("reverted 0002"),
        "expected 0002 reverted; stdout was:\n{}",
        stdout(&out)
    );

    let out = run_cli(&["migrate"], false, &db_url);
    assert!(
        stdout(&out).contains("applied 0002"),
        "expand phase should re-apply under live traffic; stdout was:\n{}",
        stdout(&out)
    );
    // The run then hits the 0003 gate, which is expected and checked above.

    stop.store(true, Ordering::Relaxed);
    let report = workload.await.expect("workload task panicked");

    assert_eq!(
        report.errors, 0,
        "the concurrent workload must observe zero errors while migrations run"
    );
    assert!(
        report.writes > 50 && report.reads > 50,
        "workload should have made real progress (writes={}, reads={})",
        report.writes,
        report.reads
    );
    assert!(
        report.max_latency < MAX_STATEMENT_LATENCY,
        "a workload statement blocked for {:?} during migration — the expand \
         phase must never take a table-wide lock",
        report.max_latency
    );

    // ── 4. Contract pre-flight guard: refuses an incomplete backfill ──────
    let out = run_cli(&["migrate"], true, &db_url);
    assert_exit_err(
        &out,
        "contract phase must abort while balance_minor is NULL anywhere",
    );
    assert!(
        stderr(&out).contains("backfill"),
        "pre-flight guard should name the fix; stderr was:\n{}",
        stderr(&out)
    );

    // ── 5. Documented backfill, then the contract phase succeeds ──────────
    sqlx::query("UPDATE holders SET balance_minor = balance WHERE balance_minor IS NULL")
        .execute(&pool)
        .await
        .expect("run the documented backfill");

    let out = run_cli(&["migrate"], true, &db_url);
    assert_exit_ok(&out, "contract phase after backfill");

    // The dual-write column was promoted: `balance_minor` is gone, and every
    // seeded value survived exactly under the original `balance` name.
    let minor_left: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_name = 'holders' AND column_name = 'balance_minor'",
    )
    .fetch_one(&pool)
    .await
    .expect("check balance_minor is gone");
    assert_eq!(minor_left, 0, "balance_minor should have been renamed away");

    let total: String =
        sqlx::query_scalar("SELECT SUM(balance)::text FROM holders WHERE asset_id = $1")
            .bind(asset_id)
            .fetch_one(&pool)
            .await
            .expect("sum holder balances");    assert_eq!(total, "7258750000", "contract phase must preserve balances exactly");

    // ── 6. Verified rollback: down 3 steps, then a clean re-run ───────────
    let out = run_cli(&["migrate-down", "3"], true, &db_url);
    assert_exit_ok(&out, "full rollback");
    let rolled = stdout(&out);
    assert!(
        rolled.contains("reverted 0003")
            && rolled.contains("reverted 0002")
            && rolled.contains("reverted 0001"),
        "expected 0003, 0002, 0001 all reverted; stdout was:\n{rolled}"
    );

    let out = run_cli(&["migrate-status"], false, &db_url);
    assert_exit_ok(&out, "migrate-status after full rollback");
    assert!(
        stdout(&out).contains("pending") && !stdout(&out).contains("applied"),
        "everything should report pending after the rollback; status was:\n{}",
        stdout(&out)
    );

    let out = run_cli(&["migrate"], false, &db_url);
    let again = stdout(&out);
    assert!(
        again.contains("applied 0001") && again.contains("applied 0002"),
        "down scripts must restore a schema the up path can run again; stdout was:\n{again}"
    );

    pool.close().await;
}
