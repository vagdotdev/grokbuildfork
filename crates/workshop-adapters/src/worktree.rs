//! Isolated git worktrees for adapter runs, and patch previews back to the user.
//!
//! A delegated CLI never edits the user's checkout directly. It works in a detached worktree of
//! the same repository; when it finishes, [`Worktree::patch_preview`] shows what changed so the
//! user can import it (or not).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::AdapterError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchPreview {
    /// Unified diff of every change in the worktree, untracked files included.
    pub diff: String,
    /// `git status --porcelain` lines.
    pub status: Vec<String>,
}

impl PatchPreview {
    pub fn is_empty(&self) -> bool {
        self.diff.trim().is_empty() && self.status.is_empty()
    }
}

#[derive(Debug)]
pub struct Worktree {
    repo: PathBuf,
    path: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, AdapterError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| AdapterError::Git(format!("failed to run git {}: {e}", args.join(" "))))?;
    if !out.status.success() {
        return Err(AdapterError::Git(format!(
            "git {} failed ({:?}): {}",
            args.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Worktree {
    /// Create a detached worktree of `repo` at `base` (a ref or commit) under `dest_parent`.
    pub fn create(repo: &Path, base: &str, dest_parent: &Path) -> Result<Self, AdapterError> {
        let toplevel = git(repo, &["rev-parse", "--show-toplevel"])?;
        let repo = PathBuf::from(toplevel.trim());
        std::fs::create_dir_all(dest_parent)?;
        let name = format!(
            "workshop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let path = dest_parent.join(name);
        git(
            &repo,
            &["worktree", "add", "--detach", &path.to_string_lossy(), base],
        )?;
        Ok(Self { repo, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// Diff of everything the run changed, including new files.
    pub fn patch_preview(&self) -> Result<PatchPreview, AdapterError> {
        // Register untracked files so they appear in the diff without staging content.
        git(&self.path, &["add", "--intent-to-add", "--all"])?;
        let diff = git(&self.path, &["diff", "--binary"])?;
        let status = git(&self.path, &["status", "--porcelain"])?
            .lines()
            .map(str::to_string)
            .collect();
        Ok(PatchPreview { diff, status })
    }

    /// Remove the worktree and its registration. Uncommitted changes are discarded; take a
    /// [`Self::patch_preview`] first.
    pub fn remove(self) -> Result<(), AdapterError> {
        git(
            &self.repo,
            &["worktree", "remove", "--force", &self.path.to_string_lossy()],
        )?;
        Ok(())
    }
}

/// True when both paths resolve to the same git working tree — the case Workshop must refuse for a
/// delegated run that is not explicitly acknowledged as in-place.
pub fn same_checkout(a: &Path, b: &Path) -> bool {
    let top = |p: &Path| git(p, &["rev-parse", "--show-toplevel"]).ok().map(|s| s.trim().to_string());
    match (top(a), top(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}
