//! One-keypress install of a vendor CLI.
//!
//! A rail whose official CLI is not installed offers one action, `Install`. Enter runs that
//! vendor's *official* installer — never a Workshop re-implementation of it — with its output
//! streamed line by line to the caller (for one status line and a log under the Workshop home)
//! and nothing else: no install ever starts on its own, and the vendor's own sign-in follows.
//!
//! The installers contact only the vendor's hosts (and, for Codex, the npm registry). This crate
//! touches no files here: the caller keeps the log (gate:no-theft, `tests/no_theft.rs`).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

use crate::model::Vendor;

/// Test hook (never printed): a program run as `<program> <vendor-id>` in place of the official
/// installer, so a gate can prove the flow with a fake that writes a fake CLI onto `PATH`.
pub const INSTALLER_ENV: &str = "WORKSHOP_RAIL_INSTALLER";

/// An installer that prints nothing for this long is treated as hung.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The vendor's documented one-line installer, as the vendor publishes it.
pub fn official_install_command(vendor: Vendor) -> Option<&'static str> {
    match vendor {
        Vendor::Claude => Some("curl -fsSL https://claude.ai/install.sh | bash"),
        Vendor::Codex => Some("npm install -g @openai/codex"),
        Vendor::Cursor => Some("curl https://cursor.com/install -fsS | bash"),
        // The engine is installed by Workshop itself on the first message.
        Vendor::OpenCode => None,
    }
}

/// The shell command this run executes for `vendor`: the official installer, or the test hook.
pub fn install_command(vendor: Vendor) -> Option<String> {
    if let Some(hook) = std::env::var_os(INSTALLER_ENV).filter(|v| !v.is_empty()) {
        let hook = hook.to_string_lossy();
        return Some(format!("'{}' {}", hook.replace('\'', "'\\''"), vendor.id()));
    }
    official_install_command(vendor).map(str::to_owned)
}

/// Where one vendor's installer output belongs: `<workshop home>/logs/install-<vendor>.log`
/// (the caller writes it).
pub fn install_log_path(workshop_home: &Path, vendor: Vendor) -> PathBuf {
    workshop_home
        .join("logs")
        .join(format!("install-{}.log", vendor.id()))
}

/// What one run of the installer reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Exit status 0.
    Installed,
    /// A non-zero exit; the caller names the log.
    Failed { status: Option<i32> },
    /// Nothing printed for the whole timeout: the process group was killed.
    Hung,
}

/// Run `command` (an [`install_command`]) through `sh -c` to completion (blocking; run it off
/// the UI thread).
///
/// stdin is closed; every stdout/stderr line is handed to `line` as it arrives (ANSI stripped,
/// raw text second so a log can keep it); the child is its own process group so a timeout
/// kills whatever it started. Only ever called from the user's Enter on an `Install` rail.
pub fn run_installer(
    command: &str,
    timeout: Duration,
    mut line: impl FnMut(&str),
) -> Result<InstallOutcome, String> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[allow(
        clippy::disallowed_methods,
        reason = "the installer is its own process group, waited on with a hard timeout, and killed by group on expiry"
    )]
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start `sh -c {command}`: {e}"))?;

    // Both pipes feed one channel so the caller sees lines in arrival order.
    let (tx, rx) = mpsc::channel::<String>();
    let readers: Vec<std::thread::JoinHandle<()>> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|pipe| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for text in BufReader::new(pipe).lines().map_while(Result::ok) {
                let _ = tx.send(text);
            }
        })
    })
    .collect();
    drop(tx);

    let started = Instant::now();
    let mut last_output = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(text) => {
                last_output = Instant::now();
                let shown = crate::process::strip_ansi(&text);
                let shown = shown.trim();
                if !shown.is_empty() {
                    line(shown);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_output.elapsed() > timeout || started.elapsed() > timeout {
                    kill_group(&mut child);
                    let _ = child.wait();
                    for r in readers {
                        let _ = r.join();
                    }
                    return Ok(InstallOutcome::Hung);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    for r in readers {
        let _ = r.join();
    }
    let status = child
        .wait_timeout(Duration::from_secs(30))
        .map_err(|e| format!("waiting for the installer: {e}"))?;
    let Some(status) = status else {
        kill_group(&mut child);
        let _ = child.wait();
        return Ok(InstallOutcome::Hung);
    };
    Ok(if status.success() {
        InstallOutcome::Installed
    } else {
        InstallOutcome::Failed {
            status: status.code(),
        }
    })
}

fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // SAFETY: killpg with the pid of a child we spawned as its own group leader; failure
        // (already gone) is harmless.
        unsafe {
            libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_commands_are_the_vendors_own_and_the_engine_has_none() {
        assert_eq!(
            official_install_command(Vendor::Claude),
            Some("curl -fsSL https://claude.ai/install.sh | bash")
        );
        assert_eq!(
            official_install_command(Vendor::Codex),
            Some("npm install -g @openai/codex")
        );
        assert_eq!(
            official_install_command(Vendor::Cursor),
            Some("curl https://cursor.com/install -fsS | bash")
        );
        assert_eq!(official_install_command(Vendor::OpenCode), None);
        for v in [Vendor::Claude, Vendor::Codex, Vendor::Cursor] {
            let c = official_install_command(v).unwrap();
            assert!(
                !c.contains("x.ai") && !c.contains("grok"),
                "an installer never points at the other product: {c}"
            );
        }
        assert!(
            install_log_path(Path::new("/h"), Vendor::Claude).ends_with("logs/install-claude.log")
        );
    }

    #[test]
    fn a_silent_installer_is_given_up_on() {
        let outcome = run_installer("sleep 30", Duration::from_millis(600), |_| {}).unwrap();
        assert_eq!(outcome, InstallOutcome::Hung);
    }

    #[test]
    fn exit_status_is_reported() {
        let mut seen = Vec::new();
        let ok = run_installer(
            "echo one; echo two >&2; exit 0",
            Duration::from_secs(30),
            |l| seen.push(l.to_owned()),
        )
        .unwrap();
        assert_eq!(ok, InstallOutcome::Installed);
        seen.sort();
        assert_eq!(seen, ["one", "two"]);
        let failed = run_installer("exit 3", Duration::from_secs(30), |_| {}).unwrap();
        assert_eq!(failed, InstallOutcome::Failed { status: Some(3) });
    }
}
