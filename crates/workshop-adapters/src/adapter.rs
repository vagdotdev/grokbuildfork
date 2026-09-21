//! The contract each vendor adapter implements.
//!
//! An adapter is pure data + parsing: it names the binary, says how to verify
//! identity, how to ask the vendor CLI for its own login state, which flags to
//! pass for a non-interactive run, and how to normalize the resulting stream.
//! It never touches the filesystem or the network itself; the detector and
//! supervisor do all process spawning.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::event::AdapterEvent;

/// The four subscription CLIs Workshop knows how to drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterId {
    Claude,
    Codex,
    Cursor,
    OpenCode,
}

impl AdapterId {
    /// Picker rail order is Claude, Codex, Cursor; OpenCode is an adapter but
    /// not a subscription rail.
    pub const ALL: [AdapterId; 4] = [
        AdapterId::Claude,
        AdapterId::Codex,
        AdapterId::Cursor,
        AdapterId::OpenCode,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AdapterId::Claude => "claude",
            AdapterId::Codex => "codex",
            AdapterId::Cursor => "cursor",
            AdapterId::OpenCode => "opencode",
        }
    }

    /// User-visible product name.
    pub fn display_name(self) -> &'static str {
        match self {
            AdapterId::Claude => "Claude Code",
            AdapterId::Codex => "Codex",
            AdapterId::Cursor => "Cursor",
            AdapterId::OpenCode => "OpenCode",
        }
    }
}

impl std::fmt::Display for AdapterId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Captured output of a short probe invocation (`--version`, status, ...).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeOutput {
    pub stdout: String,
    pub stderr: String,
    /// `None` when the process was killed by a signal or timed out.
    pub exit_code: Option<i32>,
}

impl ProbeOutput {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Inclusive range of vendor CLI versions these pinned flags were verified
/// against. Older versions are refused (fail closed); newer versions run but
/// are reported as untested so drift shows up in the UI instead of silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VersionPin {
    pub min_supported: &'static str,
    pub max_tested: &'static str,
}

/// Where the detected version sits relative to the pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinStatus {
    Tested,
    NewerThanTested,
    OlderThanSupported,
}

impl VersionPin {
    pub fn classify(&self, version: &str) -> PinStatus {
        let v = version_key(version);
        if v < version_key(self.min_supported) {
            PinStatus::OlderThanSupported
        } else if v > version_key(self.max_tested) {
            PinStatus::NewerThanTested
        } else {
            PinStatus::Tested
        }
    }
}

/// Lenient dotted-numeric version key: `2026.09.18-9a7762b` -> `[2026, 9, 18]`,
/// `codex-cli 0.155.1` -> `[0, 155, 1]`. Non-numeric tails are ignored.
pub fn version_key(version: &str) -> Vec<u64> {
    let start = version
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(version.len());
    let mut key = Vec::new();
    for part in version[start..].split('.') {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        match digits.parse::<u64>() {
            Ok(n) => key.push(n),
            Err(_) => break,
        }
        if digits.len() != part.len() {
            break;
        }
    }
    key
}

/// Vendor-reported login state. Workshop never infers this from files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LoginState {
    /// The vendor CLI says it can run. `method` is a fixed, non-secret label
    /// such as `ChatGPT` or `API key`; never raw CLI output.
    Ready { method: Option<String> },
    /// The vendor CLI says it is logged out.
    SignIn,
    /// The probe failed, timed out, or printed something the adapter does not
    /// recognize. The UI shows "Sign in"; the reason is for diagnostics.
    Unknown { reason: String },
}

/// How much the delegated CLI may do inside its worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Inspect and plan only; no file edits or commands.
    ReadOnly,
    /// Edit files inside the workspace. Command execution stays under the
    /// vendor's own sandbox / approval defaults; nothing is force-approved.
    WorkspaceWrite,
}

/// How the prompt reaches the child process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptDelivery {
    /// Written to the child's stdin, then stdin is closed.
    Stdin,
    /// Appended as the final positional argument; stdin is `/dev/null`.
    Argument,
}

/// One delegated, whole-task run.
#[derive(Clone, Debug)]
pub struct RunRequest {
    pub prompt: String,
    /// Working directory for the child. Should be an isolated worktree from
    /// [`crate::worktree::WorkspaceIsolation`], never the user's live checkout.
    pub cwd: PathBuf,
    pub model: Option<String>,
    /// Vendor session id from a previous [`AdapterEvent::Done`] to continue.
    pub resume: Option<String>,
    pub permission: PermissionPolicy,
}

impl RunRequest {
    pub fn new(prompt: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            prompt: prompt.into(),
            cwd: cwd.into(),
            model: None,
            resume: None,
            permission: PermissionPolicy::ReadOnly,
        }
    }
}

/// Why a stream could not be normalized. Always fatal for the run.
#[derive(Debug, thiserror::Error)]
pub enum NormalizeError {
    #[error("stdout line is not JSON (schema drift?): {0}")]
    NotJson(String),
    #[error("unexpected stream shape: {0}")]
    Shape(String),
}

/// How the normalizer saw the run end, independent of the exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminal {
    Completed,
    Failed(String),
}

/// Stateful per-run translator from one vendor's JSONL to [`AdapterEvent`]s.
pub trait Normalizer: Send {
    /// Feed one complete stdout line (without the trailing newline).
    fn on_line(&mut self, line: &str) -> Result<Vec<AdapterEvent>, NormalizeError>;

    /// Called once when stdout closes. `exit_code` is `None` on signal death.
    /// Vendors without an explicit terminal event decide the outcome here.
    fn on_eof(&mut self, exit_code: Option<i32>) -> Vec<AdapterEvent>;

    /// Vendor session id once seen in the stream.
    fn session_id(&self) -> Option<&str>;

    fn terminal(&self) -> Option<&Terminal>;
}

/// A vendor CLI adapter. Pure description and parsing; no I/O.
pub trait Adapter: Send + Sync {
    fn id(&self) -> AdapterId;

    /// Binary names to look for, most specific first.
    fn binary_names(&self) -> &'static [&'static str];

    /// Vendor-specific install dirs beyond the shared known dirs.
    fn extra_install_dirs(&self, home: &Path) -> Vec<PathBuf>;

    /// Argument sets to run (in order) whose outputs prove the binary is this
    /// vendor's CLI. Each is a short, credential-free command.
    fn identity_probes(&self) -> &'static [&'static [&'static str]];

    /// Parse the outputs of [`Self::identity_probes`]; `Some(version)` only
    /// when the binary is positively identified as this vendor's CLI.
    fn identify(&self, outputs: &[ProbeOutput]) -> Option<String>;

    fn version_pin(&self) -> VersionPin;

    /// The vendor's own, documented, non-secret status command.
    fn status_args(&self) -> &'static [&'static str];

    fn interpret_status(&self, output: &ProbeOutput) -> LoginState;

    /// The vendor's interactive login command, to be attached to the user's
    /// terminal. Workshop never captures or parses its output.
    fn login_args(&self) -> &'static [&'static str];

    fn logout_args(&self) -> &'static [&'static str];

    fn prompt_delivery(&self) -> PromptDelivery;

    /// Pinned non-interactive run arguments (without the prompt when
    /// [`Self::prompt_delivery`] is `Argument`; the supervisor appends it).
    fn run_args(&self, req: &RunRequest) -> Vec<String>;

    fn normalizer(&self) -> Box<dyn Normalizer>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_keys_are_lenient() {
        assert_eq!(version_key("2.1.278"), vec![2, 1, 278]);
        assert_eq!(version_key("codex-cli 0.155.1"), vec![0, 155, 1]);
        assert_eq!(version_key("2026.09.18-9a7762b"), vec![2026, 9, 18]);
        assert_eq!(version_key("1.18.31"), vec![1, 18, 31]);
        assert_eq!(version_key("garbage"), Vec::<u64>::new());
    }

    #[test]
    fn pin_classifies_older_tested_newer() {
        let pin = VersionPin {
            min_supported: "2.1.0",
            max_tested: "2.1.278",
        };
        assert_eq!(pin.classify("2.0.99"), PinStatus::OlderThanSupported);
        assert_eq!(pin.classify("2.1.0"), PinStatus::Tested);
        assert_eq!(pin.classify("2.1.278"), PinStatus::Tested);
        assert_eq!(pin.classify("2.1.279"), PinStatus::NewerThanTested);
        assert_eq!(pin.classify("3.0.0"), PinStatus::NewerThanTested);
    }
}
