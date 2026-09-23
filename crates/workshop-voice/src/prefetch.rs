//! Voice getting ready in the background: a default install ships `workshop` alone, so the TUI
//! fetches the `voice-engine` helper and this machine's speech model itself — after the first
//! reply on a first launch, half a minute into a later one — resumably, from the release mirror
//! only, and never twice. `/voice` before that reads the progress (`Voice is getting ready — 62%`)
//! instead of failing. Off with `voice.auto_download = false` or `WORKSHOP_VOICE_AUTO=0`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::store::{ModelStore, Progress};
use crate::{doctor, engine, helper, tier};

/// Environment override: a falsy value (`0`, `false`, `off`, `no`) turns the background setup off.
pub const AUTO_ENV: &str = "WORKSHOP_VOICE_AUTO";

/// Whether the background setup runs: the config flag (`voice.auto_download`, default on) unless
/// [`AUTO_ENV`] turns it off.
pub fn auto_enabled(config_flag: bool) -> bool {
    if let Some(v) = std::env::var_os(AUTO_ENV) {
        let v = v.to_string_lossy().trim().to_ascii_lowercase();
        if matches!(v.as_str(), "0" | "false" | "off" | "no") {
            return false;
        }
    }
    config_flag
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    Helper,
    Model,
    Ready,
    Failed(String),
}

/// What the background setup is doing, for `/voice` and `/doctor`.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub phase: Phase,
    pub received: u64,
    pub total: u64,
}

impl Status {
    /// Whole percent of the current download, when its size is known.
    pub fn percent(&self) -> Option<u64> {
        (self.total > 0).then(|| (self.received * 100 / self.total).min(100))
    }

    /// The one line `/voice` shows while voice is not ready yet.
    pub fn line(&self) -> String {
        match (&self.phase, self.percent()) {
            (Phase::Failed(_), _) => {
                "Voice couldn't be set up \u{2014} /doctor shows why".to_owned()
            }
            (Phase::Ready, _) => "Voice is ready".to_owned(),
            (Phase::Model, Some(pct)) => format!("Voice is getting ready \u{2014} {pct}%"),
            _ => "Voice is getting ready\u{2026}".to_owned(),
        }
    }
}

pub type Shared = Arc<Mutex<Status>>;

pub fn shared() -> Shared {
    Arc::new(Mutex::new(Status::default()))
}

fn set(shared: &Shared, f: impl FnOnce(&mut Status)) {
    if let Ok(mut s) = shared.lock() {
        f(&mut s);
    }
}

/// Whether `/voice` can run right now: the helper is in place and this machine's model is
/// present (size check; the full verification happens on the first press).
pub fn is_ready(
    voice_dir: &Path,
    tier_override: Option<&str>,
    engine_override: Option<&Path>,
) -> bool {
    let helper_present =
        engine_override.is_some_and(Path::is_file) || engine::locate_engine().is_some();
    if !helper_present {
        return false;
    }
    let tier = tier::resolve(voice_dir, tier_override, &tier::detect_machine());
    ModelStore::for_tier(voice_dir, &tier).is_some_and(|s| s.quick_status().is_ready())
}

/// Fetch the helper, then the model this machine will use, reporting into `shared`. Runs as a
/// detached task; a failure is recorded for `/doctor` and the next launch tries again (the
/// model's `.partial` resumes where it stopped).
pub async fn run(home: PathBuf, voice_dir: PathBuf, tier_override: Option<String>, shared: Shared) {
    set(&shared, |s| {
        *s = Status {
            phase: Phase::Helper,
            ..Status::default()
        }
    });
    let report = |phase: Phase, shared: &Shared| {
        let shared = shared.clone();
        move |p: Progress| {
            set(&shared, |s| {
                s.phase = phase.clone();
                s.received = p.received;
                s.total = p.total;
            });
        }
    };
    let outcome = async {
        helper::ensure_helper(&home, &report(Phase::Helper, &shared)).await?;
        let tier = tier::resolve(
            &voice_dir,
            tier_override.as_deref(),
            &tier::detect_machine(),
        );
        let store = ModelStore::for_tier(&voice_dir, &tier)
            .ok_or_else(|| crate::Error::Model(format!("unknown voice model tier {tier}")))?;
        let url = store.pin().prefetch_url().ok_or_else(|| {
            crate::Error::Download("this build has no release mirror for the voice model".into())
        })?;
        set(&shared, |s| {
            s.phase = Phase::Model;
            s.received = 0;
            s.total = store.pin().size;
        });
        store
            .ensure_from(&[url], &report(Phase::Model, &shared))
            .await?;
        Ok::<_, crate::Error>(tier)
    }
    .await;
    match outcome {
        Ok(tier) => {
            tracing::info!(%tier, "voice is ready (background setup)");
            set(&shared, |s| s.phase = Phase::Ready);
        }
        Err(e) => {
            tracing::warn!(error = %e, "background voice setup failed");
            doctor::record_last_error(format!("background setup: {e}"));
            set(&shared, |s| s.phase = Phase::Failed(e.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_env_turns_the_setup_off() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        for (value, expected) in [
            (None, true),
            (Some("1"), true),
            (Some("0"), false),
            (Some("false"), false),
            (Some(" OFF "), false),
            (Some("no"), false),
            (Some("yes"), true),
        ] {
            crate::test_support::with_env(&[(AUTO_ENV, value)], || {
                assert_eq!(auto_enabled(true), expected, "{value:?}");
                assert!(
                    !auto_enabled(false),
                    "{value:?}: the config flag still rules"
                );
            });
        }
    }

    #[test]
    fn status_line_reads_like_a_product() {
        let mut s = Status::default();
        assert_eq!(s.line(), "Voice is getting ready\u{2026}");
        s.phase = Phase::Model;
        s.received = 62;
        s.total = 100;
        assert_eq!(s.line(), "Voice is getting ready \u{2014} 62%");
        s.phase = Phase::Failed("boom".into());
        assert_eq!(
            s.line(),
            "Voice couldn't be set up \u{2014} /doctor shows why"
        );
        s.phase = Phase::Ready;
        assert_eq!(s.line(), "Voice is ready");
    }
}
