use tessera_cli::{run_cli, Cli, Command, InitArgs};

#[test]
fn cli_help_works() {
    let cli = Cli::try_parse_from(["tessera-cli", "--help"]);
    assert!(cli.is_err());
}

#[test]
fn init_subcommand_parses() {
    let cli = Cli::try_parse_from([
        "tessera-cli",
        "init",
        "test-project",
        "--asset-type",
        "real-estate",
        "--compliance-model",
        "kyc",
        "--dividend-strategy",
        "proportional",
        "--non-interactive",
    ])
    .expect("should parse");
    
    match cli.command {
        Command::Init(args) => {
            assert_eq!(args.name, "test-project");
            assert_eq!(args.asset_type, Some("real-estate".to_string()));
            assert_eq!(args.compliance_model, Some("kyc".to_string()));
            assert_eq!(args.dividend_strategy, Some("proportional".to_string()));
            assert!(args.non_interactive);
        }
    }
}

#[test]
fn run_cli_generates_project() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let output_dir = temp_dir.path().to_path_buf();
    
    let cli = Cli {
        command: Command::Init(InitArgs {
            name: "test-asset".to_string(),
            asset_type: Some("real-estate".to_string()),
            compliance_model: Some("kyc".to_string()),
            dividend_strategy: Some("proportional".to_string()),
            non_interactive: true,
            defaults: false,
            output_dir: Some(output_dir.clone()),
        }),
    };
    
    let mut input = std::io::empty();
    let mut output = Vec::new();
    
    run_cli(cli, &mut input, &mut output).expect("cli should run");
    
    let project_dir = output_dir.join("test-asset");
    assert!(project_dir.exists());
    assert!(project_dir.join("Cargo.toml").exists());
    assert!(project_dir.join("src/lib.rs").exists());
    assert!(project_dir.join("tests/contract.rs").exists());
    assert!(project_dir.join("README.md").exists());
    assert!(project_dir.join(".gitignore").exists());
    assert!(project_dir.join("rust-toolchain.toml").exists());
}