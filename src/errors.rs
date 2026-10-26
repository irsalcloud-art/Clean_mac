//! Structured errors and process exit codes.

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

/// Exit codes used consistently by every MacClean command.
pub mod exit {
    /// The command completed and everything it attempted succeeded.
    pub const SUCCESS: u8 = 0;
    /// A runtime error prevented the command from completing.
    pub const FAILURE: u8 = 1;
    /// Invalid command-line usage (also used by clap for parse errors).
    pub const USAGE: u8 = 2;
    /// The command completed, but the result is partial: a scan skipped
    /// paths, or a cleanup could not remove some items.
    pub const PARTIAL: u8 = 3;
    /// The user declined the confirmation prompt.
    pub const CANCELLED: u8 = 4;
    /// The operation was refused by the safety policy (for example, no
    /// interactive terminal was available for confirmation).
    pub const REFUSED: u8 = 5;
}

#[derive(Debug, thiserror::Error)]
pub enum MacCleanError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("refused by safety policy: {0}")]
    Policy(String),

    #[error("cancelled by user; nothing was deleted")]
    Cancelled,

    #[error(
        "confirmation requires an interactive terminal (stdin is not a TTY); nothing was deleted"
    )]
    NotInteractive,

    #[error("failed to write output: {0}")]
    Output(#[from] io::Error),

    #[error("failed to serialize JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl MacCleanError {
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        MacCleanError::Io {
            path: path.into(),
            source,
        }
    }

    pub fn exit_code(&self) -> ExitCode {
        ExitCode::from(match self {
            MacCleanError::InvalidArgument(_) => exit::USAGE,
            MacCleanError::Cancelled => exit::CANCELLED,
            MacCleanError::Policy(_) | MacCleanError::NotInteractive => exit::REFUSED,
            MacCleanError::Io { .. } | MacCleanError::Output(_) | MacCleanError::Json(_) => {
                exit::FAILURE
            }
        })
    }
}

pub type Result<T> = std::result::Result<T, MacCleanError>;
