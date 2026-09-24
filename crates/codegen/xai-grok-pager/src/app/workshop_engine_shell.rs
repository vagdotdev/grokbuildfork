//! Workshop: the shell the OpenCode engine runs its commands through, so a blocking command or a
//! GUI the model launches behaves the way Grok Build's shell tool makes it behave.
//!
//! OpenCode's own bash tool waits for the command's output pipe to reach EOF and, on its timeout,
//! kills the whole process group. Both hurt on Linux (the test platform):
//!
//!   * a command that leaves a background process holding the pipe (an AppImage's FUSE daemon, a
//!     `foo &` daemon) never reaches EOF, so the tool call — and the turn — hang until the timeout
//!     fires minutes later (fresh-eyes review issue 3);
//!   * a GUI the model launches from a tool call (`xdg-open file.html`) inherits that pipe, so the
//!     window is killed with the group when the tool times out — 30 s after the user saw it open,
//!     while the answer said it was open (owner task-3 finding F1).
//!
//! Grok Build moves a foreground command that blocks past a budget to the background and carries
//! on, and never kills a process that has detached. The engine cannot be changed, but the shell it
//! runs commands through can: Workshop points OpenCode's `shell` at `workshop __engine-shell`,
//! which
//!
//!   1. runs the command in its own session (`setsid`, via [`xai_tty_utils::detach_std_command`])
//!      with its stdout/stderr going to a capture file, not the pipe OpenCode reads — so a daemon
//!      or GUI the command leaves behind never holds that pipe open, and is never in the shim's
//!      (or OpenCode's) process group;
//!   2. streams the capture file to OpenCode and returns as soon as the *direct* child exits, so a
//!      command that spawns a daemon and returns (or launches a GUI and returns) ends the call at
//!      once, the window still up;
//!   3. if the direct child itself blocks with no output for the budget, stops waiting, prints one
//!      marker line, and returns — leaving the child running detached (Grok Build's background
//!      move), so the turn continues instead of hanging;
//!   4. on `SIGINT`/`SIGTERM`/`SIGHUP` (a cancelled turn, or OpenCode's own abort), kills the
//!      command's process group before exiting.
//!
//! The marker line ([`BACKGROUND_MARKER`]) is what `workshop_tools` looks for to badge the tool
//! row (finding F3: today a cut-off call shows nothing).

use std::ffi::{OsStr, OsString};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The one line the shim prints when it stops waiting on a still-running command and leaves it in
/// the background. Human-readable for the model, and the stable marker `workshop_tools` matches to
/// badge the row (see [`is_background_marker`]).
pub const BACKGROUND_MARKER: &str = "[workshop] still running after the wait budget — left running in the background; \
     the turn continues.";

/// The stable substring of [`BACKGROUND_MARKER`] used to recognise it in a tool's output.
pub const BACKGROUND_MARKER_KEY: &str = "left running in the background";

/// True when a command's output carries the shim's background marker.
pub fn is_background_marker(output: &str) -> bool {
    output.contains(BACKGROUND_MARKER_KEY)
}

/// How long the direct child may block with no output before the shim backgrounds it. Grok Build's
/// foreground block budget is 15 s; the review asks for ~15–30 s. Overridable for tests.
const DEFAULT_BUDGET: Duration = Duration::from_secs(25);
const BUDGET_ENV: &str = "WORKSHOP_ENGINE_SHELL_BUDGET_MS";

fn budget() -> Duration {
    std::env::var(BUDGET_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_BUDGET)
}

/// The paths Workshop points the engine at: the directory holding the `sudo` shim (prepended to
/// the engine's `PATH`) and the engine-shell script (OpenCode's `shell`).
pub struct EngineShims {
    /// `$WORKSHOP_HOME/bin/shims` — prepend to the engine's `PATH` so the `sudo` shim wins.
    pub shims_dir: PathBuf,
    /// `$WORKSHOP_HOME/bin/engine-shell` — the value for OpenCode's `shell` config.
    pub shell: PathBuf,
    /// Whether a `sudo` shim was written (a real `sudo` was found to wrap).
    pub has_sudo_shim: bool,
}

impl EngineShims {
    /// The engine `PATH` with the shims directory first, so `sudo` resolves to the shim. `base` is
    /// normally the current process `PATH` (the engine inherits it).
    pub fn path_with_shims(&self, base: &OsStr) -> OsString {
        let mut path = self.shims_dir.clone().into_os_string();
        if !base.is_empty() {
            path.push(":");
            path.push(base);
        }
        path
    }
}

/// Write the `sudo` shim and the engine-shell script under `$WORKSHOP_HOME/bin`, returning their
/// paths. `None` (logged by the caller via the returned `Err`) when the home cannot hold them, in
/// which case the engine simply runs without them, as it does today.
pub fn prepare(home: &Path) -> std::io::Result<EngineShims> {
    let bin = home.join("bin");
    let shims_dir = bin.join("shims");
    std::fs::create_dir_all(&shims_dir)?;
    let exe = std::env::current_exe()?;

    let shell = bin.join("engine-shell");
    write_script(
        &shell,
        &format!(
            "#!/bin/sh\n# Workshop's engine shell: runs OpenCode's commands with a background \
             budget and detaches what they leave behind.\nexec '{}' __engine-shell \"$@\"\n",
            shell_quote(&exe.to_string_lossy()),
        ),
    )?;

    // `sudo -A` (Grok Build's `alias sudo='sudo -A'` equivalent): sudo only falls back to
    // SUDO_ASKPASS on its own when DISPLAY is set, and the engine's environment need not have it,
    // so force the askpass path. Skip silently when there is no real sudo to wrap.
    let has_sudo_shim = match real_sudo(&shims_dir) {
        Some(sudo) => {
            write_script(
                &shims_dir.join("sudo"),
                &format!(
                    "#!/bin/sh\n# Workshop: force sudo's askpass helper (Grok Build's `sudo -A`).\n\
                     exec '{}' -A \"$@\"\n",
                    shell_quote(&sudo.to_string_lossy()),
                ),
            )?;
            true
        }
        None => false,
    };

    Ok(EngineShims {
        shims_dir,
        shell,
        has_sudo_shim,
    })
}

fn write_script(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// `'` is the only character `'…'` cannot hold; close, escape, reopen. Paths under `$WORKSHOP_HOME`
/// rarely contain one, but a home with an apostrophe must not break the script.
fn shell_quote(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// The real `sudo` to wrap: the first `sudo` on `PATH` that is not our own shims directory, else
/// the conventional locations. `None` when there is no sudo to wrap.
fn real_sudo(shims_dir: &Path) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir == shims_dir {
                continue;
            }
            let candidate = dir.join("sudo");
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    for fixed in ["/usr/bin/sudo", "/bin/sudo", "/usr/local/bin/sudo"] {
        let candidate = PathBuf::from(fixed);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

// ── The `workshop __engine-shell -c <command>` process ──────────────────────────────────────────

static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_sig: libc::c_int) {
    TERMINATED.store(true, Ordering::SeqCst);
}

/// Dispatch for `workshop __engine-shell …` (OpenCode invokes it as `<shell> -c <command>`).
/// `None` when this is an ordinary launch.
pub fn maybe_run_helper() -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args.get(1).map(OsString::as_os_str) != Some(OsStr::new("__engine-shell")) {
        return None;
    }
    let command = extract_command(&args[2..]);
    Some(run(&command))
}

/// The command from OpenCode's `<shell> -c <command>` invocation: the argument after `-c`. Lenient
/// (last argument, else joined) so an unexpected shape still runs something rather than nothing.
fn extract_command(rest: &[OsString]) -> String {
    if let Some(pos) = rest.iter().position(|a| a == OsStr::new("-c"))
        && let Some(cmd) = rest.get(pos + 1)
    {
        return cmd.to_string_lossy().into_owned();
    }
    rest.last()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn run(command: &str) -> i32 {
    if command.trim().is_empty() {
        return 0;
    }
    match run_with_watchdog(command) {
        Ok(code) => code,
        // Setting up the capture file failed: fall back to a plain shell so the command still runs
        // (without the watchdog/detach), rather than failing the call.
        Err(_) => exec_plain(command),
    }
}

fn run_with_watchdog(command: &str) -> std::io::Result<i32> {
    let path = capture_path();
    let sink = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;

    let mut cmd = std::process::Command::new(user_shell());
    cmd.arg("-c").arg(command);
    cmd.stdin(xai_tty_utils::null_stdio());
    cmd.stdout(sink.try_clone()?);
    cmd.stderr(sink);
    // `setsid`: the command runs in its own session, so a daemon or GUI it leaves behind survives
    // this shim's exit and is out of OpenCode's process group (its timeout kill cannot reach it).
    xai_tty_utils::detach_std_command(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };

    // The command's own group, for a cancel/abort kill (setsid ⇒ pgid == child pid).
    let mut group = xai_tty_utils::ProcessGroup::new().ok();
    if let Some(group) = group.as_mut() {
        let _ = group.attach_std(&child);
    }
    install_signal_handlers();

    let mut reader = std::fs::File::open(&path)?;
    let mut offset: u64 = 0;
    let mut out = std::io::stdout().lock();
    let mut last_output = Instant::now();
    let budget = budget();
    let poll = Duration::from_millis(100);

    let code = loop {
        if pump(&mut reader, &mut offset, &mut out) {
            last_output = Instant::now();
        }

        if TERMINATED.load(Ordering::SeqCst) {
            if let Some(group) = &group {
                let _ = group.terminate();
            }
            let _ = xai_tty_utils::wait_child_bounded(&mut child, Duration::from_secs(3));
            pump(&mut reader, &mut offset, &mut out);
            let _ = std::fs::remove_file(&path);
            break 130;
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                pump(&mut reader, &mut offset, &mut out);
                // The direct child is gone. Anything it detached (setsid, or writing to the
                // capture file's inode) lives on; the file is unlinked but its fd stays valid.
                let _ = std::fs::remove_file(&path);
                break exit_code(status);
            }
            Ok(None) => {}
            Err(_) => break 0,
        }

        if last_output.elapsed() >= budget {
            // The child is still blocking with no output: hand the turn back, as Grok Build does.
            // The child keeps running, detached; its output keeps appending to the (now unlinked)
            // capture file, which nothing reads — the model has what there was.
            pump(&mut reader, &mut offset, &mut out);
            let _ = writeln!(out, "\n{BACKGROUND_MARKER}");
            let _ = out.flush();
            break 0;
        }

        std::thread::sleep(poll);
    };
    Ok(code)
}

/// Copy any bytes appended since `offset` to `out`; returns whether anything was copied.
fn pump(reader: &mut std::fs::File, offset: &mut u64, out: &mut impl Write) -> bool {
    if reader.seek(SeekFrom::Start(*offset)).is_err() {
        return false;
    }
    let mut buf = [0u8; 16 * 1024];
    let mut wrote = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if out.write_all(&buf[..n]).is_ok() {
                    *offset += n as u64;
                    wrote = true;
                }
            }
            Err(_) => break,
        }
    }
    if wrote {
        let _ = out.flush();
    }
    wrote
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(code) = status.code() {
            return code;
        }
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    #[cfg(not(unix))]
    {
        if let Some(code) = status.code() {
            return code;
        }
    }
    0
}

/// Fallback when the capture file cannot be created: replace this process with a plain shell so the
/// command still runs (no watchdog, no detach). Never returns on success.
fn exec_plain(command: &str) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(user_shell())
            .arg("-c")
            .arg(command)
            .exec();
        eprintln!("workshop engine-shell: {err}");
        127
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new(user_shell())
            .arg("-c")
            .arg(command)
            .status()
            .map(|s| s.code().unwrap_or(1))
            .unwrap_or(127)
    }
}

fn install_signal_handlers() {
    #[cfg(unix)]
    unsafe {
        let handler = on_terminate as *const () as libc::sighandler_t;
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::signal(sig, handler);
        }
    }
}

/// The shell the command runs under. `bash` is what OpenCode's models write for; fall back to
/// `/bin/sh` where it is absent.
fn user_shell() -> PathBuf {
    for shell in ["/bin/bash", "/usr/bin/bash"] {
        if Path::new(shell).exists() {
            return PathBuf::from(shell);
        }
    }
    PathBuf::from("/bin/sh")
}

/// A private capture file for one command's output. Under `$WORKSHOP_HOME/run` when available (kept
/// off the user's project), else the temp dir; unique per invocation.
fn capture_path() -> PathBuf {
    let name = format!(
        "engine-shell-{}-{}.out",
        std::process::id(),
        Instant::now().elapsed().as_nanos() ^ (std::process::id() as u128)
    );
    if let Some(home) = std::env::var_os("WORKSHOP_HOME") {
        let run = PathBuf::from(home).join("run");
        if std::fs::create_dir_all(&run).is_ok() {
            return run.join(name);
        }
    }
    std::env::temp_dir().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_command_after_dash_c() {
        let argv = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(extract_command(&argv(&["-c", "echo hi"])), "echo hi");
        // Lenient shapes still yield something to run.
        assert_eq!(extract_command(&argv(&["echo hi"])), "echo hi");
        assert_eq!(extract_command(&argv(&[])), "");
    }

    #[test]
    fn background_marker_is_recognised() {
        assert!(is_background_marker(&format!("out\n{BACKGROUND_MARKER}\n")));
        assert!(!is_background_marker("ordinary output"));
    }

    #[test]
    fn path_with_shims_prepends_the_dir() {
        let shims = EngineShims {
            shims_dir: PathBuf::from("/home/u/.workshop/bin/shims"),
            shell: PathBuf::from("/home/u/.workshop/bin/engine-shell"),
            has_sudo_shim: true,
        };
        assert_eq!(
            shims.path_with_shims(OsStr::new("/usr/bin:/bin")),
            OsString::from("/home/u/.workshop/bin/shims:/usr/bin:/bin")
        );
        assert_eq!(
            shims.path_with_shims(OsStr::new("")),
            OsString::from("/home/u/.workshop/bin/shims")
        );
    }
}
