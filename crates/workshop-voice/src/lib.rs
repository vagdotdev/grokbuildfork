//! Workshop voice: provider-neutral speech-to-text behind Grok Build's dictation UX.
//!
//! Upstream (`xai-grok-voice`) hardwires one provider: a bearer from the xAI login and a
//! WebSocket to `wss://api.x.ai/v1/stt`. This overlay keeps the user-visible feature —
//! `/voice`, Ctrl+Space / F8, the interim overlay, commit-at-caret — and swaps the transport
//! for an [`SttBackend`]:
//!
//! - [`local::LocalWhisperBackend`] — whisper.cpp on the user's machine (the default);
//! - BYOK cloud streaming STT (OpenAI, Groq, Deepgram) — designed in
//!   `internal/voice-stt-repoint-spec.md`, not yet implemented;
//! - [`xai::XaiSttBackend`] (feature `xai`) — the inherited service, only when the user
//!   connected the optional xAI provider.
//!
//! [`pipeline::run_voice_pipeline`] is a drop-in for `xai_grok_voice::run_voice_pipeline`: same
//! `VoiceCommand` in, same `VoiceEvent` out, so the pager's voice state machine is untouched.
//! No telemetry; no network unless a cloud provider is chosen or a model is downloaded.

#![deny(clippy::indexing_slicing)]

pub mod backend;
pub mod capture;
pub mod eval;
pub mod local;
pub mod pipeline;
pub mod provider;
#[cfg(feature = "xai")]
pub mod xai;

pub use backend::{SttBackend, SttEvent, SttSession, SttSessionOptions, VoiceError};
#[cfg(feature = "audio")]
pub use capture::MicCapture;
pub use capture::{AudioCapture, CaptureHandle, PcmReplayCapture};
pub use local::{LocalOptions, LocalWhisperBackend, WhisperModel};
pub use pipeline::{PipelineDeps, PipelineOptions, VoiceCommand, VoiceEvent, run_voice_pipeline};
pub use provider::{
    DEFAULT_LOCAL_MODEL, Resolution, ResolveContext, VoiceProvider, VoiceProviderConfig, resolve,
};

/// Build [`SttSessionOptions`] from upstream's `[voice]` config, resolving `auto` the way the
/// upstream client does for its own wire format while letting local/cloud backends auto-detect.
pub fn session_options_from_voice_config(cfg: &xai_grok_voice::VoiceConfig) -> SttSessionOptions {
    let language = if cfg.language.trim() == xai_grok_voice::STT_LANGUAGE_AUTO {
        None
    } else {
        Some(xai_grok_voice::language_for_api(&cfg.language).to_owned())
    };
    SttSessionOptions {
        sample_rate: cfg.sample_rate,
        language,
        endpointing_ms: cfg.stt_endpointing_ms,
        interim_results: cfg.stt_interim_results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_options_follow_upstream_voice_config() {
        let mut cfg = xai_grok_voice::VoiceConfig {
            language: "de".into(),
            stt_endpointing_ms: 650,
            ..xai_grok_voice::VoiceConfig::default()
        };
        let opts = session_options_from_voice_config(&cfg);
        assert_eq!(opts.language.as_deref(), Some("de"));
        assert_eq!(opts.endpointing_ms, 650);
        assert_eq!(opts.sample_rate, 16_000);

        cfg.language = "auto".into();
        assert_eq!(session_options_from_voice_config(&cfg).language, None);
    }
}
