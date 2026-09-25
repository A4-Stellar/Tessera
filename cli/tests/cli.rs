use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let number = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "tessera-cli-test-{}-{}",
            std::process::id(),
            number
        ));
        fs::create_dir_all(&path).expect("test directory should be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(directory: &TestDirectory) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tessera-cli"));
    command.current_dir(directory.path());
    command
}

fn run(directory: &TestDirectory, arguments: &[&str]) -> Output {
    command(directory)
        .args(arguments)
        .output()
        .expect("CLI process should start")
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed: {}",
        output_text(output)
    );
}

fn assert_failure(output: &Output, expected: &str) {
    assert!(!output.status.success(), "CLI unexpectedly succeeded");
    assert!(
        output_text(output).contains(expected),
        "expected '{expected}' in CLI output: {}",
        output_text(output)
    );
}

fn generated_files(directory: &TestDirectory, project: &str) -> Vec<(&'static str, String)> {
    [
        "Cargo.toml",
        "rust-toolchain.toml",
        "README.md",
        ".gitignore",
        "src/lib.rs",
        "tests/contract.rs",
    ]
    .into_iter()
    .map(|relative| {
        let contents = fs::read_to_string(directory.path().join(project).join(relative))
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        (relative, contents)
    })
    .collect()
}

#[test]
fn noninteractive_generation_is_self_contained_and_deterministic() {
    let first = TestDirectory::new();
    let second = TestDirectory::new();
    let first_arguments = [
        "init",
        "my-rwa-asset",
        "--asset-type",
        "real-estate",
        "--compliance-model",
        "kyc",
        "--dividend-strategy",
        "proportional",
        "--non-interactive",
    ];
    let first_output = run(&first, &first_arguments);
    assert_success(&first_output);
    let second_arguments = [
        "init",
        "my-rwa-asset",
        "--asset-type",
        "real-estate",
        "--compliance-model",
        "kyc",
        "--dividend-strategy",
        "proportional",
        "--non-interactive",
    ];
    let second_output = run(&second, &second_arguments);
    assert_success(&second_output);

    let first_files = generated_files(&first, "my-rwa-asset");
    let second_files = generated_files(&second, "my-rwa-asset");
    assert_eq!(first_files, second_files);
    let manifest = &first_files[0].1;
    assert!(manifest.contains("soroban-sdk = \"25.0.0\""));
    assert!(!manifest.contains(env!("CARGO_MANIFEST_DIR")));
    let toolchain = &first_files[1].1;
    assert!(toolchain.contains("channel = \"1.81.0\""));
    assert!(toolchain.contains("wasm32-unknown-unknown"));
    let source = &first_files[4].1;
    assert!(source.contains("real-estate"));
    assert!(source.contains("kyc"));
    assert!(source.contains("proportional"));
    for (_, contents) in &first_files {
        assert!(!contents.contains(env!("CARGO_MANIFEST_DIR")));
        assert!(!contents.contains("cli/templates"));
    }
    for relative in ["src/lib.rs", "tests/contract.rs"] {
        let contents = fs::read_to_string(first.path().join("my-rwa-asset").join(relative))
            .expect("generated source should be readable");
        for line in contents.lines() {
            let trimmed = line.trim_start();
            assert!(!trimmed.starts_with("//"));
            assert!(!trimmed.starts_with("/*"));
            assert!(!trimmed.starts_with("*"));
        }
    }
}

#[test]
fn existing_destination_is_rejected_without_overwrite() {
    let directory = TestDirectory::new();
    let project = directory.path().join("my-rwa-asset");
    fs::create_dir(&project).expect("project directory should be created");
    let sentinel = project.join("keep.txt");
    fs::write(&sentinel, "keep").expect("sentinel should be written");

    let output = run(
        &directory,
        &[
            "init",
            "my-rwa-asset",
            "--asset-type",
            "invoice",
            "--compliance-model",
            "allowlist",
            "--dividend-strategy",
            "fixed",
            "--non-interactive",
        ],
    );
    assert_failure(&output, "refusing to overwrite");
    assert_eq!(
        fs::read_to_string(sentinel).expect("sentinel should remain"),
        "keep"
    );
    assert!(!project.join("Cargo.toml").exists());
}

#[test]
fn unsafe_project_names_are_rejected() {
    let directory = TestDirectory::new();
    for name in [
        "../escape",
        "nested/name",
        "..",
        "/absolute",
        "bad\\name",
        "9starts-with-digit",
        "crate",
        "type",
    ] {
        let output = run(
            &directory,
            &[
                "init",
                name,
                "--asset-type",
                "debt",
                "--compliance-model",
                "hybrid",
                "--dividend-strategy",
                "tiered",
                "--non-interactive",
            ],
        );
        assert_failure(&output, "invalid project name");
    }
}

#[test]
fn interactive_prompts_accept_numeric_answers() {
    let directory = TestDirectory::new();
    let mut child = command(&directory)
        .args(["init", "interactive-asset"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("interactive CLI should start");
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(b"1\n1\n1\n")
        .expect("answers should be written");
    let output = child
        .wait_with_output()
        .expect("interactive CLI should finish");
    assert_success(&output);
    let text = output_text(&output);
    assert!(text.contains("Asset type:"));
    assert!(text.contains("Compliance model:"));
    assert!(text.contains("Dividend strategy:"));
    assert!(text.contains("created"));
    assert!(directory
        .path()
        .join("interactive-asset/Cargo.toml")
        .exists());
}

#[test]
fn noninteractive_mode_requires_explicit_choices() {
    let directory = TestDirectory::new();
    let output = run(
        &directory,
        &["init", "automation-asset", "--non-interactive"],
    );
    assert_failure(&output, "requires --asset-type");
    assert!(!directory.path().join("automation-asset").exists());
}

#[test]
fn defaults_can_be_used_in_noninteractive_mode() {
    let directory = TestDirectory::new();
    let output = run(
        &directory,
        &["init", "default-asset", "--non-interactive", "--defaults"],
    );
    assert_success(&output);
    let source = fs::read_to_string(directory.path().join("default-asset/src/lib.rs"))
        .expect("generated source should be readable");
    assert!(source.contains("real-estate"));
    assert!(source.contains("kyc"));
    assert!(source.contains("proportional"));
}

#[test]
fn all_asset_choices_are_available_noninteractively() {
    let directory = TestDirectory::new();
    let cases = [
        ("real-estate", "kyc", "proportional"),
        ("invoice", "allowlist", "fixed"),
        ("commodity", "permissionless", "tiered"),
        ("debt", "hybrid", "none"),
    ];
    for (index, (asset, compliance, dividend)) in cases.into_iter().enumerate() {
        let name = format!("asset-{index}");
        let output = run(
            &directory,
            &[
                "init",
                &name,
                "--asset-type",
                asset,
                "--compliance-model",
                compliance,
                "--dividend-strategy",
                dividend,
                "--non-interactive",
            ],
        );
        assert_success(&output);
        let source = fs::read_to_string(directory.path().join(&name).join("src/lib.rs"))
            .expect("generated source should be readable");
        assert!(source.contains(asset));
        assert!(source.contains(compliance));
        assert!(source.contains(dividend));
    }
}

#[test]
fn invalid_selection_reports_the_allowed_values() {
    let directory = TestDirectory::new();
    let output = run(
        &directory,
        &[
            "init",
            "invalid-choice",
            "--asset-type",
            "not-an-asset",
            "--compliance-model",
            "kyc",
            "--dividend-strategy",
            "proportional",
            "--non-interactive",
        ],
    );
    assert_failure(&output, "Real Estate, Invoice, Commodity, or Debt");
}
