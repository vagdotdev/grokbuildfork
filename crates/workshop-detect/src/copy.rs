//! User-visible copy for the Subscriptions tab, taken from the Blackpen picker export and the
//! Workshop plan. Keep these strings byte-identical to the spec; the PTY snapshots depend on them.

use crate::model::Rail;

pub const INSTALL_CLAUDE: &str = "Enter installs Claude Code, then signs you in";
pub const INSTALL_CODEX: &str = "Enter installs Codex, then signs you in";
pub const INSTALL_CURSOR: &str = "Enter installs the Cursor Agent CLI, then signs you in";
pub const SIGN_IN_CLAUDE: &str = "Sign in to see Claude models";
pub const SIGN_IN_CODEX: &str = "Sign in to see Codex models";
pub const SIGN_IN_CURSOR: &str = "Sign in to see Cursor models";
/// Cursor: no Agent CLI and no desktop app found, so nothing here can run Cursor.
pub const CURSOR_DESKTOP_ONLY: &str = "Cursor runs in the desktop app";
/// Cursor: the desktop app is installed but `cursor-agent` is not.
pub const CURSOR_APP_WITHOUT_CLI: &str =
    "Cursor app found — install Agent CLI to use this subscription";
pub const NO_MODELS: &str = "No models for this harness";
/// A signed-in rail whose CLI has not answered with its model list yet.
pub const LOADING_MODELS: &str = "Loading models…";
/// A signed-in rail whose CLI could not list its models, with nothing cached. Enter on that rail
/// retries: every terminal delivers it, while Ctrl+R is the composer's session picker and a chord
/// prefix some VS Code-family terminals keep from the app.
pub const MODELS_FAILED: &str = "Couldn't load models — press Enter to retry";

/// Tab labels.
pub const TAB_MODELS: &str = "Models";
pub const TAB_SUBSCRIPTIONS: &str = "Subscriptions";
/// Footer buttons.
pub const CONNECT: &str = "Connect";
pub const CONNECT_PROVIDER: &str = "Connect provider";
pub const MANAGE_MODELS: &str = "Manage models";
/// Optional xAI card copy (Models tab, always last, never preselected).
pub const XAI_OPTIONAL_TITLE: &str = "xAI (optional)";
pub const XAI_OPTIONAL_BODY: &str = "Uses xAI accounts and `auth.x.ai`. Not required.";

/// Copy for an empty rail.
///
/// * `installed`: a verified vendor binary exists.
/// * `ready`: installed and the official status command says signed in.
/// * `app_present`: (Cursor only) the desktop app was found.
pub fn empty_rail_copy(
    rail: Rail,
    installed: bool,
    ready: bool,
    app_present: bool,
) -> &'static str {
    match rail {
        Rail::Claude => {
            if !installed {
                INSTALL_CLAUDE
            } else if !ready {
                SIGN_IN_CLAUDE
            } else {
                NO_MODELS
            }
        }
        Rail::Codex => {
            if !installed {
                INSTALL_CODEX
            } else if !ready {
                SIGN_IN_CODEX
            } else {
                NO_MODELS
            }
        }
        Rail::Cursor => {
            // With or without the desktop app, the Agent CLI is one official install away.
            let _ = app_present;
            if !installed {
                INSTALL_CURSOR
            } else if !ready {
                SIGN_IN_CURSOR
            } else {
                NO_MODELS
            }
        }
    }
}

/// Whether the rail shows a Connect button (`harnessNeedsConnect` in the export): never when the
/// rail already lists models; otherwise when not ready; and when ready-but-empty for Claude and
/// Codex only.
pub fn needs_connect(rail: Rail, ready: bool, models_empty: bool) -> bool {
    if !models_empty {
        return false;
    }
    if !ready {
        return true;
    }
    rail != Rail::Cursor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_table_matches_spec() {
        assert_eq!(
            empty_rail_copy(Rail::Claude, false, false, false),
            INSTALL_CLAUDE
        );
        assert_eq!(
            empty_rail_copy(Rail::Codex, false, false, false),
            INSTALL_CODEX
        );
        assert_eq!(
            empty_rail_copy(Rail::Claude, true, false, false),
            SIGN_IN_CLAUDE
        );
        assert_eq!(
            empty_rail_copy(Rail::Codex, true, false, false),
            SIGN_IN_CODEX
        );
        assert_eq!(
            empty_rail_copy(Rail::Cursor, true, false, false),
            SIGN_IN_CURSOR
        );
        // The Agent CLI is one official install away, desktop app or not.
        assert_eq!(
            empty_rail_copy(Rail::Cursor, false, false, false),
            INSTALL_CURSOR
        );
        assert_eq!(
            empty_rail_copy(Rail::Cursor, false, false, true),
            INSTALL_CURSOR
        );
        for rail in Rail::ALL {
            assert_eq!(empty_rail_copy(rail, true, true, true), NO_MODELS);
        }
    }

    #[test]
    fn connect_rules_match_export() {
        for rail in Rail::ALL {
            assert!(
                !needs_connect(rail, true, false),
                "rail with models never shows Connect"
            );
            assert!(
                needs_connect(rail, false, true),
                "signed-out empty rail shows Connect"
            );
        }
        assert!(needs_connect(Rail::Claude, true, true));
        assert!(needs_connect(Rail::Codex, true, true));
        assert!(!needs_connect(Rail::Cursor, true, true));
    }
}
