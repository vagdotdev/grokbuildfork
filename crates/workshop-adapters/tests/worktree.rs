//! The isolation hook: a delegated run gets its own git worktree, the patch
//! can be previewed, and cleanup removes the worktree.

use std::path::Path;
use std::process::Command;

use workshop_adapters::{GitWorktreeIsolation, InPlace, WorkspaceIsolation};

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .expect("git available");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn git_worktree_isolation_prepares_previews_and_cleans_up() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# Demo\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let isolation = GitWorktreeIsolation {
        root: Some(tmp.path().join("worktrees")),
    };
    let ws = isolation.prepare(&repo, "run 1/alpha").unwrap();
    assert_eq!(ws.path(), tmp.path().join("worktrees").join("run-1-alpha"));
    assert!(ws.path().join("README.md").exists());
    assert!(ws.path().join(".git").exists(), "is a linked worktree");

    // The delegated CLI edits and adds files inside the worktree only.
    std::fs::write(ws.path().join("README.md"), "# Demo\nhello\n").unwrap();
    std::fs::write(ws.path().join("new.txt"), "brand new\n").unwrap();
    let patch = ws.patch().unwrap();
    assert!(patch.contains("+hello"), "{patch}");
    assert!(patch.contains("+brand new"), "{patch}");
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "# Demo\n",
        "live checkout untouched"
    );
    assert!(!repo.join("new.txt").exists());

    let path = ws.path().to_path_buf();
    ws.cleanup().unwrap();
    assert!(!path.exists(), "worktree removed");
}

#[test]
fn prepare_outside_a_repo_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let err = GitWorktreeIsolation::default()
        .prepare(tmp.path(), "x")
        .err()
        .unwrap();
    assert!(
        err.to_string().contains("not inside a git repository"),
        "{err}"
    );
}

#[test]
fn in_place_is_an_explicit_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = InPlace.prepare(tmp.path(), "x").unwrap();
    assert_eq!(ws.path(), tmp.path());
    assert_eq!(ws.patch().unwrap(), "");
    ws.cleanup().unwrap();
    assert!(tmp.path().exists());
}
