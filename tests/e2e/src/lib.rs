//! Shared helpers for the Tessera E2E test suite (issue #143).
//!
//! This library provides:
//! - [`TestEnvironment`]: reads configuration from environment variables,
//!   with safe fallbacks to local defaults so tests can run both in CI
//!   (against a docker-compose stack) and locally (against testnet).
//! - [`DockerCompose`]: thin wrapper around `docker compose` subprocesses
//!   for starting, stopping, and capturing logs from the local E2E stack.
//! - [`wait_for_api`]: polls the Tessera API `/health` endpoint until it
//!   responds or a timeout elapses, giving containers time to start.
//! - [`wait_for_rpc`]: polls a Soroban JSON-RPC endpoint (`getHealth`)
//!   with the same back-off loop.

use std::process::{Command, Output, Stdio};
use std::time::Duration;

// ─── TestEnvironment ────────────────────────────────────────────────────────

/// Configuration resolved from the process environment at test startup.
///
/// Override any field by setting the corresponding `E2E_*` variable before
/// running the suite.  All values fall back to defaults that match the
/// `docker-compose.yml` in the same directory.
#[derive(Debug, Clone)]
pub struct TestEnvironment {
    /// Base URL of the running Tessera REST API, e.g. `http://localhost:8080`.
    pub api_base_url: String,
    /// Soroban JSON-RPC endpoint the API is configured against.
    pub rpc_url: String,
    /// Network passphrase that the RPC node uses.
    pub network_passphrase: String,
    /// Registry contract ID (StrKey C…).
    pub registry_id: String,
    /// Compliance contract ID (StrKey C…).
    pub compliance_id: String,
    /// Dividend contract ID (StrKey C…).
    pub dividend_id: String,
    /// Sample asset-token contract ID (StrKey C…).
    pub asset_token_id: String,
    /// How long to wait for the API to become healthy on startup.
    pub api_startup_timeout: Duration,
    /// How long to wait for the RPC node to become healthy on startup.
    pub rpc_startup_timeout: Duration,
    /// Poll interval used by [`wait_for_api`] and [`wait_for_rpc`].
    pub poll_interval: Duration,
}

impl TestEnvironment {
    /// Build a [`TestEnvironment`] from the process environment.
    ///
    /// Every field has a safe local default so the suite is runnable without
    /// any environment configuration. CI overrides the relevant variables to
    /// point at the docker-compose stack.
    pub fn from_env() -> Self {
        Self {
            api_base_url: env_or(
                "E2E_API_BASE_URL",
                "http://localhost:8080",
            ),
            rpc_url: env_or(
                "E2E_RPC_URL",
                "https://soroban-testnet.stellar.org",
            ),
            network_passphrase: env_or(
                "E2E_NETWORK_PASSPHRASE",
                "Test SDF Network ; September 2015",
            ),
            registry_id: env_or(
                "E2E_REGISTRY_ID",
                "CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3",
            ),
            compliance_id: env_or(
                "E2E_COMPLIANCE_ID",
                "CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU",
            ),
            dividend_id: env_or(
                "E2E_DIVIDEND_ID",
                "CAR4XY3CEBQWFOL27JEWFW34KXSIZA7RFKDQMEIV7ZU723RWY37I2SYX",
            ),
            asset_token_id: env_or(
                "E2E_ASSET_TOKEN_ID",
                "CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ",
            ),
            api_startup_timeout: Duration::from_secs(
                env_or("E2E_API_STARTUP_TIMEOUT_SECS", "60")
                    .parse()
                    .unwrap_or(60),
            ),
            rpc_startup_timeout: Duration::from_secs(
                env_or("E2E_RPC_STARTUP_TIMEOUT_SECS", "120")
                    .parse()
                    .unwrap_or(120),
            ),
            poll_interval: Duration::from_millis(
                env_or("E2E_POLL_INTERVAL_MS", "500")
                    .parse()
                    .unwrap_or(500),
            ),
        }
    }

    /// Return `true` when the suite is being run in Docker-compose mode
    /// (i.e. `E2E_DOCKER_COMPOSE=1` is set).  Tests that require on-chain
    /// state mutation skip themselves when this is `false`, the same
    /// way [`testnet_integration`](../../api/tests/testnet_integration.rs)
    /// uses `RUN_TESTNET_TESTS`.
    pub fn docker_compose_mode(&self) -> bool {
        std::env::var("E2E_DOCKER_COMPOSE")
            .ok()
            .as_deref()
            == Some("1")
    }

    /// Return `true` when the suite may hit the live testnet
    /// (`RUN_TESTNET_TESTS=1`).
    pub fn testnet_mode(&self) -> bool {
        std::env::var("RUN_TESTNET_TESTS")
            .ok()
            .as_deref()
            == Some("1")
    }

    /// Return `true` when any live-infrastructure mode is active.
    pub fn live_infra(&self) -> bool {
        self.docker_compose_mode() || self.testnet_mode()
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

// ─── DockerCompose ───────────────────────────────────────────────────────────

/// Manages the lifecycle of a `docker compose` stack for the E2E suite.
///
/// Creating a [`DockerCompose`] does **not** automatically start the stack.
/// Call [`DockerCompose::up`] to start, and [`DockerCompose::down`] (or let
/// the value drop) to tear it down.  Log output is captured so test failures
/// include container logs in the panic message.
pub struct DockerCompose {
    /// Path to the directory containing `docker-compose.yml`.
    compose_dir: std::path::PathBuf,
    /// Optional project name passed via `--project-name` for isolation.
    project_name: String,
}

impl DockerCompose {
    /// Create a new handle pointing at `compose_dir`.
    ///
    /// `project_name` is passed to `docker compose --project-name` so
    /// concurrent test runs do not share containers.
    pub fn new(compose_dir: impl Into<std::path::PathBuf>, project_name: impl Into<String>) -> Self {
        Self {
            compose_dir: compose_dir.into(),
            project_name: project_name.into(),
        }
    }

    /// Run `docker compose up --detach --wait`.
    ///
    /// Returns an error string if the command exits non-zero.
    pub fn up(&self) -> Result<(), String> {
        self.run(&["up", "--detach", "--wait"])
    }

    /// Run `docker compose down --volumes --remove-orphans`.
    ///
    /// Intended to be called in a test teardown or `Drop` impl.
    pub fn down(&self) -> Result<(), String> {
        self.run(&["down", "--volumes", "--remove-orphans"])
    }

    /// Capture and return the combined stdout+stderr of all containers.
    pub fn logs(&self) -> String {
        let output = Command::new("docker")
            .args(["compose", "--project-name", &self.project_name, "logs", "--no-color"])
            .current_dir(&self.compose_dir)
            .output()
            .unwrap_or_else(|e| panic!("docker compose logs failed to spawn: {e}"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        format!("STDOUT:\n{stdout}\nSTDERR:\n{stderr}")
    }

    fn run(&self, args: &[&str]) -> Result<(), String> {
        let mut full_args = vec!["compose", "--project-name", &self.project_name];
        full_args.extend_from_slice(args);

        let output: Output = Command::new("docker")
            .args(&full_args)
            .current_dir(&self.compose_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to spawn `docker {args:?}`: {e}"))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            Err(format!(
                "`docker {args:?}` failed ({})\nSTDOUT: {stdout}\nSTDERR: {stderr}",
                output.status
            ))
        }
    }
}

impl Drop for DockerCompose {
    fn drop(&mut self) {
        // Best-effort teardown; ignore errors on drop so the real test
        // failure (if any) is not replaced by a teardown error.
        let _ = self.down();
    }
}

// ─── Health-poll helpers ─────────────────────────────────────────────────────

/// Poll `{base_url}/health` until the server responds with any HTTP status
/// (including `503 Degraded`) or the deadline passes.
///
/// Returns `Ok(())` on the first successful TCP connection, or an `Err`
/// describing the timeout.
pub async fn wait_for_api(base_url: &str, timeout: Duration, poll_interval: Duration) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("failed to build reqwest client");

    let deadline = tokio::time::Instant::now() + timeout;
    let url = format!("{base_url}/health");

    loop {
        match client.get(&url).send().await {
            Ok(_) => return Ok(()),
            Err(_) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Tessera API at {base_url} did not become ready within {timeout:?}"
            ));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Poll the Soroban JSON-RPC `getHealth` method until it returns `"healthy"`
/// or the deadline passes.
///
/// Returns `Ok(())` on the first `"healthy"` result, or an `Err` on timeout.
pub async fn wait_for_rpc(rpc_url: &str, timeout: Duration, poll_interval: Duration) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("failed to build reqwest client");

    let deadline = tokio::time::Instant::now() + timeout;
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getHealth",
        "params": {}
    });

    loop {
        if let Ok(resp) = client.post(rpc_url).json(&body).send().await {
            if let Ok(value) = resp.json::<serde_json::Value>().await {
                if value["result"]["status"] == "healthy" {
                    return Ok(());
                }
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Soroban RPC at {rpc_url} did not become healthy within {timeout:?}"
            ));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

// ─── JSON assertion helpers ──────────────────────────────────────────────────

/// Assert that `value[field]` exists, logging the full `value` on failure.
pub fn assert_json_field(value: &serde_json::Value, field: &str) {
    assert!(
        value.get(field).is_some(),
        "expected JSON field `{field}` to be present, got: {value}"
    );
}

/// Assert that `value[field]` equals `expected`, logging the full `value` on failure.
pub fn assert_json_field_eq(
    value: &serde_json::Value,
    field: &str,
    expected: &serde_json::Value,
) {
    assert_eq!(
        value.get(field),
        Some(expected),
        "expected JSON field `{field}` == {expected}, got: {value}"
    );
}

/// Assert that `value` is a JSON array with at least `min` elements.
pub fn assert_json_array_min_len(value: &serde_json::Value, min: usize) {
    let arr = value
        .as_array()
        .unwrap_or_else(|| panic!("expected a JSON array, got: {value}"));
    assert!(
        arr.len() >= min,
        "expected array length >= {min}, got {}: {value}",
        arr.len()
    );
}
