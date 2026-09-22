//! On-disk engine diagnostics under `$WORKSHOP_HOME`: the last start attempt and the server log.
//!
//! Written by the host every time the engine is (re)started so `workshop doctor` can show the
//! binary, its version, the last error, and where the `opencode serve` output went — the answer
//! to "nothing works" on a machine we cannot see.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `$WORKSHOP_HOME/engine/state.json`.
pub fn state_path(workshop_home: &Path) -> PathBuf {
    workshop_home.join("engine").join("state.json")
}

/// `$WORKSHOP_HOME/logs/opencode-engine.log` (stdout + stderr of `opencode serve`, appended).
pub fn log_path(workshop_home: &Path) -> PathBuf {
    workshop_home.join("logs").join("opencode-engine.log")
}

/// What the last engine start attempt looked like.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineState {
    /// Resolved `opencode` binary (`None` until detection succeeded once).
    #[serde(default)]
    pub binary: Option<PathBuf>,
    #[serde(default)]
    pub version: Option<String>,
    /// Unix seconds of the last start attempt.
    #[serde(default)]
    pub last_start_unix: Option<u64>,
    /// The phase the last attempt reached: `install`, `start`, `session`, `ready`.
    #[serde(default)]
    pub last_phase: Option<String>,
    /// The error the last attempt ended with, if it failed.
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub log_path: Option<PathBuf>,
}

impl EngineState {
    pub fn load(workshop_home: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(state_path(workshop_home)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// Best-effort atomic write (0600 on Unix); failures are logged, never fatal.
    pub fn save(&self, workshop_home: &Path) {
        let path = state_path(workshop_home);
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!(error = %e, path = %parent.display(), "engine state dir");
            return;
        }
        let Ok(json) = serde_json::to_vec_pretty(self) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::write(&tmp, &json).and_then(|()| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(&tmp, &path)
        });
        if let Err(e) = written {
            tracing::warn!(error = %e, path = %path.display(), "engine state write");
        }
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Append one stamped line to the engine log; creates the directory on first use.
pub fn append_log(path: &Path, line: &str) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[{}] {}", now_unix(), line.trim_end());
    }
}

/// The macOS quarantine attribute on `path` (`Some(value)` when set); `None` elsewhere or when
/// absent. A quarantined binary is what Gatekeeper refuses to run from a non-terminal spawn.
pub fn quarantine_flag(path: &Path) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = std::process::Command::new("xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Remove the macOS quarantine attribute from a freshly installed binary (best-effort; no-op
/// elsewhere). Mirrors what `scripts/install.sh` does for the `workshop` binary itself.
pub fn clear_quarantine(path: &Path) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let _ = std::process::Command::new("xattr")
        .args(["-d", "com.apple.quarantine"])
        .arg(path)
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_is_private() {
        let home = tempfile::tempdir().unwrap();
        let state = EngineState {
            binary: Some(PathBuf::from("/x/opencode")),
            version: Some("1.18.31".into()),
            last_start_unix: Some(1),
            last_phase: Some("ready".into()),
            last_error: None,
            log_path: Some(log_path(home.path())),
        };
        state.save(home.path());
        assert_eq!(EngineState::load(home.path()), Some(state));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(state_path(home.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn missing_state_is_none_and_log_appends() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(EngineState::load(home.path()), None);
        let log = log_path(home.path());
        append_log(&log, "one");
        append_log(&log, "two\n");
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().all(|l| l.starts_with('[')));
    }
}
