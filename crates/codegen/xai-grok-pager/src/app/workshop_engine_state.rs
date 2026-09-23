//! Workshop overlay: on-disk OpenCode engine diagnostics under `$WORKSHOP_HOME` — the last start
//! attempt (`engine/state.json`) and the `opencode serve` log (`logs/opencode-engine.log`).
//!
//! Written every time the engine is (re)started so `workshop doctor` can show the binary, its
//! version, the last error, and where the server output went — the answer to "nothing works" on a
//! machine we cannot see. Lives in the pager because `workshop-adapters` never reads file
//! contents (its no-theft gate); the adapter crate only hands us log lines.

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
    /// The phase the last attempt reached: `detect`, `install`, `start`, `ready`.
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

    /// Best-effort private write; failures are logged, never fatal.
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
        if let Err(e) = workshop_providers::atomic_write_private(&path, &json) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips() {
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
