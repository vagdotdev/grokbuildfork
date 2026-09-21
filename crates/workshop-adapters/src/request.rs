use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use workshop_detect::Vendor;

use crate::worktree::Worktree;

/// What the delegated CLI may do to the working directory.
///
/// Both profiles deny unsandboxed shell for CLIs that have no sandbox of their own; shell is
/// allowed only where the vendor sandboxes it (Codex `--sandbox workspace-write`, Cursor
/// `--sandbox enabled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProfile {
    /// Read and search only; edits are proposed, never applied.
    #[default]
    ReadOnly,
    /// Edits inside the working directory are applied; Workshop imports them as a patch preview.
    WorkspaceWrite,
}

/// A directory the adapter may run in. Constructed from an isolated [`Worktree`], or explicitly
/// acknowledged as in-place (tests, or a user who opted out of isolation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workdir {
    path: PathBuf,
    isolated: bool,
}

impl Workdir {
    pub fn from_worktree(worktree: &Worktree) -> Self {
        Self {
            path: worktree.path().to_path_buf(),
            isolated: true,
        }
    }

    /// Run directly in `path`. The caller takes responsibility for not editing the same checkout
    /// concurrently with Workshop.
    pub fn in_place_acknowledged(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            isolated: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_isolated(&self) -> bool {
        self.isolated
    }
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub vendor: Vendor,
    pub prompt: String,
    pub workdir: Workdir,
    /// Vendor model id (`--model`); `None` uses the CLI's configured default.
    pub model: Option<String>,
    /// Vendor session id from a previous [`crate::RunOutcome::session_id`] to continue.
    pub resume: Option<String>,
    pub permissions: PermissionProfile,
    /// Claude only (`--max-turns`); other CLIs have no documented equivalent.
    pub max_turns: Option<u32>,
    /// Whole-run budget.
    pub timeout: Duration,
    /// Kill the run if the CLI prints nothing for this long.
    pub idle_timeout: Duration,
    /// A single stdout line longer than this fails the run (fail closed on runaway output).
    pub max_line_bytes: usize,
    /// Extra child environment. Fixtures only; credential-like names are rejected.
    pub extra_env: Vec<(OsString, OsString)>,
}

impl RunRequest {
    pub fn new(vendor: Vendor, prompt: impl Into<String>, workdir: Workdir) -> Self {
        Self {
            vendor,
            prompt: prompt.into(),
            workdir,
            model: None,
            resume: None,
            permissions: PermissionProfile::ReadOnly,
            max_turns: None,
            timeout: Duration::from_secs(30 * 60),
            idle_timeout: Duration::from_secs(5 * 60),
            max_line_bytes: 4 * 1024 * 1024,
            extra_env: Vec::new(),
        }
    }

    pub fn with_resume(mut self, session_id: impl Into<String>) -> Self {
        self.resume = Some(session_id.into());
        self
    }

    pub fn with_permissions(mut self, permissions: PermissionProfile) -> Self {
        self.permissions = permissions;
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}
