pub mod dev;
mod error;
mod generator;
mod options;
mod sandbox;

use std::io::{BufRead, Write};
use std::path::Path;

use clap::{Parser, Subcommand};

pub use error::CliError;
pub use generator::{generate_project, validate_project_name, ProjectConfig};
pub use options::{
    resolve_choices, AssetType, ComplianceModel, DevArgs, DividendStrategy, InitArgs,
    ProjectChoices, SandboxArgs, SandboxCommand, SandboxStartArgs, SandboxStopArgs,
};

#[derive(Debug, Parser)]
#[command(
    name = "tessera-cli",
    version,
    about = "Generate standalone Soroban RWA contracts",
    long_about = "Generate deterministic, self-contained Soroban RWA contract projects."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init(InitArgs),
    /// Start and stop the isolated regulatory sandbox (issue #468).
    Sandbox(SandboxArgs),
    /// Start the local development environment.
    Dev(DevArgs),
}

pub fn run() -> Result<(), CliError> {
    let cli = Cli::parse();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    run_cli(cli, &mut input, &mut output)
}

pub fn run_cli<C: BufRead, W: Write>(
    cli: Cli,
    input: &mut C,
    output: &mut W,
) -> Result<(), CliError> {
    match cli.command {
        Command::Init(args) => run_init(args, input, output),
        Command::Sandbox(args) => match args.command {
            SandboxCommand::Start(start_args) => sandbox::start(&start_args, output),
            SandboxCommand::Stop(stop_args) => sandbox::stop(&stop_args, output),
        },
        Command::Dev(args) => dev::run_dev(args),
    }
}

fn run_init<C: BufRead, W: Write>(
    args: InitArgs,
    input: &mut C,
    output: &mut W,
) -> Result<(), CliError> {
    validate_project_name(&args.name)?;
    let choices = resolve_choices(&args, input, output)?;
    let config = ProjectConfig::new(&args.name, choices)?;
    let output_root = args.output_dir.as_deref().unwrap_or_else(|| Path::new("."));
    let destination = generate_project(&config, output_root)?;
    writeln!(output, "created {}", destination.display()).map_err(|source| CliError::Io {
        path: destination.clone(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_init_command_and_selection_aliases() {
        let cli = Cli::try_parse_from([
            "tessera-cli",
            "init",
            "my-rwa-asset",
            "--type",
            "real-estate",
            "--compliance",
            "kyc",
            "--dividends",
            "proportional",
            "--non-interactive",
        ])
        .expect("command should parse");
        let Command::Init(args) = cli.command else {
            panic!("expected init command");
        };
        assert_eq!(args.name, "my-rwa-asset");
        assert_eq!(args.asset_type.as_deref(), Some("real-estate"));
        assert_eq!(args.compliance_model.as_deref(), Some("kyc"));
        assert_eq!(args.dividend_strategy.as_deref(), Some("proportional"));
        assert!(args.non_interactive);
    }

    #[test]
<<<<<<< HEAD
    fn parses_sandbox_start_command() {
        let cli = Cli::try_parse_from([
            "tessera",
            "sandbox",
            "start",
            "--tenant",
            "acme-1",
            "--assets",
            "100",
            "--holders",
            "200",
            "--seed",
            "7",
            "--print-only",
        ])
        .expect("sandbox start should parse");
        let Command::Sandbox(args) = cli.command else {
            panic!("expected sandbox command");
        };
        let SandboxCommand::Start(start) = args.command else {
            panic!("expected sandbox start");
        };
        assert_eq!(start.tenant, "acme-1");
        assert_eq!(start.assets, 100);
        assert_eq!(start.holders, 200);
        assert_eq!(start.seed, Some(7));
        assert!(start.print_only);
        assert!(start.wait);
=======
    fn parses_dev_command_with_custom_flags() {
        let cli = Cli::try_parse_from([
            "tessera-cli",
            "dev",
            "--api-port",
            "8081",
            "--rpc-url",
            "http://localhost:8000",
            "--docs-port",
            "3001",
            "--no-docker",
            "--skip-docs",
            "--once",
        ])
        .expect("dev command should parse");
        let Command::Dev(args) = cli.command;
        assert_eq!(args.api_port, 8081);
        assert_eq!(args.rpc_url, "http://localhost:8000");
        assert_eq!(args.docs_port, 3001);
        assert!(args.no_docker);
        assert!(args.skip_docs);
        assert!(args.once);
>>>>>>> upstream/main
    }
}
