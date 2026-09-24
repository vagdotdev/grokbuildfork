//! The contract each vendor adapter implements.
//!
//! An adapter is pure data + parsing: it says which flags to pass for a non-interactive run and
//! how to normalize the resulting stream. Who the vendor is, where its CLI lives, whether the
//! binary really is that CLI and whether the user is signed in all come from `workshop-detect` —
//! the one detection stack, shared with the picker — so a turn can never see a different CLI
//! than the picker's vendor row does. An adapter never touches the filesystem or the network
//! itself; the detector and supervisor do all process spawning.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::event::AdapterEvent;

/// The four subscription CLIs Workshop knows how to drive: the detection stack's vendor identity.
pub type AdapterId = workshop_detect::Vendor;

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

/// How much the delegated CLI may do inside its worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Inspect and plan only; no file edits or commands.
    ReadOnly,
    /// Edit files inside the workspace. Command execution stays under the
    /// vendor's own sandbox / approval defaults; nothing is force-approved.
    WorkspaceWrite,
    /// Every tool call is approved: the vendor's own run-everything mode
    /// (`cursor-agent --force`, `claude --permission-mode bypassPermissions`,
    /// `codex -s danger-full-access`), as Grok Build's always-approve.
    AlwaysApprove,
}

/// How the prompt reaches the child process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptDelivery {
    /// Written to the child's stdin, then stdin is closed.
    Stdin,
    /// Appended as the final positional argument; stdin is `/dev/null`.
    Argument,
    /// Written to stdin as the vendor's message lines ([`Adapter::prompt_lines`]); stdin then
    /// stays open as the CLI's control channel — the asks it raises ([`AdapterEvent::PermissionAsk`],
    /// [`AdapterEvent::Question`]) are answered on it ([`Normalizer::reply`]) — and is closed
    /// once the normalizer reaches its terminal event, which ends the CLI.
    Channel,
}

/// The host's answer to an ask the vendor CLI raised on its control channel.
#[derive(Clone, Debug, PartialEq)]
pub enum AskReply {
    /// Approve the tool call asked about in [`AdapterEvent::PermissionAsk`] `id`; `always` also
    /// approves the same kind of call for the rest of the session (the vendor's own rule).
    Allow { id: String, always: bool },
    /// Refuse it; `message` is what the model is told.
    Deny { id: String, message: String },
    /// Answer the [`AdapterEvent::Question`] `id`: one list of chosen labels (or typed text) per
    /// question, in order.
    Answer {
        id: String,
        answers: Vec<Vec<String>>,
    },
}

impl AskReply {
    pub fn id(&self) -> &str {
        match self {
            AskReply::Allow { id, .. }
            | AskReply::Deny { id, .. }
            | AskReply::Answer { id, .. } => id,
        }
    }
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

    /// Queue the stdin line that answers `reply` on the CLI's control channel
    /// ([`PromptDelivery::Channel`]). `false` when this run cannot carry it: the vendor has no
    /// channel, or the ask was not one of its own — the host then falls back (a question's
    /// answers resume the session as the next prompt).
    fn reply(&mut self, reply: &AskReply) -> bool {
        let _ = reply;
        false
    }

    /// Lines the normalizer wants written to the CLI's stdin (answers queued by [`Self::reply`],
    /// refusals of control requests Workshop does not serve). Drained by the supervisor after
    /// every stdout line and every reply.
    fn take_stdin_lines(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// A vendor CLI adapter: the pinned run flags and the stream shape. Pure description and
/// parsing; no I/O. Identity, location and login state are `workshop-detect`'s.
pub trait Adapter: Send + Sync {
    fn id(&self) -> AdapterId;

    fn version_pin(&self) -> VersionPin;

    fn prompt_delivery(&self) -> PromptDelivery;

    /// The stdin lines that carry the prompt under [`PromptDelivery::Channel`] (the vendor's
    /// message framing); unused for the other deliveries.
    fn prompt_lines(&self, prompt: &str) -> Vec<String> {
        vec![prompt.to_string()]
    }

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
