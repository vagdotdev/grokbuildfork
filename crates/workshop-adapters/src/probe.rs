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
    let name = program.display().to_string();
    let build = || {
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
        cmd
    };

    // Own process group, enrolled so session teardown can reap it. `group`
    // must outlive the child. A binary that was written a moment ago (an
    // installer that just finished, a fixture another thread is still
    // closing) can refuse to exec with ETXTBSY for a few milliseconds; that is
    // transient, so try again briefly before reporting it.
    let mut attempt = 0u32;
    let (mut child, group) = loop {
        match xai_tty_utils::global_process_scope().spawn(build()) {
            Ok(spawned) => break spawned,
            Err(source)
                if source.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 5 =>
            {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(20 * u64::from(attempt))).await;
            }
            Err(source) => {
                return Err(ProbeError::Spawn {
                    program: name.clone(),
                    source,
                });
            }
        }
    };
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

    match tokio::time::timeout(timeout, collect).await {
        Ok(Ok((out, err, status))) => Ok(ProbeOutput {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            exit_code: status.code(),
        }),
        Ok(Err(source)) => Err(ProbeError::Io {
            program: name,
            source,
        }),
        Err(_) => {
            let _ = group.kill();
            let _ = child.kill().await;
            Err(ProbeError::Timeout {
                program: name,
                timeout,
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

    /// A script still open for writing cannot be exec'd (`ETXTBSY`); the probe retries briefly
    /// instead of reporting the transient state, so a binary an installer just finished writing
    /// (or a fixture another test thread is still closing) probes fine.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_briefly_busy_executable_is_retried() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("busy.sh");
        std::fs::write(&script, "#!/bin/sh\necho ok\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Hold the file open for writing for a moment, as a writer that has not closed it yet.
        let writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&script)
            .unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            drop(writer);
        });
        let out = run_probe(
            &script,
            &[],
            &BTreeMap::new(),
            None,
            Duration::from_secs(10),
        )
        .await
        .expect("the probe waits out ETXTBSY");
        assert_eq!(out.stdout.trim(), "ok");
        release.await.unwrap();
    }
}
