//! Sandbox tenant isolation E2E (issue #468).
//!
//! Run the stack with `tessera sandbox start`, then:
//!   RUN_SANDBOX_E2E=true cargo test --manifest-path tests/e2e/Cargo.toml \
//!     --test sandbox_isolation -- --test-threads=1

use serde_json::Value;

const API: &str = "http://localhost:8081";

#[test]
fn migration_enforces_force_row_level_security() {
    let sql = include_str!("../../api/src/db/migrations/0005_sandbox_tenant_rls.sql");
    for table in [
        "sandbox_assets",
        "sandbox_holders",
        "sandbox_dividends",
        "sandbox_resets",
    ] {
        assert!(
            sql.contains(&format!("ALTER TABLE {table}"))
                && sql.contains("FORCE  ROW LEVEL SECURITY"),
            "migration must force RLS for {table}"
        );
    }
    assert!(sql.contains("current_setting('app.sandbox_tenant_id', true)"));
    assert!(sql.contains("WITH CHECK (tenant_id = tessera_sandbox_tenant())"));
}

#[tokio::test]
async fn tenants_cannot_see_each_others_data() {
    if std::env::var("RUN_SANDBOX_E2E").ok().as_deref() != Some("true") {
        return;
    }
    let client = reqwest::Client::new();

    let status_a: Value = client
        .get(format!("{API}/sandbox/status"))
        .header("X-Sandbox-Tenant-ID", "tenant-a")
        .send()
        .await
        .expect("sandbox API must be up")
        .json()
        .await
        .expect("JSON status");
    let status_b: Value = client
        .get(format!("{API}/sandbox/status"))
        .header("X-Sandbox-Tenant-ID", "tenant-b")
        .send()
        .await
        .expect("sandbox API must be up")
        .json()
        .await
        .expect("JSON status");

    assert_eq!(status_a["tenant_id"], "tenant-a");
    assert_eq!(status_b["tenant_id"], "tenant-b");
    assert_eq!(status_a["holders"], 100_000);
    assert_eq!(status_b["holders"], 100_000);

    // Resetting one tenant must not perturb the other.
    let reset: Value = client
        .post(format!("{API}/sandbox/reset"))
        .header("X-Sandbox-Tenant-ID", "tenant-a")
        .send()
        .await
        .expect("reset must succeed")
        .json()
        .await
        .expect("JSON reset summary");
    assert_eq!(reset["status"], "reset");
    assert!(
        reset["duration_ms"].as_u64().is_some_and(|ms| ms < 5_000),
        "reset exceeded the 5 s budget: {reset}"
    );
    let after_b: Value = client
        .get(format!("{API}/sandbox/status"))
        .header("X-Sandbox-Tenant-ID", "tenant-b")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after_b["holders"], status_b["holders"]);
}
