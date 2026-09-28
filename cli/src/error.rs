use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("invalid project name: {reason}")]
    InvalidName {
        name: String,
        reason: String,
    },

    #[error("invalid choice for {field}: {value} (choose from: {choices})")]
    InvalidChoice {
        field: String,
        value: String,
        choices: String,
    },

    #[error("missing required selection for {field} in non-interactive mode")]
    MissingNonInteractive {
        field: String,
    },

    #[error("prompt failed: {0}")]
    Prompt(String),

    #[error("prompt I/O error")]
    PromptIo {
        #[from]
        source: std::io::Error,
    },

    #[error("template error in {0}")]
    Template(String),

    #[error("I/O error on {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("destination already exists: {0}")]
    DestinationExists(PathBuf),

    #[error("invalid output directory: {reason}")]
    InvalidOutputDirectory {
        path: PathBuf,
        reason: String,
    },
}