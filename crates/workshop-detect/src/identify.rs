//! Verify that a candidate executable really is the vendor CLI it is named after.
//!
//! Signatures are taken from the CLIs' own `--version` / `--help` output (verified 2026-09-21
//! against Claude Code 2.1.278, Codex CLI 0.155.1, Cursor Agent 2026.09.18, OpenCode 1.18.31):
//!
//! | Vendor   | `--version`                      | extra signal (`--help`)          |
//! |----------|----------------------------------|----------------------------------|
//! | Claude   | `2.1.278 (Claude Code)`          | —                                |
//! | Codex    | `codex-cli 0.155.1`              | —                                |
//! | Cursor   | `2026.09.18-9a7762b` (date-hash) | help mentions `Cursor Agent`     |
//! | OpenCode | `1.18.31` (bare semver)          | help lists `opencode <command>`  |
//!
//! Cursor and OpenCode print no product name in `--version`, so both signals are required for them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::locate::DetectConfig;
use crate::model::Vendor;
use crate::process::{self, ChildOutput};

/// A verified vendor binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub vendor: Vendor,
    pub path: PathBuf,
    /// Version string as the CLI printed it, trimmed to the version token.
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentifyError {
    #[error("{path}: could not run --version: {reason}")]
    Unrunnable { path: PathBuf, reason: String },
    #[error("{path}: --version timed out")]
    TimedOut { path: PathBuf },
    #[error("{path}: --version exited with {code:?}")]
    NonZero { path: PathBuf, code: Option<i32> },
    #[error("{path}: not {vendor}: --version printed {version_output:?}")]
    NotVendor {
        path: PathBuf,
        vendor: &'static str,
        version_output: String,
    },
}

fn cursor_version_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{4}\.\d{2}\.\d{2}-[0-9a-f]{6,}$").expect("valid regex"))
}

fn semver_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^v?\d+\.\d+\.\d+(?:[-+.][0-9A-Za-z.-]+)?$").expect("valid regex")
    })
}

fn first_line(s: &str) -> &str {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

/// Parse `--version` output for `vendor`. Returns the version token, or `None` when the output does
/// not carry the vendor's signature. Pure function; see [`identify`] for the process wrapper.
pub fn version_from_output(vendor: Vendor, version_output: &str) -> Option<String> {
    let clean = process::strip_ansi(version_output);
    let line = first_line(&clean);
    match vendor {
        Vendor::Claude => {
            // "2.1.278 (Claude Code)"
            let (ver, rest) = line.split_once(' ')?;
            rest.trim()
                .eq_ignore_ascii_case("(Claude Code)")
                .then(|| ver.to_string())
        }
        Vendor::Codex => {
            // "codex-cli 0.155.1"
            let rest = line.strip_prefix("codex-cli ")?;
            let ver = rest.split_whitespace().next()?;
            semver_re().is_match(ver).then(|| ver.to_string())
        }
        Vendor::Cursor => cursor_version_re().is_match(line).then(|| line.to_string()),
        Vendor::OpenCode => semver_re().is_match(line).then(|| line.to_string()),
    }
}

/// Second signal from `--help` for vendors whose `--version` is anonymous.
pub fn help_matches(vendor: Vendor, help_output: &str) -> bool {
    let help = process::strip_ansi(help_output);
    match vendor {
        Vendor::Claude | Vendor::Codex => true,
        Vendor::Cursor => help.contains("Cursor Agent"),
        Vendor::OpenCode => help
            .lines()
            .any(|l| l.trim_start().starts_with("opencode ")),
    }
}

fn needs_help_signal(vendor: Vendor) -> bool {
    matches!(vendor, Vendor::Cursor | Vendor::OpenCode)
}

fn run_probe(
    path: &Path,
    args: &[&str],
    cfg: &DetectConfig,
    env: &[(OsString, OsString)],
) -> Result<ChildOutput, IdentifyError> {
    process::run(path, args, None, env, cfg.timeout).map_err(|e| IdentifyError::Unrunnable {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

/// Run `path --version` (and `--help` where needed) and confirm the binary is `vendor`.
pub fn identify(
    vendor: Vendor,
    path: &Path,
    cfg: &DetectConfig,
) -> Result<Identity, IdentifyError> {
    let env = crate::env::minimal_env(&cfg.extra_env).map_err(|e| IdentifyError::Unrunnable {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    let out = run_probe(path, &["--version"], cfg, &env)?;
    if out.timed_out {
        return Err(IdentifyError::TimedOut {
            path: path.to_path_buf(),
        });
    }
    if out.code != Some(0) {
        return Err(IdentifyError::NonZero {
            path: path.to_path_buf(),
            code: out.code,
        });
    }
    let combined = if out.stdout.trim().is_empty() {
        out.stderr.clone()
    } else {
        out.stdout.clone()
    };
    let not_vendor = || IdentifyError::NotVendor {
        path: path.to_path_buf(),
        vendor: vendor.display_name(),
        version_output: first_line(&combined).chars().take(120).collect(),
    };
    let version = version_from_output(vendor, &combined).ok_or_else(not_vendor)?;

    if needs_help_signal(vendor) {
        let help = run_probe(path, &["--help"], cfg, &env)?;
        if help.timed_out {
            return Err(IdentifyError::TimedOut {
                path: path.to_path_buf(),
            });
        }
        let text = format!("{}\n{}", help.stdout, help.stderr);
        if !help_matches(vendor, &text) {
            return Err(not_vendor());
        }
    }

    Ok(Identity {
        vendor,
        path: path.to_path_buf(),
        version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_version_strings_are_recognised() {
        assert_eq!(
            version_from_output(Vendor::Claude, "2.1.278 (Claude Code)\n"),
            Some("2.1.278".into())
        );
        assert_eq!(
            version_from_output(Vendor::Codex, "codex-cli 0.155.1\n"),
            Some("0.155.1".into())
        );
        assert_eq!(
            version_from_output(Vendor::Cursor, "2026.09.18-9a7762b\n"),
            Some("2026.09.18-9a7762b".into())
        );
        assert_eq!(
            version_from_output(Vendor::OpenCode, "1.18.31\n"),
            Some("1.18.31".into())
        );
    }

    #[test]
    fn impostors_are_rejected() {
        // A generic `agent` binary is not Cursor.
        assert_eq!(version_from_output(Vendor::Cursor, "agent 1.0.0"), None);
        assert_eq!(version_from_output(Vendor::Cursor, "1.2.3"), None);
        // Wrong vendor for the name.
        assert_eq!(
            version_from_output(Vendor::Claude, "codex-cli 0.155.1"),
            None
        );
        assert_eq!(
            version_from_output(Vendor::Codex, "2.1.278 (Claude Code)"),
            None
        );
        assert_eq!(version_from_output(Vendor::Claude, "2.1.278"), None);
        assert_eq!(version_from_output(Vendor::OpenCode, "opencode"), None);
        assert_eq!(version_from_output(Vendor::Codex, ""), None);
    }

    #[test]
    fn help_signals() {
        assert!(help_matches(
            Vendor::Cursor,
            "Usage: agent [options]\n\nStart the Cursor Agent\n"
        ));
        assert!(!help_matches(
            Vendor::Cursor,
            "Usage: agent [options]\n\nA generic agent runner\n"
        ));
        assert!(help_matches(
            Vendor::OpenCode,
            "Commands:\n  opencode completion   generate shell completion script\n  opencode acp\n"
        ));
        assert!(!help_matches(Vendor::OpenCode, "Usage: something-else\n"));
    }
}
