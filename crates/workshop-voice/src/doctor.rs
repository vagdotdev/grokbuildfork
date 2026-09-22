//! `/doctor` Voice section: helper present, model path, checksum status, last error. Debugging
//! aid only; never a required step. Prints paths and statuses, never audio or secrets.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::store::{ModelStatus, ModelStore};
use crate::{engine, tier};

static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);
static ENGINE_NOTE: Mutex<Option<String>> = Mutex::new(None);

/// Remember the most recent voice failure for `/doctor`.
pub fn record_last_error(message: impl Into<String>) {
    if let Ok(mut g) = LAST_ERROR.lock() {
        *g = Some(message.into());
    }
}

pub fn last_error() -> Option<String> {
    LAST_ERROR.lock().ok().and_then(|g| g.clone())
}

/// The helper's one-line stderr note (slow probe), if it printed one.
pub fn record_engine_note(line: impl Into<String>) {
    if let Ok(mut g) = ENGINE_NOTE.lock() {
        *g = Some(line.into());
    }
}

pub fn engine_note() -> Option<String> {
    ENGINE_NOTE.lock().ok().and_then(|g| g.clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorFacts {
    pub engine_path: Option<PathBuf>,
    pub engine_version: Option<String>,
    pub engine_error: Option<String>,
    pub tier: String,
    pub tier_source: &'static str,
    pub model_path: PathBuf,
    pub model_status: ModelStatus,
    pub last_error: Option<String>,
    pub engine_note: Option<String>,
}

/// Probe helper and model. `full_checksum` hashes the model (1–2 s); otherwise size only.
pub fn probe(
    voice_dir: &Path,
    tier_override: Option<&str>,
    engine_override: Option<&Path>,
    full_checksum: bool,
) -> DoctorFacts {
    let engine_path = engine_override
        .filter(|p| p.is_file())
        .map(Path::to_path_buf)
        .or_else(engine::locate_engine);
    let (engine_version, engine_error) = match engine_path.as_deref() {
        Some(p) => match engine::engine_version(p) {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e.to_string())),
        },
        None => (
            None,
            Some(format!(
                "{} not found beside the binary, in the workshop bin dir, or on PATH; re-run the installer",
                engine::ENGINE_BIN
            )),
        ),
    };
    let machine = tier::detect_machine();
    let (tier_name, tier_source) = match tier_override.filter(|t| tier::is_tier(t)) {
        Some(t) => (t.to_owned(), "config voice.model"),
        None => match tier::read_selection(voice_dir) {
            Some(t) => (t, "persisted selection"),
            None => (
                tier::resolve(voice_dir, None, &machine),
                "default for this machine",
            ),
        },
    };
    let (model_path, model_status) = match ModelStore::for_tier(voice_dir, &tier_name) {
        Some(store) => (
            store.path(),
            if full_checksum {
                store.status_cached()
            } else {
                store.quick_status()
            },
        ),
        None => (voice_dir.join("unknown"), ModelStatus::Missing),
    };
    DoctorFacts {
        engine_path,
        engine_version,
        engine_error,
        tier: tier_name,
        tier_source,
        model_path,
        model_status,
        last_error: last_error(),
        engine_note: engine_note(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_without_helper_or_model_reports_both() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        crate::test_support::with_env(
            &[
                (engine::ENGINE_ENV, Some("/nonexistent/voice-engine")),
                ("PATH", Some("/nonexistent-bin")),
                ("WORKSHOP_HOME", Some(dir.path().to_str().unwrap())),
            ],
            || {
                let facts = probe(dir.path(), None, None, true);
                assert!(facts.engine_path.is_none());
                assert!(facts.engine_error.is_some());
                assert_eq!(facts.model_status, ModelStatus::Missing);
                assert!(tier::is_tier(&facts.tier));
                assert_eq!(facts.tier_source, "default for this machine");
                assert!(facts.model_path.starts_with(dir.path()));
            },
        );
        record_last_error("boom");
        assert_eq!(last_error().as_deref(), Some("boom"));
    }
}
