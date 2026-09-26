use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::CliError;
use crate::options::ProjectChoices;

const LIB_TEMPLATE: &str = include_str!("../templates/lib.rs.tmpl");
const TEST_TEMPLATE: &str = include_str!("../templates/contract-test.rs.tmpl");
const CARGO_TEMPLATE: &str = include_str!("../templates/Cargo.toml.tmpl");
const TOOLCHAIN_TEMPLATE: &str = include_str!("../templates/rust-toolchain.toml.tmpl");
const README_TEMPLATE: &str = include_str!("../templates/README.md.tmpl");
const GITIGNORE_TEMPLATE: &str = include_str!("../templates/gitignore.tmpl");

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectConfig {
    pub package_name: String,
    pub crate_name: String,
    pub contract_name: String,
    pub symbol: String,
    pub choices: ProjectChoices,
}

#[derive(Clone, Debug)]
struct RenderedFile {
    relative_path: &'static str,
    contents: String,
}

impl ProjectConfig {
    pub fn new(name: &str, choices: ProjectChoices) -> Result<Self, CliError> {
        let package_name = validate_project_name(name)?;
        let contract_base = pascal_case(&package_name);
        Ok(Self {
            crate_name: package_name.replace('-', "_"),
            contract_name: format!("{contract_base}Contract"),
            symbol: make_symbol(&package_name),
            package_name,
            choices,
        })
    }
}

pub fn generate_project(config: &ProjectConfig, output_root: &Path) -> Result<PathBuf, CliError> {
    validate_project_name(&config.package_name)?;
    let output_root = validate_output_root(output_root)?;
    let destination = output_root.join(&config.package_name);
    ensure_destination_is_absent(&destination)?;

    let files = render_files(config)?;
    fs::create_dir(&destination).map_err(|source| io_error(&destination, source))?;

    let mut created_files = Vec::new();
    let mut created_directories = Vec::new();
    let result = write_files(
        &destination,
        &files,
        &mut created_files,
        &mut created_directories,
    );

    if let Err(error) = result {
        cleanup(&destination, &created_files, &created_directories);
        return Err(error);
    }
    Ok(destination)
}

pub fn validate_project_name(name: &str) -> Result<String, CliError> {
    let invalid = |reason: &str| CliError::InvalidName {
        name: name.to_owned(),
        reason: reason.to_owned(),
    };
    if name.is_empty() {
        return Err(invalid("name cannot be empty"));
    }
    if name.len() > 64 {
        return Err(invalid("name must be 64 ASCII characters or fewer"));
    }
    if !name.as_bytes()[0].is_ascii_alphabetic() {
        return Err(invalid("name must start with an ASCII letter"));
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(invalid(
            "use only ASCII letters, digits, hyphens, and underscores",
        ));
    }
    if is_windows_reserved_name(name) {
        return Err(invalid("name is reserved on Windows"));
    }
    if is_reserved_crate_name(name) {
        return Err(invalid("name becomes a reserved Rust identifier"));
    }
    Ok(name.to_owned())
}

fn is_windows_reserved_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let base = lower.split('.').next().unwrap_or(&lower);
    matches!(base, "con" | "prn" | "aux" | "nul")
        || base
            .strip_prefix("com")
            .or_else(|| base.strip_prefix("lpt"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
}

fn is_reserved_crate_name(name: &str) -> bool {
    matches!(
        name.replace('-', "_").as_str(),
        "abstract"
            | "as"
            | "async"
            | "await"
            | "become"
            | "box"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "do"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "final"
            | "fn"
            | "for"
            | "gen"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "macro"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "override"
            | "priv"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "try"
            | "type"
            | "typeof"
            | "union"
            | "unsafe"
            | "unsized"
            | "use"
            | "virtual"
            | "where"
            | "while"
            | "yield"
    )
}

fn validate_output_root(output_root: &Path) -> Result<PathBuf, CliError> {
    if output_root.as_os_str().is_empty() {
        return Err(CliError::InvalidOutputDirectory {
            path: output_root.to_owned(),
            reason: "path cannot be empty".to_owned(),
        });
    }
    if output_root.to_string_lossy().contains('\0') {
        return Err(CliError::InvalidOutputDirectory {
            path: output_root.to_owned(),
            reason: "path contains a NUL byte".to_owned(),
        });
    }
    let metadata = fs::symlink_metadata(output_root).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            CliError::InvalidOutputDirectory {
                path: output_root.to_owned(),
                reason: "directory does not exist".to_owned(),
            }
        } else {
            io_error(output_root, source)
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(CliError::InvalidOutputDirectory {
            path: output_root.to_owned(),
            reason: "symlinked output directories are not accepted".to_owned(),
        });
    }
    if !metadata.is_dir() {
        return Err(CliError::InvalidOutputDirectory {
            path: output_root.to_owned(),
            reason: "path is not a directory".to_owned(),
        });
    }
    fs::canonicalize(output_root).map_err(|source| io_error(output_root, source))
}

fn ensure_destination_is_absent(destination: &Path) -> Result<(), CliError> {
    match fs::symlink_metadata(destination) {
        Ok(_) => Err(CliError::DestinationExists(destination.to_owned())),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(destination, source)),
    }
}

fn render_files(config: &ProjectConfig) -> Result<Vec<RenderedFile>, CliError> {
    let asset_type = config.choices.asset_type;
    let compliance_model = config.choices.compliance_model;
    let dividend_strategy = config.choices.dividend_strategy;
    let values = vec![
        ("package_name", config.package_name.clone()),
        ("crate_name", config.crate_name.clone()),
        ("contract_name", config.contract_name.clone()),
        ("symbol", config.symbol.clone()),
        ("asset_type", asset_type.slug().to_owned()),
        ("asset_type_label", asset_type.label().to_owned()),
        ("compliance_model", compliance_model.slug().to_owned()),
        (
            "compliance_model_label",
            compliance_model.label().to_owned(),
        ),
        ("dividend_strategy", dividend_strategy.slug().to_owned()),
        (
            "dividend_strategy_label",
            dividend_strategy.label().to_owned(),
        ),
        (
            "package_description",
            format!(
                "Standalone Soroban {} RWA asset contract.",
                asset_type.label()
            ),
        ),
        (
            "expected_dividend",
            dividend_strategy.preview_for(50, 25, 100).to_string(),
        ),
    ];

    Ok(vec![
        RenderedFile {
            relative_path: "Cargo.toml",
            contents: render(CARGO_TEMPLATE, &values, "Cargo.toml")?,
        },
        RenderedFile {
            relative_path: "rust-toolchain.toml",
            contents: render(TOOLCHAIN_TEMPLATE, &values, "rust-toolchain.toml")?,
        },
        RenderedFile {
            relative_path: "README.md",
            contents: render(README_TEMPLATE, &values, "README.md")?,
        },
        RenderedFile {
            relative_path: ".gitignore",
            contents: render(GITIGNORE_TEMPLATE, &values, ".gitignore")?,
        },
        RenderedFile {
            relative_path: "src/lib.rs",
            contents: render(LIB_TEMPLATE, &values, "src/lib.rs")?,
        },
        RenderedFile {
            relative_path: "tests/contract.rs",
            contents: render(TEST_TEMPLATE, &values, "tests/contract.rs")?,
        },
    ])
}

fn render(
    template: &str,
    values: &[(&str, String)],
    template_name: &str,
) -> Result<String, CliError> {
    let mut rendered = template.to_owned();
    for (key, value) in values {
        let placeholder = format!("{{{{{key}}}}}");
        rendered = rendered.replace(&placeholder, value);
    }
    if rendered.contains("{{") {
        return Err(CliError::Template(format!(
            "{template_name} contains an unresolved placeholder"
        )));
    }
    Ok(rendered)
}

fn write_files(
    destination: &Path,
    files: &[RenderedFile],
    created_files: &mut Vec<PathBuf>,
    created_directories: &mut Vec<PathBuf>,
) -> Result<(), CliError> {
    for relative in ["src", "tests"] {
        let directory = destination.join(relative);
        fs::create_dir(&directory).map_err(|source| io_error(&directory, source))?;
        created_directories.push(directory);
    }
    for file in files {
        let path = destination.join(file.relative_path);
        let mut handle = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        created_files.push(path.clone());
        handle
            .write_all(file.contents.as_bytes())
            .map_err(|source| io_error(&path, source))?;
    }
    Ok(())
}

fn cleanup(destination: &Path, files: &[PathBuf], directories: &[PathBuf]) {
    for file in files.iter().rev() {
        let _ = fs::remove_file(file);
    }
    for directory in directories.iter().rev() {
        let _ = fs::remove_dir(directory);
    }
    let _ = fs::remove_dir(destination);
}

fn io_error(path: &Path, source: std::io::Error) -> CliError {
    CliError::Io {
        path: path.to_owned(),
        source,
    }
}

fn pascal_case(value: &str) -> String {
    let mut result = String::new();
    let mut capitalize = true;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if capitalize {
                result.push(character.to_ascii_uppercase());
                capitalize = false;
            } else {
                result.push(character.to_ascii_lowercase());
            }
        } else {
            capitalize = true;
        }
    }
    if result.is_empty() {
        "Asset".to_owned()
    } else {
        result
    }
}

fn make_symbol(value: &str) -> String {
    let mut symbol = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_uppercase())
        .collect::<String>();
    if symbol.is_empty() {
        symbol.push_str("RWA");
    }
    symbol.truncate(12);
    symbol
}
