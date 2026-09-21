//! Isolated worktree + patch preview, and an adapter run that stays inside the worktree.

#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use workshop_adapters::worktree::same_checkout;
use workshop_adapters::{RunRequest, RunStatus, Supervisor, SupervisorConfig, Vendor, Workdir, Worktree};
use workshop_detect::DetectConfig;

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("README.md"), "# hello\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

#[test]
fn worktree_isolates_changes_and_previews_them() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);

    let wt = Worktree::create(&repo, "HEAD", &tmp.path().join("worktrees")).unwrap();
    assert!(wt.path().join("README.md").exists());
    assert!(!same_checkout(&repo, wt.path()), "a worktree is not the user's checkout");
    assert!(wt.patch_preview().unwrap().is_empty());

    // The delegated run edits inside the worktree only.
    std::fs::write(wt.path().join("README.md"), "# hello\nchanged\n").unwrap();
    std::fs::write(wt.path().join("new.txt"), "brand new\n").unwrap();
    let preview = wt.patch_preview().unwrap();
    assert!(preview.diff.contains("+changed"), "{}", preview.diff);
    assert!(preview.diff.contains("+brand new"), "untracked files are part of the preview: {}", preview.diff);
    assert_eq!(preview.status.len(), 2, "{:?}", preview.status);

    // The user's checkout is untouched.
    assert_eq!(std::fs::read_to_string(repo.join("README.md")).unwrap(), "# hello\n");
    assert!(!repo.join("new.txt").exists());
    assert_eq!(git(&repo, &["status", "--porcelain"]).trim(), "");

    let path = wt.path().to_path_buf();
    wt.remove().unwrap();
    assert!(!path.exists());
    assert!(!git(&repo, &["worktree", "list"]).contains(&*path.to_string_lossy()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adapter_runs_inside_the_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);
    let wt = Worktree::create(&repo, "HEAD", &tmp.path().join("worktrees")).unwrap();

    let fixtures: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bin");
    let state = tempfile::tempdir().unwrap();
    let mut detect = DetectConfig::hermetic(fixtures.as_os_str().to_owned(), state.path().join("home"));
    detect.timeout = Duration::from_secs(10);
    let sup = Supervisor::new(SupervisorConfig { detect, ..SupervisorConfig::default() });

    let workdir = Workdir::from_worktree(&wt);
    assert!(workdir.is_isolated());
    let mut req = RunRequest::new(Vendor::Codex, "make a change", workdir);
    req.extra_env.push((OsString::from("FAKE_CLI_STATE_DIR"), state.path().as_os_str().to_owned()));

    let (tx, mut rx) = mpsc::channel(64);
    let outcome = sup.run(req, tx, CancellationToken::new()).await;
    while rx.recv().await.is_some() {}
    assert_eq!(outcome.status, RunStatus::Completed, "{outcome:#?}");

    let cwd = std::fs::read_to_string(state.path().join("run-cwd.codex")).unwrap();
    assert_eq!(Path::new(cwd.trim()), dunce::canonicalize(wt.path()).unwrap());
    // Codex saw a git checkout, so the git-repo check was not skipped.
    let args = std::fs::read_to_string(state.path().join("run-args.codex")).unwrap();
    assert!(!args.contains("--skip-git-repo-check"), "{args}");
    assert!(args.lines().any(|l| l == wt.path().to_string_lossy()), "-C points at the worktree: {args}");
    wt.remove().unwrap();
}
