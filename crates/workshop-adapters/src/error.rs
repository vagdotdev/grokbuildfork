use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use workshop_detect::Vendor;

/// Why a run did not complete. Serializable so the TUI and usage ledger can show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum FailureReason {
    /// No verified vendor binary was found.
    NotInstalled { vendor: Vendor },
    /// The binary's version is outside the pinned support range; flags are not trusted.
    UnsupportedVersion { vendor: Vendor, version: String, supported: String },
    /// The process could not be started.
    SpawnFailed { detail: String },
    /// The CLI reported an error through its own event stream (auth, model, turn failure).
    VendorError { detail: String },
    /// The process exited without a recognized terminal event, or emitted output that is not the
    /// documented JSON stream. Workshop fails closed rather than guessing.
    SchemaDrift { detail: String },
    /// A single output line exceeded the configured bound.
    OutputBound { max_line_bytes: usize },
    /// No output for longer than the idle timeout.
    IdleTimeout { secs: u64 },
    /// The whole run exceeded its time budget.
    Timeout { secs: u64 },
    /// The process exited non-zero without a vendor error event.
    NonZeroExit { code: Option<i32> },
    /// The caller cancelled the run.
    Cancelled,
}

impl std::fmt::Display for FailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FailureReason::NotInstalled { vendor } => write!(f, "{} is not installed", vendor.display_name()),
            FailureReason::UnsupportedVersion { vendor, version, supported } => write!(
                f,
                "{} {} is outside the supported range ({})",
                vendor.display_name(),
                version,
                supported
            ),
            FailureReason::SpawnFailed { detail } => write!(f, "could not start the CLI: {detail}"),
            FailureReason::VendorError { detail } => write!(f, "the CLI reported an error: {detail}"),
            FailureReason::SchemaDrift { detail } => write!(f, "unexpected CLI output: {detail}"),
            FailureReason::OutputBound { max_line_bytes } => {
                write!(f, "CLI output line exceeded {max_line_bytes} bytes")
            }
            FailureReason::IdleTimeout { secs } => write!(f, "no output for {secs}s"),
            FailureReason::Timeout { secs } => write!(f, "run exceeded {secs}s"),
            FailureReason::NonZeroExit { code } => write!(f, "CLI exited with {code:?}"),
            FailureReason::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Errors surfaced before a run can produce an outcome (bad request, worktree problems).
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("working directory {0} does not exist")]
    MissingWorkdir(PathBuf),
    #[error("prompt is empty")]
    EmptyPrompt,
    #[error("environment: {0}")]
    Env(#[from] workshop_detect::env::CredentialInEnv),
    #[error("git: {0}")]
    Git(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
