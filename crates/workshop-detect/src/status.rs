//! Login state through each vendor's **official status command** — and nothing else.
//!
//! Commands and their documented behaviour (verified 2026-09-21 against the vendor docs and the
//! installed CLIs):
//!
//! | Vendor   | Status command                 | Signed in                          | Signed out                                   |
//! |----------|--------------------------------|------------------------------------|----------------------------------------------|
//! | Claude   | `claude auth status`           | JSON `loggedIn: true`, exit 0      | JSON `loggedIn: false`, exit 1               |
//! | Codex    | `codex login status`           | exit 0 (prints auth mode)          | `Not logged in`, exit 1                      |
//! | Cursor   | `agent status --format json`   | JSON `isAuthenticated: true`       | JSON `isAuthenticated: false` (exit still 0) |
//! | OpenCode | `opencode auth list`           | `N credentials` with N > 0         | `0 credentials`                              |
//!
//! Anything that does not match is `Unknown`, which the picker treats as Sign in (fail closed).
//! This module never reads vendor credential files, keychains, or databases.

use std::ffi::OsString;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::locate::DetectConfig;
use crate::model::Vendor;
use crate::process::{self, ChildOutput};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LoginState {
    LoggedIn,
    LoggedOut,
    /// The status command ran but its answer could not be interpreted, or it failed to run.
    Unknown {
        reason: String,
    },
}

impl LoginState {
    pub fn is_logged_in(&self) -> bool {
        matches!(self, LoginState::LoggedIn)
    }

    fn unknown(reason: impl Into<String>) -> Self {
        LoginState::Unknown {
            reason: reason.into(),
        }
    }
}

/// `--version` for every vendor.
pub fn version_argv(_vendor: Vendor) -> &'static [&'static str] {
    &["--version"]
}

/// The documented status command.
pub fn status_argv(vendor: Vendor) -> &'static [&'static str] {
    match vendor {
        Vendor::Claude => &["auth", "status"],
        Vendor::Codex => &["login", "status"],
        Vendor::Cursor => &["status", "--format", "json"],
        Vendor::OpenCode => &["auth", "list"],
    }
}

/// The documented interactive login command. Workshop attaches this to the user's terminal; it
/// never captures or parses the login output.
pub fn login_argv(vendor: Vendor) -> &'static [&'static str] {
    match vendor {
        Vendor::Claude => &["auth", "login"],
        Vendor::Codex => &["login"],
        Vendor::Cursor => &["login"],
        Vendor::OpenCode => &["auth", "login"],
    }
}

/// The documented logout command (`workshop auth logout <adapter>` delegates here).
pub fn logout_argv(vendor: Vendor) -> &'static [&'static str] {
    match vendor {
        Vendor::Claude => &["auth", "logout"],
        Vendor::Codex => &["logout"],
        Vendor::Cursor => &["logout"],
        Vendor::OpenCode => &["auth", "logout"],
    }
}

fn credentials_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)\b(\d+)\s+credentials?\b").expect("valid regex"))
}

fn json_object(stdout: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let text = process::strip_ansi(stdout);
    let start = text.find('{')?;
    match serde_json::from_str::<serde_json::Value>(text[start..].trim()) {
        Ok(serde_json::Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// Interpret the status command's output. Pure function over the captured output.
pub fn interpret_status(vendor: Vendor, out: &ChildOutput) -> LoginState {
    if out.timed_out {
        return LoginState::unknown("status command timed out");
    }
    match vendor {
        Vendor::Claude => {
            if let Some(map) = json_object(&out.stdout)
                && let Some(logged_in) = map.get("loggedIn").and_then(|v| v.as_bool())
            {
                return if logged_in {
                    LoginState::LoggedIn
                } else {
                    LoginState::LoggedOut
                };
            }
            match out.code {
                Some(0) => LoginState::LoggedIn,
                Some(1) => LoginState::LoggedOut,
                code => {
                    LoginState::unknown(format!("claude auth status exited {code:?} without JSON"))
                }
            }
        }
        Vendor::Codex => match out.code {
            Some(0) => LoginState::LoggedIn,
            Some(1) => LoginState::LoggedOut,
            code => LoginState::unknown(format!("codex login status exited {code:?}")),
        },
        Vendor::Cursor => match json_object(&out.stdout) {
            Some(map) => match map.get("isAuthenticated").and_then(|v| v.as_bool()) {
                Some(true) => LoginState::LoggedIn,
                Some(false) => LoginState::LoggedOut,
                None => LoginState::unknown("agent status JSON lacks isAuthenticated"),
            },
            None => LoginState::unknown(format!(
                "agent status printed no JSON (exit {:?})",
                out.code
            )),
        },
        Vendor::OpenCode => {
            let text = process::strip_ansi(&format!("{}\n{}", out.stdout, out.stderr));
            match credentials_re().captures(&text) {
                Some(c) => match c[1].parse::<u64>() {
                    Ok(0) => LoginState::LoggedOut,
                    Ok(_) => LoginState::LoggedIn,
                    Err(_) => LoginState::unknown("unparseable credential count"),
                },
                None => LoginState::unknown(format!(
                    "opencode auth list printed no credential count (exit {:?})",
                    out.code
                )),
            }
        }
    }
}

/// Run the official status command for a verified binary.
pub fn login_state(vendor: Vendor, bin: &Path, cfg: &DetectConfig) -> LoginState {
    let env: Vec<(OsString, OsString)> = match crate::env::minimal_env(&cfg.extra_env) {
        Ok(env) => env,
        Err(e) => return LoginState::unknown(e.to_string()),
    };
    match process::run(bin, status_argv(vendor), None, &env, cfg.timeout) {
        Ok(out) => interpret_status(vendor, &out),
        Err(e) => LoginState::unknown(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(code: Option<i32>, stdout: &str, stderr: &str) -> ChildOutput {
        ChildOutput {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            timed_out: false,
        }
    }

    #[test]
    fn claude_real_outputs() {
        let logged_out = r#"{
  "loggedIn": false,
  "authMethod": "none",
  "apiProvider": "firstParty"
}"#;
        assert_eq!(
            interpret_status(Vendor::Claude, &out(Some(1), logged_out, "")),
            LoginState::LoggedOut
        );
        let logged_in = r#"{"loggedIn": true, "authMethod": "claude.ai"}"#;
        assert_eq!(
            interpret_status(Vendor::Claude, &out(Some(0), logged_in, "")),
            LoginState::LoggedIn
        );
        // Exit code fallback when the CLI prints text instead of JSON.
        assert_eq!(
            interpret_status(
                Vendor::Claude,
                &out(Some(1), "Not logged in. Run claude auth login", "")
            ),
            LoginState::LoggedOut
        );
        assert!(matches!(
            interpret_status(Vendor::Claude, &out(Some(2), "usage error", "")),
            LoginState::Unknown { .. }
        ));
    }

    #[test]
    fn codex_real_outputs() {
        assert_eq!(
            interpret_status(Vendor::Codex, &out(Some(1), "Not logged in\n", "")),
            LoginState::LoggedOut
        );
        assert_eq!(
            interpret_status(
                Vendor::Codex,
                &out(Some(0), "Logged in using ChatGPT\n", "")
            ),
            LoginState::LoggedIn
        );
        assert!(matches!(
            interpret_status(Vendor::Codex, &out(Some(101), "", "panic")),
            LoginState::Unknown { .. }
        ));
    }

    #[test]
    fn cursor_real_outputs() {
        let logged_out = r#"{
  "status": "unauthenticated",
  "isAuthenticated": false,
  "hasAccessToken": false,
  "message": "Not logged in"
}"#;
        assert_eq!(
            interpret_status(Vendor::Cursor, &out(Some(0), logged_out, "")),
            LoginState::LoggedOut
        );
        assert_eq!(
            interpret_status(
                Vendor::Cursor,
                &out(
                    Some(0),
                    r#"{"status":"authenticated","isAuthenticated":true}"#,
                    ""
                )
            ),
            LoginState::LoggedIn
        );
        assert!(matches!(
            interpret_status(Vendor::Cursor, &out(Some(0), "Logged in as someone", "")),
            LoginState::Unknown { .. }
        ));
    }

    #[test]
    fn opencode_real_outputs() {
        // Real output prints the path of OpenCode's credential file after "Credentials"; Workshop
        // only reads the count and never opens that file, so the path is elided here.
        let none = "\u{1b}[0m\n┌  Credentials \u{1b}[90m~/<opencode credential file>\n│\n└  0 credentials\n";
        assert_eq!(
            interpret_status(Vendor::OpenCode, &out(Some(0), none, "")),
            LoginState::LoggedOut
        );
        let one = "┌  Credentials ~/x\n│\n●  anthropic \u{1b}[90moauth\n│\n└  1 credential\n";
        assert_eq!(
            interpret_status(Vendor::OpenCode, &out(Some(0), one, "")),
            LoginState::LoggedIn
        );
        assert!(matches!(
            interpret_status(Vendor::OpenCode, &out(Some(0), "something else", "")),
            LoginState::Unknown { .. }
        ));
    }

    #[test]
    fn timeout_is_unknown() {
        let mut o = out(None, "", "");
        o.timed_out = true;
        for v in Vendor::ALL {
            assert!(matches!(
                interpret_status(v, &o),
                LoginState::Unknown { .. }
            ));
        }
    }

    #[test]
    fn argv_are_the_documented_commands() {
        assert_eq!(status_argv(Vendor::Claude), &["auth", "status"]);
        assert_eq!(status_argv(Vendor::Codex), &["login", "status"]);
        assert_eq!(status_argv(Vendor::Cursor), &["status", "--format", "json"]);
        assert_eq!(status_argv(Vendor::OpenCode), &["auth", "list"]);
        assert_eq!(login_argv(Vendor::Claude), &["auth", "login"]);
        assert_eq!(login_argv(Vendor::Codex), &["login"]);
        assert_eq!(login_argv(Vendor::Cursor), &["login"]);
        assert_eq!(login_argv(Vendor::OpenCode), &["auth", "login"]);
    }
}
