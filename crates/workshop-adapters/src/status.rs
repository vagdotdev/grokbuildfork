//! Login state via the vendor's own status command, and the picker rail state
//! derived from detection + login.
//!
//! Nothing here opens `~/.claude`, `~/.codex/auth.json`, OpenCode's
//! `auth.json`, `~/.cursor/sdk/auth.json`, or a keychain. The vendor CLI is
//! asked, and only its documented, non-secret answer is interpreted.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::adapter::{Adapter, AdapterId, LoginState};
use crate::detect::{Detection, InstalledCli};
use crate::probe::run_probe;

/// Ask the installed CLI whether it is logged in.
pub async fn probe_login(
    adapter: &dyn Adapter,
    cli: &InstalledCli,
    env: Option<&BTreeMap<OsString, OsString>>,
    timeout: Duration,
) -> LoginState {
    let owned_env;
    let env = match env {
        Some(env) => env,
        None => match crate::env::minimal_env_from_process(&[]) {
            Ok(env) => {
                owned_env = env;
                &owned_env
            }
            Err(e) => {
                return LoginState::Unknown {
                    reason: e.to_string(),
                };
            }
        },
    };
    match run_probe(&cli.path, adapter.status_args(), env, None, timeout).await {
        Ok(output) => adapter.interpret_status(&output),
        Err(e) => LoginState::Unknown {
            reason: e.to_string(),
        },
    }
}

/// The pill shown on a subscription rail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RailPill {
    Detecting,
    Ready,
    SignIn,
}

impl RailPill {
    pub fn label(self) -> &'static str {
        match self {
            RailPill::Detecting => "Detecting",
            RailPill::Ready => "Ready",
            RailPill::SignIn => "Sign in",
        }
    }
}

/// What a subscription rail shows for one product.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RailStatus {
    pub adapter: AdapterId,
    pub installed: bool,
    pub pill: RailPill,
    /// Empty-state copy for the right pane when there is nothing to list.
    pub empty_copy: String,
    /// Whether the rail should offer a Connect action (official vendor login).
    pub show_connect: bool,
}

/// Compose the rail from a finished (or in-flight) probe.
///
/// `detection == None` means the probe is still running. `cursor_app_present`
/// is a plain path-exists hint used only for the Cursor desktop-only copy.
pub fn rail_status(
    adapter: AdapterId,
    detection: Option<&Detection>,
    login: Option<&LoginState>,
    cursor_app_present: bool,
) -> RailStatus {
    let name = adapter.display_name();
    let Some(detection) = detection else {
        return RailStatus {
            adapter,
            installed: false,
            pill: RailPill::Detecting,
            empty_copy: String::new(),
            show_connect: false,
        };
    };
    match detection {
        Detection::Installed(_) => match login {
            None => RailStatus {
                adapter,
                installed: true,
                pill: RailPill::Detecting,
                empty_copy: String::new(),
                show_connect: false,
            },
            Some(LoginState::Ready { .. }) => RailStatus {
                adapter,
                installed: true,
                pill: RailPill::Ready,
                empty_copy: "No models for this harness".to_string(),
                show_connect: false,
            },
            Some(LoginState::SignIn) | Some(LoginState::Unknown { .. }) => RailStatus {
                adapter,
                installed: true,
                pill: RailPill::SignIn,
                empty_copy: format!(
                    "Sign in to see {} models",
                    match adapter {
                        AdapterId::Claude => "Claude",
                        other => other.display_name(),
                    }
                ),
                show_connect: true,
            },
        },
        Detection::Unverified { .. } | Detection::NotInstalled => RailStatus {
            adapter,
            installed: false,
            pill: RailPill::SignIn,
            empty_copy: match adapter {
                AdapterId::Cursor if cursor_app_present => {
                    "Cursor app found — install Agent CLI to use this subscription".to_string()
                }
                AdapterId::Cursor => "Cursor runs in the desktop app".to_string(),
                _ => format!("Install {name}, then sign in"),
            },
            show_connect: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::PinStatus;
    use std::path::PathBuf;

    fn installed(id: AdapterId) -> Detection {
        Detection::Installed(InstalledCli {
            adapter: id,
            path: PathBuf::from("/x"),
            version: "1".into(),
            pin: PinStatus::Tested,
        })
    }

    #[test]
    fn rail_copy_matches_picker_states() {
        let r = rail_status(AdapterId::Claude, None, None, false);
        assert_eq!(r.pill, RailPill::Detecting);

        let r = rail_status(
            AdapterId::Claude,
            Some(&Detection::NotInstalled),
            None,
            false,
        );
        assert_eq!(r.pill, RailPill::SignIn);
        assert_eq!(r.empty_copy, "Install Claude Code, then sign in");
        assert!(!r.show_connect);

        let r = rail_status(
            AdapterId::Codex,
            Some(&Detection::NotInstalled),
            None,
            false,
        );
        assert_eq!(r.empty_copy, "Install Codex, then sign in");

        let d = installed(AdapterId::Codex);
        let r = rail_status(AdapterId::Codex, Some(&d), Some(&LoginState::SignIn), false);
        assert_eq!(r.pill, RailPill::SignIn);
        assert_eq!(r.empty_copy, "Sign in to see Codex models");
        assert!(r.show_connect);

        let d = installed(AdapterId::Claude);
        let ready = LoginState::Ready { method: None };
        let r = rail_status(AdapterId::Claude, Some(&d), Some(&ready), false);
        assert_eq!(r.pill, RailPill::Ready);

        let r = rail_status(
            AdapterId::Cursor,
            Some(&Detection::NotInstalled),
            None,
            true,
        );
        assert_eq!(
            r.empty_copy,
            "Cursor app found — install Agent CLI to use this subscription"
        );
        let r = rail_status(
            AdapterId::Cursor,
            Some(&Detection::NotInstalled),
            None,
            false,
        );
        assert_eq!(r.empty_copy, "Cursor runs in the desktop app");
    }
}
