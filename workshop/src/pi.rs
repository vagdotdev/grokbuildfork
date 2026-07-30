use anyhow::{Context, Result, bail};
use command_group::CommandGroup;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_ERROR_BYTES: usize = 4 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct PiHarness {
    binary: PathBuf,
}

impl Default for PiHarness {
    fn default() -> Self {
        Self {
            binary: std::env::var_os("WORKSHOP_PI_BINARY")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("pi")),
        }
    }
}

impl PiHarness {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn is_available(&self) -> bool {
        self.run(&["--version"], VERSION_TIMEOUT)
            .is_ok_and(|output| output.status.success())
    }

    pub fn bearer_token(&self, provider: &str, model: &str) -> Result<String> {
        self.run_print_command("print-bearer-token", provider, model, true)
    }

    pub fn api_key(&self, provider: &str, model: &str) -> Result<String> {
        self.run_print_command("print-api-key", provider, model, false)
    }

    fn run_print_command(
        &self,
        subcommand: &str,
        provider: &str,
        model: &str,
        minimum_expiry: bool,
    ) -> Result<String> {
        if provider.trim().is_empty() || model.trim().is_empty() {
            bail!("Pi provider and model are required");
        }
        let mut arguments = vec!["auth", subcommand, "--provider", provider, "--model", model];
        if minimum_expiry {
            arguments.extend(["--min-expiry", "30m"]);
        }
        let output = self.run(&arguments, COMMAND_TIMEOUT)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "Pi could not provide {provider} credentials (exit {}): {}",
                output.status,
                stderr.trim()
            );
        }
        let token = std::str::from_utf8(&output.stdout)
            .context("Pi credential output was not UTF-8")?
            .trim();
        if token.is_empty() {
            bail!("Pi returned an empty credential for {provider}");
        }
        if token.contains('\n') || token.contains('\r') || token.contains('\0') {
            bail!("Pi credential output contained unexpected control characters");
        }
        Ok(token.to_owned())
    }

    fn run(&self, arguments: &[&str], timeout: Duration) -> Result<CapturedOutput> {
        let binary = command_binary(&self.binary);
        let mut command = Command::new(&binary);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.group_spawn().with_context(|| {
            format!(
                "run Pi credential helper at {}; install Pi or set WORKSHOP_PI_BINARY",
                binary.display()
            )
        })?;
        let (stdout, stderr) = {
            let inner = child.inner();
            (
                inner.stdout.take().context("capture Pi stdout")?,
                inner.stderr.take().context("capture Pi stderr")?,
            )
        };
        let (stdout_sender, stdout_reader) = std::sync::mpsc::sync_channel(1);
        let (stderr_sender, stderr_reader) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = stdout_sender.send(read_capped(stdout, MAX_TOKEN_BYTES));
        });
        std::thread::spawn(move || {
            let _ = stderr_sender.send(read_capped(stderr, MAX_ERROR_BYTES));
        });

        let deadline = std::time::Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .context("wait for Pi credential helper process group")?
            {
                break status;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "Pi credential helper timed out after {} seconds",
                    timeout.as_secs()
                );
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
        };
        let mut killed_group = false;
        let (stdout, stdout_truncated) = receive_output(
            stdout_reader,
            deadline,
            &mut child,
            &mut killed_group,
            "stdout",
        )?;
        let (mut stderr, stderr_truncated) = receive_output(
            stderr_reader,
            deadline,
            &mut child,
            &mut killed_group,
            "stderr",
        )?;
        if stdout_truncated {
            bail!("Pi credential output exceeded {MAX_TOKEN_BYTES} bytes");
        }
        if stderr_truncated {
            stderr.extend_from_slice(b"\n[stderr truncated]");
        }
        Ok(CapturedOutput {
            status,
            stdout,
            stderr,
        })
    }
}

fn receive_output(
    receiver: std::sync::mpsc::Receiver<std::io::Result<(Vec<u8>, bool)>>,
    deadline: std::time::Instant,
    child: &mut command_group::GroupChild,
    killed_group: &mut bool,
    stream: &str,
) -> Result<(Vec<u8>, bool)> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    match receiver.recv_timeout(remaining) {
        Ok(result) => return result.with_context(|| format!("read Pi {stream}")),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            bail!("Pi {stream} reader stopped unexpectedly")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
    }

    if !*killed_group {
        *killed_group = true;
        let _ = child.kill();
        let _ = child.wait();
    }
    match receiver.recv_timeout(Duration::from_secs(1)) {
        Ok(result) => result.with_context(|| format!("read Pi {stream} after process-group kill")),
        Err(_) => bail!("Pi {stream} remained open after its process group was killed"),
    }
}

#[derive(Debug)]
struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn read_capped(mut reader: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::with_capacity(limit.min(8192));
    (&mut reader)
        .take((limit + 1) as u64)
        .read_to_end(&mut output)?;
    let truncated = output.len() > limit;
    output.truncate(limit);
    std::io::copy(&mut reader, &mut std::io::sink())?;
    Ok((output, truncated))
}

#[cfg(windows)]
fn command_binary(binary: &Path) -> PathBuf {
    if binary.components().count() == 1 && binary.extension().is_none() {
        PathBuf::from(format!("{}.cmd", binary.display()))
    } else {
        binary.to_owned()
    }
}

#[cfg(not(windows))]
fn command_binary(binary: &Path) -> PathBuf {
    binary.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn helper(script: &str) -> (tempfile::TempDir, PiHarness) {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pi");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        (temp, PiHarness::new(path))
    }

    #[cfg(unix)]
    #[test]
    fn reads_only_the_raw_pi_token() {
        let (_temp, pi) = helper("#!/bin/sh\nprintf 'pi-token\\n'\n");
        assert_eq!(
            pi.bearer_token("anthropic", "claude-test").unwrap(),
            "pi-token"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_multiline_output() {
        let (_temp, pi) = helper("#!/bin/sh\nprintf 'token\\nextra\\n'\n");
        assert!(
            pi.bearer_token("anthropic", "claude-test")
                .unwrap_err()
                .to_string()
                .contains("control characters")
        );
    }

    #[cfg(unix)]
    #[test]
    fn reports_pi_failure_without_panicking() {
        let (_temp, pi) = helper("#!/bin/sh\necho 'not logged in' >&2\nexit 2\n");
        let error = pi
            .bearer_token("anthropic", "claude-test")
            .unwrap_err()
            .to_string();
        assert!(error.contains("not logged in"));
    }

    #[cfg(unix)]
    #[test]
    fn kills_a_hung_pi_process() {
        let (_temp, pi) = helper("#!/bin/sh\nexec sleep 5\n");
        let started = std::time::Instant::now();

        let error = pi
            .run(&["auth"], Duration::from_millis(20))
            .unwrap_err()
            .to_string();

        assert!(error.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
