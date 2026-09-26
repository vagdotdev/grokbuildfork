//! Engine-shell gates (issue 3 + owner task-3 findings F1/F3): the shell Workshop points OpenCode
//! at (`workshop __engine-shell -c <command>`) must behave the way Grok Build's shell tool makes a
//! command behave, where OpenCode's own bash tool does not.
//!
//! Driven against the built binary directly (opt-in via `WORKSHOP_BIN`, run with
//! `--include-ignored`) — no PTY, no engine, so these are fast and hermetic:
//!
//!   * `daemon_command_returns_promptly_and_survives` — a command that leaves a background process
//!     holding the pipe (the AppImage/daemon and `xdg-open`→GUI shapes) ends the call as soon as
//!     the direct child returns, and the detached process is *not* killed. This is the F1 fix: the
//!     window a tool call opens stays up after the call ends.
//!   * `hung_command_is_cut_off_and_child_survives` — a command that blocks with no output past the
//!     budget is handed back with one clear marker line (so the turn continues, issue 3), and the
//!     still-running child is left alive, not killed.
//!   * `ordinary_command_streams_output_and_exit_code` — a normal command streams its output and
//!     returns its own exit code.
//!   * `sigterm_kills_the_command_group` — a cancelled turn / OpenCode abort (SIGTERM to the shim)
//!     kills the command's process group.

#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin_from_env() -> Option<PathBuf> {
    let bin = PathBuf::from(std::env::var_os("WORKSHOP_BIN")?);
    if !bin.is_file() {
        eprintln!("WORKSHOP_BIN={} is not a file; skipping", bin.display());
        return None;
    }
    Some(bin)
}

/// The stable marker the shim prints when it backgrounds a still-running command; mirrors
/// `workshop_engine_shell::BACKGROUND_MARKER_KEY` (kept in step by the shim's own unit test).
const BACKGROUND_MARKER_KEY: &str = "left running in the background";

struct ShimRun {
    code: i32,
    stdout: String,
    elapsed: Duration,
}

/// Run `workshop __engine-shell -c <command>` with an isolated `WORKSHOP_HOME`, capturing stdout.
fn run_shim(bin: &Path, home: &Path, command: &str, budget_ms: Option<u64>) -> ShimRun {
    let mut cmd = Command::new(bin);
    cmd.args(["__engine-shell", "-c", command])
        .env("WORKSHOP_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(ms) = budget_ms {
        cmd.env("WORKSHOP_ENGINE_SHELL_BUDGET_MS", ms.to_string());
    }
    let start = Instant::now();
    let out = cmd.output().expect("run engine-shell");
    ShimRun {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        elapsed: start.elapsed(),
    }
}

fn pgrep(marker: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", marker])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn pkill(marker: &str) {
    let _ = Command::new("pkill").args(["-9", "-f", marker]).status();
}

fn wait_until(mut f: impl FnMut() -> bool, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f()
}

/// A command that spawns a background process (holding the call's output) and returns must end the
/// call at once, and the process it left behind must survive — the daemon and the `xdg-open`→window
/// shapes (finding F1). Under OpenCode's own bash tool this hangs to the timeout and then the
/// window is killed with the group.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn daemon_command_returns_promptly_and_survives() {
    let Some(bin) = bin_from_env() else { return };
    let home = tempfile::tempdir().expect("home");
    let marker = format!("workshop-daemon-probe-{}", std::process::id());
    // A GUI/daemon stand-in: the direct child launches a detached long-lived process (renamed so we
    // can find it) that inherits the command's output, then returns — like `xdg-open file.html`.
    let command = format!("( exec -a {marker} sleep 120 & ) ; echo opened");

    let run = run_shim(&bin, home.path(), &command, None);

    assert!(
        run.elapsed < Duration::from_secs(10),
        "the call returned when the direct child exited, not at a timeout: took {:?}",
        run.elapsed
    );
    assert_eq!(run.code, 0, "the command itself succeeded");
    assert!(
        run.stdout.contains("opened"),
        "the command's output reached the model: {:?}",
        run.stdout
    );
    assert!(
        !run.stdout.contains(BACKGROUND_MARKER_KEY),
        "the direct child returned, so this is not a backgrounded call: {:?}",
        run.stdout
    );
    assert!(
        pgrep(&marker),
        "the launched process must stay up after the call ends (F1); it was killed"
    );
    pkill(&marker);
}

/// A command that itself blocks with no output past the budget is handed back with one marker line
/// so the turn continues (issue 3), and the still-running command is left alive (Grok Build's
/// background move), not killed.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn hung_command_is_cut_off_and_child_survives() {
    let Some(bin) = bin_from_env() else { return };
    let home = tempfile::tempdir().expect("home");
    let marker = format!("workshop-hung-probe-{}", std::process::id());
    // Prints, then blocks forever with no further output; `exec -a` renames the blocker so we can
    // confirm it is left running rather than killed.
    let command = format!("echo begin; exec -a {marker} sleep 120");

    let run = run_shim(&bin, home.path(), &command, Some(1500));

    assert!(
        run.elapsed < Duration::from_secs(10),
        "the watchdog handed the turn back near the budget, not at a timeout: took {:?}",
        run.elapsed
    );
    assert_eq!(
        run.code, 0,
        "backgrounding returns success so the turn continues"
    );
    assert!(
        run.stdout.contains("begin"),
        "what was captured before the budget reached the model: {:?}",
        run.stdout
    );
    assert!(
        run.stdout.contains(BACKGROUND_MARKER_KEY),
        "one clear marker line says the command was left running: {:?}",
        run.stdout
    );
    assert!(
        pgrep(&marker),
        "a backgrounded command is left running, not killed"
    );
    pkill(&marker);
}

/// The ordinary case is untouched: output streams through and the command's own exit code is
/// returned.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn ordinary_command_streams_output_and_exit_code() {
    let Some(bin) = bin_from_env() else { return };
    let home = tempfile::tempdir().expect("home");

    let ok = run_shim(&bin, home.path(), "echo hello world", None);
    assert_eq!(ok.code, 0);
    assert!(ok.stdout.contains("hello world"), "{:?}", ok.stdout);

    let fail = run_shim(&bin, home.path(), "echo to-stderr >&2; exit 7", None);
    assert_eq!(fail.code, 7, "the command's own exit code is returned");
    assert!(
        fail.stdout.contains("to-stderr"),
        "stderr is merged into the streamed output: {:?}",
        fail.stdout
    );
}

/// A cancelled turn (OpenCode aborts by killing the shim's group with SIGTERM) must take the
/// command's process group with it.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn sigterm_kills_the_command_group() {
    let Some(bin) = bin_from_env() else { return };
    let home = tempfile::tempdir().expect("home");
    let marker = format!("workshop-cancel-probe-{}", std::process::id());
    let command = format!("echo started; exec -a {marker} sleep 120");

    // reaped by wait() below; deliberately unenrolled (we SIGTERM it to prove the group is killed)
    #[allow(clippy::zombie_processes, clippy::disallowed_methods)]
    let mut child = Command::new(&bin)
        .args(["__engine-shell", "-c", &command])
        .env("WORKSHOP_HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn engine-shell");

    // Wait until the blocking command is up under the shim.
    assert!(
        wait_until(|| pgrep(&marker), 5),
        "the command should be running under the shim"
    );

    // SIGTERM the shim, as OpenCode's abort does to its shell's group.
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();
    // Drain the pipe so the child handle is clean.
    if let Some(mut out) = child.stdout.take() {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
    }

    assert!(
        wait_until(|| !pgrep(&marker), 5),
        "SIGTERM to the shim must kill the command's process group"
    );
    pkill(&marker);
}
