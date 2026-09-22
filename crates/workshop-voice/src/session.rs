//! `/voice` press → a ready helper with the right model: resolve the tier, make its file present
//! and verified (downloading with a progress line if not), start or reuse the helper, step down a
//! tier when this machine cannot decode an interim within budget, remember the choice, open the
//! utterance. This is the whole self-heal path of voice-spec §5 rule 6 and the tiering of §9.6.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::engine::{self, EngineConfig, EngineSession, ReadyInfo};
use crate::store::{ModelStore, Progress};
use crate::{Error, tier};

#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Voice directory (`<workshop home>/voice`); `None` = default.
    pub voice_dir: Option<PathBuf>,
    /// `voice.model` from config, if set.
    pub tier_override: Option<String>,
    /// `voice.engine_path` from config, if set; else the helper is located beside the binary.
    pub engine_path: Option<PathBuf>,
    /// Concrete Whisper language code, or `None` for auto-detect.
    pub language: Option<String>,
}

/// Everything the caller may want to log or show once the session is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub tier: String,
    pub model: PathBuf,
    pub ready: ReadyInfo,
    /// Tiers that were tried and stepped down from during this open.
    pub stepped_down_from: Vec<String>,
    pub downloaded: bool,
}

/// One-line status for the recording banner ("Downloading voice model… 42%").
pub type StatusFn<'a> = dyn Fn(String) + Send + Sync + 'a;

pub fn progress_line(p: Progress) -> String {
    let pct = if p.total > 0 {
        (p.received * 100 / p.total).min(100)
    } else {
        0
    };
    let mib = |b: u64| b / (1024 * 1024);
    let retry = if p.attempt > 1 {
        format!(" (retry {})", p.attempt)
    } else {
        String::new()
    };
    format!(
        "Downloading voice model… {pct}% ({} of {} MiB){retry}",
        mib(p.received),
        mib(p.total)
    )
}

/// Open an utterance, downloading and tier-stepping as needed. `status` receives banner text
/// while something takes time and an empty string when the banner should go back to "Recording".
pub async fn open(
    opts: &OpenOptions,
    status: &StatusFn<'_>,
) -> Result<(EngineSession, Opened), Error> {
    let engine_path = match opts.engine_path.clone().filter(|p| p.is_file()) {
        Some(p) => p,
        None => engine::locate_engine().ok_or(Error::EngineMissing)?,
    };
    let dir = opts
        .voice_dir
        .clone()
        .unwrap_or_else(crate::store::default_dir);
    let machine = tier::detect_machine();
    let mut stepped_down_from = Vec::new();
    let mut downloaded = false;
    let mut tier_name = tier::resolve(&dir, opts.tier_override.as_deref(), &machine);
    let mut model_retry = false;
    tracing::info!(engine = %engine_path.display(), dir = %dir.display(), tier = %tier_name, "voice: opening session");

    loop {
        let store = ModelStore::for_tier(&dir, &tier_name)
            .ok_or_else(|| Error::Model(format!("unknown voice model tier {tier_name}")))?;
        let started = Instant::now();
        let before = store.quick_status();
        tracing::debug!(tier = %tier_name, status = %before.describe(), "voice: model quick status");
        // The downloader reports every chunk; the banner needs a line per percent, not per packet.
        let last_line = std::sync::Mutex::new(String::new());
        let model = store
            .ensure(&|p: Progress| {
                let line = progress_line(p);
                if let Ok(mut last) = last_line.lock()
                    && *last != line
                {
                    *last = line.clone();
                    status(line);
                }
            })
            .await?;
        if !before.is_ready() {
            downloaded = true;
            tracing::info!(
                tier = %tier_name,
                secs = started.elapsed().as_secs(),
                "voice model downloaded during /voice"
            );
        }
        tracing::debug!(tier = %tier_name, verify_ms = started.elapsed().as_millis() as u64, "voice: model verified");
        status("Loading voice model…".to_owned());

        let config = EngineConfig {
            engine: engine_path.clone(),
            model: model.clone(),
            tier: tier_name.clone(),
            language: opts.language.clone(),
            use_gpu: true,
        };
        let ready = match engine::start_helper(&config).await {
            Ok(r) => r,
            Err(Error::Model(msg)) if !model_retry => {
                // The helper refused the file: hash it again, replace it, one retry.
                tracing::warn!(error = %msg, "voice engine rejected the model file; re-verifying");
                store.forget_verified();
                if !store.status().is_ready() {
                    store.remove_bad_model();
                }
                model_retry = true;
                continue;
            }
            Err(e) => {
                status(String::new());
                return Err(e);
            }
        };

        let forced = opts.tier_override.as_deref().is_some_and(tier::is_tier);
        if ready.cold && !forced && !tier::within_budget(ready.probe_ms) {
            if let Some(lower) = tier::next_lower(&tier_name) {
                tracing::info!(
                    from = %tier_name,
                    to = lower,
                    probe_ms = ready.probe_ms,
                    budget_ms = tier::interim_budget_ms(),
                    "voice model too slow here; stepping down a tier"
                );
                engine::shutdown().await;
                stepped_down_from.push(tier_name.clone());
                tier_name = lower.to_owned();
                if let Err(e) = tier::write_selection(&dir, lower) {
                    tracing::warn!(error = %e, "could not persist the voice model selection");
                }
                continue;
            }
            tracing::warn!(
                probe_ms = ready.probe_ms,
                "smallest voice model is still over budget on this machine; using it anyway"
            );
        }
        if !forced
            && tier::read_selection(&dir).is_none()
            && let Err(e) = tier::write_selection(&dir, &tier_name)
        {
            tracing::warn!(error = %e, "could not persist the voice model selection");
        }

        let session = engine::open_session(opts.language.as_deref()).await?;
        status(String::new());
        return Ok((
            session,
            Opened {
                tier: tier_name,
                model,
                ready,
                stepped_down_from,
                downloaded,
            },
        ));
    }
}

/// The voice directory the TUI and the installer share.
pub fn voice_dir(opts: &OpenOptions) -> PathBuf {
    opts.voice_dir
        .clone()
        .unwrap_or_else(crate::store::default_dir)
}

/// Path of the model `/voice` would use right now (for `/doctor`); no download.
pub fn current_model_path(dir: &Path, tier_override: Option<&str>) -> (String, PathBuf) {
    let t = tier::resolve(dir, tier_override, &tier::detect_machine());
    let path = ModelStore::for_tier(dir, &t)
        .map(|s| s.path())
        .unwrap_or_else(|| dir.join("unknown"));
    (t, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line_is_one_short_line() {
        let line = progress_line(Progress {
            received: 231 << 20,
            total: 547 << 20,
            attempt: 1,
        });
        assert_eq!(line, "Downloading voice model… 42% (231 of 547 MiB)");
        let retry = progress_line(Progress {
            received: 0,
            total: 547 << 20,
            attempt: 2,
        });
        assert!(retry.ends_with("(retry 2)"));
    }
}
