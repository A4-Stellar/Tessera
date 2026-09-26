use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum CliError {
    InvalidName {
        name: String,
        reason: String,
    },
    InvalidChoice {
        field: &'static str,
        value: String,
        choices: &'static str,
    },
    MissingNonInteractive {
        field: &'static str,
    },
    InvalidOutputDirectory {
        path: PathBuf,
        reason: String,
    },
    DestinationExists(PathBuf),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    PromptIo {
        source: io::Error,
    },
    Prompt(String),
    Template(String),
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName { name, reason } => {
                write!(formatter, "invalid project name '{name}': {reason}")
            }
            Self::InvalidChoice {
                field,
                value,
                choices,
            } => write!(
                formatter,
                "invalid {field} '{value}': choose one of {choices}"
            ),
            Self::MissingNonInteractive { field } => write!(
                formatter,
                "--non-interactive requires --{field}; use --defaults for standard values"
            ),
            Self::InvalidOutputDirectory { path, reason } => {
                write!(
                    formatter,
                    "invalid output directory '{}': {reason}",
                    path.display()
                )
            }
            Self::DestinationExists(path) => {
                write!(
                    formatter,
                    "destination '{}' already exists; refusing to overwrite",
                    path.display()
                )
            }
            Self::Io { path, source } => {
                write!(formatter, "I/O error for '{}': {source}", path.display())
            }
            Self::PromptIo { source } => {
                write!(formatter, "interactive prompt I/O error: {source}")
            }
            Self::Prompt(message) => write!(formatter, "interactive prompt failed: {message}"),
            Self::Template(message) => write!(formatter, "template error: {message}"),
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::PromptIo { source } => Some(source),
            _ => None,
        }
    }
}
