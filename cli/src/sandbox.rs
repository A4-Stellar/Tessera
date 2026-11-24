//! Implementation of `tessera sandbox start|stop`.
//!
//! The compose file is embedded in the binary so the command works from a
//! release install without the source checkout. `--compose-file` overrides the
//! destination for operators who want to edit the stack.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::CliError;
use crate::options::{SandboxStartArgs, SandboxStopArgs};

pub const COMPOSE_YAML: &str = include_str!("../../tests/e2e/docker-compose.sandbox.yml");

fn default_compose_path() -> Result<PathBuf, CliError> {
    let dir = std::env::temp_dir().join("tessera-sandbox");
    Ok(dir.join("docker-compose.yml"))
}

fn write_compose(path: &Path) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CliError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, COMPOSE_YAML).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_tenant(tenant: &str) -> Result<(), CliError> {
    if tenant.is_empty()
        || tenant.len() > 128
        || !tenant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(CliError::Sandbox(
            "tenant id must be 1-128 ASCII letters, digits, '-' or '_'".to_owned(),
        ));
    }
    Ok(())
}

fn command_line(command: &Command) -> String {
    let mut parts: Vec<String> = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| format!("{}={}", key.to_string_lossy(), value.to_string_lossy()))
        })
        .collect();
    parts.push(command.get_program().to_string_lossy().to_string());
    parts.extend(command.get_args().map(|a| a.to_string_lossy().to_string()));
    parts.join(" ")
}

/// Start the sandbox. Returns after the stack is healthy (unless `--no-wait`).
pub fn start<W: Write>(args: &SandboxStartArgs, output: &mut W) -> Result<(), CliError> {
    validate_tenant(&args.tenant)?;
    let path = args
        .compose_file
        .clone()
        .map(Ok)
        .unwrap_or_else(default_compose_path)?;
    write_compose(&path)?;

    let mut command = Command::new("docker");
    command
        .arg("compose")
        .arg("-f")
        .arg(&path)
        .arg("up")
        .arg("--detach");
    if args.wait {
        command.arg("--wait");
    }
    command
        .env("SANDBOX_TENANT", &args.tenant)
        .env("SANDBOX_ASSETS", args.assets.to_string())
        .env("SANDBOX_HOLDERS", args.holders.to_string());
    if let Some(seed) = args.seed {
        command.env("SANDBOX_SEED", seed.to_string());
    }

    writeln!(output, "compose file: {}", path.display()).map_err(|source| CliError::Io {
        path: path.clone(),
        source,
    })?;
    writeln!(output, "$ {}", command_line(&command)).map_err(|source| CliError::Io {
        path: path.clone(),
        source,
    })?;

    if args.print_only {
        return Ok(());
    }

    let status = command
        .status()
        .map_err(|error| CliError::Sandbox(format!("failed to launch docker compose: {error}")))?;
    if !status.success() {
        return Err(CliError::Sandbox(format!(
            "docker compose exited with {status}"
        )));
    }
    writeln!(
        output,
        "sandbox ready: API http://localhost:8081 · RPC http://localhost:8001/soroban/rpc · tenant '{}'",
        args.tenant
    )
    .map_err(|source| CliError::Io {
        path,
        source,
    })
}

/// Tear the stack down.
pub fn stop<W: Write>(args: &SandboxStopArgs, output: &mut W) -> Result<(), CliError> {
    let path = args
        .compose_file
        .clone()
        .map(Ok)
        .unwrap_or_else(default_compose_path)?;
    if !path.exists() {
        write_compose(&path)?;
    }
    let mut command = Command::new("docker");
    command.arg("compose").arg("-f").arg(&path).arg("down");
    if args.volumes {
        command.arg("--volumes");
    }
    command.arg("--remove-orphans");

    writeln!(output, "$ {}", command_line(&command)).map_err(|source| CliError::Io {
        path: path.clone(),
        source,
    })?;
    if args.print_only {
        return Ok(());
    }
    let status = command
        .status()
        .map_err(|error| CliError::Sandbox(format!("failed to launch docker compose: {error}")))?;
    if !status.success() {
        return Err(CliError::Sandbox(format!(
            "docker compose exited with {status}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::SandboxStartArgs;

    #[test]
    fn embedded_compose_declares_the_required_services() {
        for service in [
            "stellar-node:",
            "postgres:",
            "datagen:",
            "seeder:",
            "contract-deployer:",
            "tessera-api:",
        ] {
            assert!(COMPOSE_YAML.contains(service), "missing {service}");
        }
        assert!(COMPOSE_YAML.contains("X-Sandbox-Tenant-ID"));
    }

    #[test]
    fn print_only_writes_compose_and_does_not_need_docker() {
        let dir = std::env::temp_dir().join(format!("tessera-cli-sandbox-{}", std::process::id()));
        let path = dir.join("compose.yml");
        let args = SandboxStartArgs {
            compose_file: Some(path.clone()),
            tenant: "tenant-1".to_owned(),
            assets: 10,
            holders: 20,
            seed: Some(7),
            print_only: true,
            wait: true,
        };
        let mut output = Vec::new();
        start(&args, &mut output).expect("print-only start must succeed");
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("compose file:"));
        assert!(text.contains("SANDBOX_TENANT=tenant-1"));
        assert_eq!(fs::read_to_string(&path).unwrap(), COMPOSE_YAML);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_tenant_ids_that_could_break_the_stack() {
        let args = SandboxStartArgs {
            compose_file: None,
            tenant: "bad;tenant".to_owned(),
            assets: 10,
            holders: 20,
            seed: None,
            print_only: true,
            wait: true,
        };
        let mut output = Vec::new();
        assert!(start(&args, &mut output).is_err());
    }
}
