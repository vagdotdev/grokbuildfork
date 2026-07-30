use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_ERROR_BYTES: usize = 4 * 1024;

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
        Command::new(&self.binary)
            .arg("--version")
            .output()
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
        let mut command = Command::new(&self.binary);
        command
            .arg("auth")
            .arg(subcommand)
            .arg("--provider")
            .arg(provider)
            .arg("--model")
            .arg(model);
        if minimum_expiry {
            command.arg("--min-expiry").arg("30m");
        }
        let output = command.output().with_context(|| {
            format!(
                "run Pi credential helper at {}; install Pi or set WORKSHOP_PI_BINARY",
                self.binary.display()
            )
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(
                &output.stderr[..output.stderr.len().min(MAX_ERROR_BYTES)],
            );
            bail!(
                "Pi could not provide {provider} credentials (exit {}): {}",
                output.status,
                stderr.trim()
            );
        }
        if output.stdout.len() > MAX_TOKEN_BYTES {
            bail!("Pi credential output exceeded {MAX_TOKEN_BYTES} bytes");
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
}

