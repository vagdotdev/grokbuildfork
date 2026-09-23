//! Short, credential-free child invocations (`--version`, status commands).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use crate::adapter::ProbeOutput;

/// Output larger than this is truncated; probes are expected to print a few
/// lines at most.
pub const PROBE_OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("failed to spawn `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{program}` did not exit within {timeout:?}")]
    Timeout { program: String, timeout: Duration },
    #[error("i/o error while probing `{program}`: {source}")]
    Io {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{program}` was stopped: {reason}")]
    Stopped { program: String, reason: String },
}

/// Run `program args...` with the given environment, `/dev/null` stdin, and a
/// hard timeout. On timeout the process group is killed.
pub async fn run_probe(
    program: &Path,
    args: &[&str],
    env: &BTreeMap<OsString, OsString>,
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<ProbeOutput, ProbeError> {
    run_probe_until(program, args, env, cwd, timeout, std::future::pending()).await
}

/// [`run_probe`] that also ends early when `stop` resolves (a stall watchdog, say): the process
/// group is killed and the reason `stop` returned is reported as [`ProbeError::Stopped`].
pub async fn run_probe_until(
    program: &Path,
    args: &[&str],
    env: &BTreeMap<OsString, OsString>,
    cwd: Option<&Path>,
    timeout: Duration,
    stop: impl std::future::Future<Output = String>,
) -> Result<ProbeOutput, ProbeError> {
    let name = program.display().to_string();
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    crate::env::apply(&mut cmd, env);

    // Own process group, enrolled so session teardown can reap it. `group`
    // must outlive the child.
    let (mut child, group) =
        xai_tty_utils::global_process_scope()
            .spawn(cmd)
            .map_err(|source| ProbeError::Spawn {
                program: name.clone(),
                source,
            })?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    let collect = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut stdout = (&mut stdout).take(PROBE_OUTPUT_LIMIT as u64);
        let mut stderr = (&mut stderr).take(PROBE_OUTPUT_LIMIT as u64);
        let (a, b) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        a?;
        b?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((out, err, status))
    };

    let result = tokio::select! {
        r = tokio::time::timeout(timeout, collect) => r.map_err(|_| None),
        reason = stop => Err(Some(reason)),
    };
    match result {
        Ok(Ok((out, err, status))) => Ok(ProbeOutput {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            exit_code: status.code(),
        }),
        Ok(Err(source)) => Err(ProbeError::Io {
            program: name,
            source,
        }),
        Err(stopped) => {
            let _ = group.kill();
            let _ = child.kill().await;
            Err(match stopped {
                Some(reason) => ProbeError::Stopped {
                    program: name,
                    reason,
                },
                None => ProbeError::Timeout {
                    program: name,
                    timeout,
                },
            })
        }
    }
}

/// Remove ANSI escape sequences so status text can be matched reliably.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_sequences() {
        assert_eq!(
            strip_ansi("\u{1b}[90m~/x/auth.json\u{1b}[0m 0 credentials"),
            "~/x/auth.json 0 credentials"
        );
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
