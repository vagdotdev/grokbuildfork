//! Which STT provider a Workshop process uses, and how the default is chosen.
//!
//! Config lives in the same `[voice]` table upstream already parses (unknown keys are ignored
//! there), so a user's `language` / `stt_endpointing_ms` keep working:
//!
//! ```toml
//! [voice]
//! provider = "local"            # local | openai | groq | deepgram | xai   (default: resolved)
//! local_model = "base.en"       # tiny.en | base.en | small.en | large-v3-turbo-q5_0
//! auto_download = true          # fetch a missing local model on first use
//! ```
//!
//! Default policy (no `provider` set): **local** when a model is on disk or may be downloaded;
//! otherwise the pager sends the user to the connection picker to choose a voice provider.
//! xAI is never picked automatically: it needs `provider = "xai"` *and* a connected optional
//! xAI provider. No provider is contacted at resolution time.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::local::models::{self, WhisperModel};

/// Provider ids. Cloud ones are BYOK: the user's own key, never a Workshop-issued one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceProvider {
    Local,
    OpenAi,
    Groq,
    Deepgram,
    Xai,
}

impl VoiceProvider {
    pub fn id(self) -> &'static str {
        match self {
            VoiceProvider::Local => "local",
            VoiceProvider::OpenAi => "openai",
            VoiceProvider::Groq => "groq",
            VoiceProvider::Deepgram => "deepgram",
            VoiceProvider::Xai => "xai",
        }
    }

    /// Environment variable whose *presence* marks a BYOK key as available (value never printed).
    pub fn key_env(self) -> Option<&'static str> {
        match self {
            VoiceProvider::Local => None,
            VoiceProvider::OpenAi => Some("OPENAI_API_KEY"),
            VoiceProvider::Groq => Some("GROQ_API_KEY"),
            VoiceProvider::Deepgram => Some("DEEPGRAM_API_KEY"),
            VoiceProvider::Xai => Some("XAI_API_KEY"),
        }
    }
}

/// Workshop's `[voice]` keys (upstream's live alongside; both parsers ignore the other's keys).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct VoiceProviderConfig {
    pub provider: Option<VoiceProvider>,
    pub local_model: Option<String>,
    pub auto_download: bool,
}

impl Default for VoiceProviderConfig {
    fn default() -> Self {
        Self {
            provider: None,
            local_model: None,
            auto_download: true,
        }
    }
}

impl VoiceProviderConfig {
    pub fn from_config_table(root: &toml::Table) -> Self {
        root.get("voice")
            .and_then(|v| v.as_table())
            .and_then(|t| toml::Value::Table(t.clone()).try_into().ok())
            .unwrap_or_default()
    }

    /// Configured model, or the CPU-friendly default.
    pub fn local_model(&self) -> WhisperModel {
        self.local_model
            .as_deref()
            .and_then(WhisperModel::parse)
            .unwrap_or(DEFAULT_LOCAL_MODEL)
    }
}

/// `base.en`: 142 MB, real-time on a laptop CPU, good enough for prompt dictation.
pub const DEFAULT_LOCAL_MODEL: WhisperModel = WhisperModel::BaseEn;

/// Facts the pager knows at resolution time. Nothing here performs I/O beyond a `stat`.
#[derive(Debug, Clone)]
pub struct ResolveContext {
    pub models_dir: PathBuf,
    /// The user connected the optional xAI provider (Direct API key or xAI session).
    pub xai_connected: bool,
    /// Whether a BYOK key for `provider` is available (env presence or Workshop's broker).
    pub key_available: fn(VoiceProvider) -> bool,
    /// Network downloads permitted (offline mode / policy can turn this off).
    pub downloads_allowed: bool,
}

impl ResolveContext {
    pub fn from_process_env(xai_connected: bool) -> Self {
        Self {
            models_dir: models::models_dir(),
            xai_connected,
            key_available: env_key_present,
            downloads_allowed: true,
        }
    }
}

fn env_key_present(provider: VoiceProvider) -> bool {
    provider
        .key_env()
        .is_some_and(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Run whisper.cpp locally. `present == false` means the first press downloads it.
    Local { model: WhisperModel, present: bool },
    /// Stream to the user's own cloud STT account (backend construction is the pager's job).
    Cloud(VoiceProvider),
    /// The inherited xAI streaming STT, only because the user connected xAI.
    Xai,
    /// Nothing usable without a decision: open the picker's voice section.
    NeedsChoice { reason: String },
}

/// Pick the backend for this process. Pure given `ctx`.
pub fn resolve(cfg: &VoiceProviderConfig, ctx: &ResolveContext) -> Resolution {
    let model = cfg.local_model();
    let local_ok = |dir: &Path| -> Option<Resolution> {
        if model.is_present_in(dir) {
            Some(Resolution::Local {
                model,
                present: true,
            })
        } else if let Some(&present) = models::present_models(dir).last() {
            // Another model is already on disk; prefer it over a download
            Some(Resolution::Local {
                model: present,
                present: true,
            })
        } else if cfg.auto_download && ctx.downloads_allowed {
            Some(Resolution::Local {
                model,
                present: false,
            })
        } else {
            None
        }
    };

    match cfg.provider {
        Some(VoiceProvider::Local) => {
            local_ok(&ctx.models_dir).unwrap_or(Resolution::NeedsChoice {
                reason: format!(
                    "voice provider is local but model {} is not downloaded and downloads are off",
                    model.id()
                ),
            })
        }
        Some(VoiceProvider::Xai) => {
            if ctx.xai_connected {
                Resolution::Xai
            } else {
                Resolution::NeedsChoice {
                    reason: "voice provider is xai but the xAI provider is not connected".into(),
                }
            }
        }
        Some(cloud @ (VoiceProvider::OpenAi | VoiceProvider::Groq | VoiceProvider::Deepgram)) => {
            if (ctx.key_available)(cloud) {
                Resolution::Cloud(cloud)
            } else {
                Resolution::NeedsChoice {
                    reason: format!(
                        "voice provider is {} but no {} is available",
                        cloud.id(),
                        cloud.key_env().unwrap_or("API key")
                    ),
                }
            }
        }
        None => local_ok(&ctx.models_dir).unwrap_or_else(|| Resolution::NeedsChoice {
            reason: "no local voice model and downloads are off; pick a voice provider".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_keys(_: VoiceProvider) -> bool {
        false
    }
    fn all_keys(_: VoiceProvider) -> bool {
        true
    }

    fn ctx(dir: &Path) -> ResolveContext {
        ResolveContext {
            models_dir: dir.to_path_buf(),
            xai_connected: false,
            key_available: no_keys,
            downloads_allowed: true,
        }
    }

    fn empty_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wv-provider-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_model(dir: &Path, model: WhisperModel) {
        let f = std::fs::File::create(model.path_in(dir)).unwrap();
        f.set_len(model.size_bytes()).unwrap();
    }

    #[test]
    fn default_is_local_download_when_nothing_present() {
        let dir = empty_dir("default");
        let r = resolve(&VoiceProviderConfig::default(), &ctx(&dir));
        assert_eq!(
            r,
            Resolution::Local {
                model: DEFAULT_LOCAL_MODEL,
                present: false
            }
        );
    }

    #[test]
    fn default_prefers_a_model_already_on_disk() {
        let dir = empty_dir("present");
        fake_model(&dir, WhisperModel::SmallEn);
        let r = resolve(&VoiceProviderConfig::default(), &ctx(&dir));
        assert_eq!(
            r,
            Resolution::Local {
                model: WhisperModel::SmallEn,
                present: true
            }
        );
    }

    #[test]
    fn xai_never_wins_by_default_even_when_connected() {
        let dir = empty_dir("xai-default");
        let mut c = ctx(&dir);
        c.xai_connected = true;
        assert!(matches!(
            resolve(&VoiceProviderConfig::default(), &c),
            Resolution::Local { .. }
        ));
        // Explicitly chosen xAI requires the connection
        let cfg = VoiceProviderConfig {
            provider: Some(VoiceProvider::Xai),
            ..Default::default()
        };
        assert_eq!(resolve(&cfg, &c), Resolution::Xai);
        c.xai_connected = false;
        assert!(matches!(resolve(&cfg, &c), Resolution::NeedsChoice { .. }));
    }

    #[test]
    fn offline_without_model_needs_a_choice() {
        let dir = empty_dir("offline");
        let mut c = ctx(&dir);
        c.downloads_allowed = false;
        assert!(matches!(
            resolve(&VoiceProviderConfig::default(), &c),
            Resolution::NeedsChoice { .. }
        ));
    }

    #[test]
    fn cloud_requires_key_presence_only() {
        let dir = empty_dir("cloud");
        let cfg = VoiceProviderConfig {
            provider: Some(VoiceProvider::Deepgram),
            ..Default::default()
        };
        let mut c = ctx(&dir);
        assert!(matches!(resolve(&cfg, &c), Resolution::NeedsChoice { .. }));
        c.key_available = all_keys;
        assert_eq!(
            resolve(&cfg, &c),
            Resolution::Cloud(VoiceProvider::Deepgram)
        );
    }

    #[test]
    fn parses_workshop_keys_next_to_upstream_ones() {
        let table: toml::Table = toml::from_str(
            r#"
[voice]
language = "de"
stt_endpointing_ms = 500
provider = "local"
local_model = "small.en"
auto_download = false
"#,
        )
        .unwrap();
        let cfg = VoiceProviderConfig::from_config_table(&table);
        assert_eq!(cfg.provider, Some(VoiceProvider::Local));
        assert_eq!(cfg.local_model(), WhisperModel::SmallEn);
        assert!(!cfg.auto_download);
        // Upstream's parser still accepts the same table (unknown keys ignored).
        let up = xai_grok_voice::VoiceConfig::from_config_table(&table, None);
        assert_eq!(up.language, "de");
        assert_eq!(up.stt_endpointing_ms, 500);
    }
}
