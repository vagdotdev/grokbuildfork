//! Workshop overlay: the local `voice-engine` helper behind the same session surface as
//! [`super::StreamingSttSession`], so `pipeline.rs` drives both the same way.
//!
//! Partials map to non-final interims (the preview), the helper's single full-utterance decode
//! maps to `Done` (the only text the prompt commits). Getting the helper ready — locating it,
//! verifying or downloading the pinned model, stepping down a model tier on a slow machine — is
//! `workshop_voice::session::open`; its progress lines surface as [`VoiceEvent::Status`].

use tokio::sync::mpsc;
use workshop_voice::{EngineSession, SessionEvent};

use super::{StreamingSttEvent, SttTranscriptPartial};
use crate::config::VoiceConfig;
use crate::error::VoiceError;
use crate::event::VoiceEvent;

pub struct LocalSttSession {
    inner: EngineSession,
}

impl LocalSttSession {
    /// Ready the helper (self-healing the model with progress on `event_tx`) and open an utterance.
    pub async fn open(
        config: &VoiceConfig,
        event_tx: &mpsc::Sender<VoiceEvent>,
    ) -> Result<Self, VoiceError> {
        let opts = workshop_voice::OpenOptions {
            voice_dir: None,
            tier_override: config.model.clone(),
            engine_path: config.engine_path.clone().map(std::path::PathBuf::from),
            language: whisper_language(&config.language),
        };
        let status = |text: String| {
            let _ = event_tx.try_send(VoiceEvent::Status { text });
        };
        let (inner, opened) = workshop_voice::open(&opts, &status).await.map_err(|e| {
            workshop_voice::doctor::record_last_error(e.to_string());
            match e {
                workshop_voice::Error::EngineMissing | workshop_voice::Error::Config(_) => {
                    VoiceError::Config(e.to_string())
                }
                other => VoiceError::Stt(other.to_string()),
            }
        })?;
        tracing::info!(
            tier = %opened.tier,
            cold = opened.ready.cold,
            load_ms = opened.ready.load_ms,
            probe_ms = opened.ready.probe_ms,
            downloaded = opened.downloaded,
            stepped_down_from = ?opened.stepped_down_from,
            "local voice session open"
        );
        Ok(Self { inner })
    }

    pub fn audio_sender(&self) -> Option<mpsc::Sender<Vec<u8>>> {
        self.inner.audio_sender()
    }

    pub fn finish_audio(&mut self) {
        self.inner.finish_audio();
    }

    pub async fn recv(&mut self) -> Option<StreamingSttEvent> {
        Some(match self.inner.recv().await? {
            SessionEvent::Partial { text, .. } => {
                StreamingSttEvent::Partial(SttTranscriptPartial {
                    text,
                    is_final: false,
                    speech_final: false,
                })
            }
            SessionEvent::Final { text, .. } => StreamingSttEvent::Done { text },
            SessionEvent::Error(message) => {
                workshop_voice::doctor::record_last_error(message.clone());
                StreamingSttEvent::Error { message }
            }
        })
    }
}

/// Catalog / `auto` preference → Whisper language code. `auto` (and anything unknown) becomes
/// `None`, which is whisper.cpp's detect-per-utterance; the literal `auto` never goes to the engine.
pub fn whisper_language(stored: &str) -> Option<String> {
    let canonical = crate::language::canonicalize_stt_language(Some(stored));
    if canonical == crate::language::STT_LANGUAGE_AUTO {
        return None;
    }
    // Whisper's table uses `tl` for Filipino/Tagalog; the catalog uses the ISO-639-2 `fil`.
    let code = match canonical {
        "fil" => "tl",
        other => other,
    };
    Some(code.to_owned())
}

#[cfg(test)]
mod tests {
    use super::whisper_language;

    #[test]
    fn auto_is_detection_and_catalog_codes_map_to_whisper() {
        assert_eq!(whisper_language("auto"), None);
        assert_eq!(whisper_language(" AUTO "), None);
        assert_eq!(whisper_language("de"), Some("de".into()));
        assert_eq!(whisper_language("fil"), Some("tl".into()));
        assert_eq!(whisper_language("en-US"), Some("en".into()));
        // Unknown codes fall back to the catalog default, never to the literal "auto"
        assert_eq!(whisper_language("xx"), Some("en".into()));
    }
}
