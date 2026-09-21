//! Bounded, timed execution of a vendor CLI probe command.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

/// Maximum bytes kept from each of stdout and stderr. Probe output is tiny; anything larger is
/// truncated rather than buffered without bound.
pub const MAX_CAPTURE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildOutput {
    /// Exit code; `None` when killed by a signal or by the timeout.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl ChildOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("failed to spawn {0}: {1}")]
    Spawn(String, #[source] std::io::Error),
    #[error("failed while waiting for {0}: {1}")]
    Wait(String, #[source] std::io::Error),
}

fn drain(mut reader: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() < MAX_CAPTURE_BYTES {
                        let take = n.min(MAX_CAPTURE_BYTES - buf.len());
                        buf.extend_from_slice(&chunk[..take]);
                    }
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Run `bin args…` with a cleared environment replaced by `env`, no stdin, and a hard timeout.
///
/// On Unix the child is placed in its own process group so a timeout kills any grandchildren too.
pub fn run(
    bin: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(OsString, OsString)],
    timeout: Duration,
) -> Result<ChildOutput, RunError> {
    let label = format!("{} {}", bin.display(), args.join(" "));
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "probe child is its own process group, waited on with a hard timeout, and killed by group on expiry"
    )]
    let mut child = cmd.spawn().map_err(|e| RunError::Spawn(label.clone(), e))?;
    let stdout = drain(child.stdout.take().expect("stdout piped"));
    let stderr = drain(child.stderr.take().expect("stderr piped"));

    let (code, timed_out) = match child.wait_timeout(timeout) {
        Ok(Some(status)) => (status.code(), false),
        Ok(None) => {
            kill_group(&mut child);
            let _ = child.wait();
            (None, true)
        }
        Err(e) => {
            kill_group(&mut child);
            let _ = child.wait();
            return Err(RunError::Wait(label, e));
        }
    };

    Ok(ChildOutput {
        code,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
        timed_out,
    })
}

fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child is its own group leader (process_group(0)), so its pid is the pgid.
        // SAFETY: killpg with a pid we spawned and still own; failure is harmless (ESRCH).
        unsafe {
            libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Strip ANSI escape sequences (CSI and simple two-byte escapes) from CLI output.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: parameter bytes 0x30–0x3F, intermediates 0x20–0x2F, final 0x40–0x7E.
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC … BEL or ESC \
                chars.next();
                let mut prev = '\0';
                for next in chars.by_ref() {
                    if next == '\u{7}' || (prev == '\u{1b}' && next == '\\') {
                        break;
                    }
                    prev = next;
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc() {
        assert_eq!(
            strip_ansi("\u{1b}[91m\u{1b}[1mError: \u{1b}[0mSession"),
            "Error: Session"
        );
        assert_eq!(
            strip_ansi("\u{1b}[0m\n┌  Credentials \u{1b}[90m~/x"),
            "\n┌  Credentials ~/x"
        );
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}rest"), "rest");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_child_and_reports() {
        let out = run(
            Path::new("/bin/sh"),
            &["-c", "sleep 30"],
            None,
            &[],
            Duration::from_millis(200),
        )
        .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.code, None);
    }

    #[cfg(unix)]
    #[test]
    fn captures_output_and_exit_code() {
        let out = run(
            Path::new("/bin/sh"),
            &["-c", "printf out; printf err >&2; exit 3"],
            None,
            &[],
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout, "out");
        assert_eq!(out.stderr, "err");
        assert!(!out.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn environment_is_exactly_what_was_passed() {
        let env = vec![(OsString::from("ONLY_THIS"), OsString::from("1"))];
        let out = run(
            Path::new("/usr/bin/env"),
            &[],
            None,
            &env,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.stdout.trim(), "ONLY_THIS=1");
    }
}
