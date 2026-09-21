//! Workspace isolation for delegated runs.
//!
//! A delegated CLI never edits the user's live checkout. It gets its own git
//! worktree; the caller previews the resulting patch before importing it.
//! Workshop and the child must not edit the same checkout concurrently.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    #[error("`{0}` is not inside a git repository")]
    NotARepo(PathBuf),
    #[error("git {args:?} failed: {stderr}")]
    Git { args: Vec<String>, stderr: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Prepares an isolated working directory for one run.
pub trait WorkspaceIsolation: Send + Sync {
    fn prepare(&self, repo: &Path, run_id: &str) -> Result<IsolatedWorkspace, IsolationError>;
}

/// A directory a delegated CLI may work in, plus how to clean it up.
pub struct IsolatedWorkspace {
    path: PathBuf,
    repo: Option<PathBuf>,
    remove_on_cleanup: bool,
}

impl IsolatedWorkspace {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Unified diff of everything the run changed relative to the worktree's
    /// base commit, including new files. Empty when nothing changed.
    pub fn patch(&self) -> Result<String, IsolationError> {
        if self.repo.is_none() {
            return Ok(String::new());
        }
        // Intent-to-add makes untracked files visible to `git diff HEAD`.
        // The worktree is disposable, so touching its index is fine.
        git(&self.path, &["add", "--intent-to-add", "--all", "."])?;
        git(&self.path, &["diff", "--binary", "HEAD"])
    }

    /// Remove the worktree. Safe to call on an in-place workspace (no-op).
    pub fn cleanup(self) -> Result<(), IsolationError> {
        let Some(repo) = &self.repo else {
            return Ok(());
        };
        if !self.remove_on_cleanup {
            return Ok(());
        }
        let path = self.path.to_string_lossy().into_owned();
        git(repo, &["worktree", "remove", "--force", &path])?;
        let _ = git(repo, &["worktree", "prune"]);
        Ok(())
    }
}

/// Default isolation: `git worktree add --detach <root>/<run_id> HEAD`.
#[derive(Clone, Debug, Default)]
pub struct GitWorktreeIsolation {
    /// Where worktrees are created. `None` uses `<repo>/.workshop/worktrees`.
    pub root: Option<PathBuf>,
}

impl WorkspaceIsolation for GitWorktreeIsolation {
    fn prepare(&self, repo: &Path, run_id: &str) -> Result<IsolatedWorkspace, IsolationError> {
        let toplevel = git(repo, &["rev-parse", "--show-toplevel"])
            .map_err(|_| IsolationError::NotARepo(repo.to_path_buf()))?;
        let toplevel = PathBuf::from(toplevel.trim());
        let root = self
            .root
            .clone()
            .unwrap_or_else(|| toplevel.join(".workshop").join("worktrees"));
        std::fs::create_dir_all(&root)?;
        let path = root.join(sanitize(run_id));
        let path_str = path.to_string_lossy().into_owned();
        git(
            &toplevel,
            &["worktree", "add", "--detach", &path_str, "HEAD"],
        )?;
        Ok(IsolatedWorkspace {
            path,
            repo: Some(toplevel),
            remove_on_cleanup: true,
        })
    }
}

/// Explicit opt-out: run directly in `repo`. Only for callers that already
/// hold an exclusive, disposable checkout (tests, CI sandboxes).
#[derive(Clone, Copy, Debug, Default)]
pub struct InPlace;

impl WorkspaceIsolation for InPlace {
    fn prepare(&self, repo: &Path, _run_id: &str) -> Result<IsolatedWorkspace, IsolationError> {
        Ok(IsolatedWorkspace {
            path: repo.to_path_buf(),
            repo: None,
            remove_on_cleanup: false,
        })
    }
}

fn sanitize(run_id: &str) -> String {
    let s: String = run_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if s.is_empty() { "run".to_string() } else { s }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, IsolationError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !output.status.success() {
        return Err(IsolationError::Git {
            args: args.iter().map(|s| s.to_string()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
