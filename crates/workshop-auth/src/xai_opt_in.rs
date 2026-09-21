//! Explicit, persisted opt-in for the optional xAI connection.
//!
//! `GrokComConfig::default()` only constructs the xAI OAuth2 issuer when this
//! marker is present in the Workshop home. The picker's xAI card is the only
//! writer. Nothing else in the tree may enable it.
//!
//! The marker is a tiny non-secret TOML file, not `auth.json`; enabling it
//! does not sign anyone in, it only lets the inherited OIDC flow target
//! `auth.x.ai` the next time the user chooses the card.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// File name under the Workshop home.
pub const MARKER_FILE_NAME: &str = "workshop-connections.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Connections {
    xai: XaiSection,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct XaiSection {
    opt_in: bool,
}

/// Path of the marker file for `home`.
pub fn marker_path(home: &Path) -> PathBuf {
    home.join(MARKER_FILE_NAME)
}

/// `true` when the user has explicitly enabled the optional xAI connection.
/// Any read or parse failure is `false` (fail closed: no xAI).
pub fn is_enabled(home: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(marker_path(home)) else {
        return false;
    };
    toml::from_str::<Connections>(&raw)
        .map(|c| c.xai.opt_in)
        .unwrap_or(false)
}

/// The opt-in state as it was the first time this process asked.
///
/// The agent builds its `GrokComConfig` once at startup, so a card chosen later
/// in the same process cannot retarget the running auth manager. Callers use
/// this frozen value to decide between "start the xAI flow now" and "enabled;
/// restart to sign in". [`enable`] freezes the value before writing so the
/// distinction survives an enable in the same process.
pub fn enabled_at_process_start(home: &Path) -> bool {
    static AT_START: OnceLock<bool> = OnceLock::new();
    *AT_START.get_or_init(|| is_enabled(home))
}

/// Persist the opt-in. Creates `home` if needed; writes 0600 on Unix.
pub fn enable(home: &Path) -> std::io::Result<()> {
    let _ = enabled_at_process_start(home);
    write(home, true)
}

/// Remove the opt-in. Keeps the file so a later `enable` is a one-line change.
pub fn disable(home: &Path) -> std::io::Result<()> {
    write(home, false)
}

fn write(home: &Path, opt_in: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let doc = Connections {
        xai: XaiSection { opt_in },
    };
    let body = toml::to_string(&doc).map_err(std::io::Error::other)?;
    let contents = format!(
        "# Written by Workshop when you choose the optional xAI card in the connection picker.\n# xai.opt_in = true lets the inherited OIDC flow target auth.x.ai. Delete this file or set it to false to revoke.\n{body}"
    );
    let path = marker_path(home);
    let tmp = path.with_extension("toml.tmp");
    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_marker_means_no_xai() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_enabled(dir.path()));
    }

    #[test]
    fn enable_then_disable_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        enable(dir.path()).unwrap();
        assert!(is_enabled(dir.path()));
        disable(dir.path()).unwrap();
        assert!(!is_enabled(dir.path()));
        assert!(marker_path(dir.path()).exists());
    }

    #[test]
    fn malformed_marker_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(marker_path(dir.path()), "[xai]\nopt_in = \"yes\"\n").unwrap();
        assert!(!is_enabled(dir.path()));
        std::fs::write(marker_path(dir.path()), "not toml at all").unwrap();
        assert!(!is_enabled(dir.path()));
    }
}
