//! The provider-neutral seam: [`SttBackend`] opens an [`SttSession`], a session eats 16 kHz mono
//! PCM16 LE and yields [`SttEvent`]s.
//!
//! The event vocabulary is deliberately the upstream xAI one (`transcript.partial` with
//! `is_final` / `speech_final`, then `transcript.done`), because the pager's dictation UX is
//! written against it:
//!
//! | event | pager behavior (unchanged) |
//! |---|---|
//! | `Partial { is_final: false, speech_final: false }` | live preview in the prompt overlay |
//! | `Partial { is_final: true, speech_final: false }` | chunk locked; appended to the preview prefix |
//! | `Partial { speech_final: true }` | utterance committed into the prompt at the caret |
//! | `Done { text }` | final commit after end-of-audio; session ends |
//! | `Error { message }` | toast, voice state reset |
//!
//! A backend that has no notion of a "locked chunk" simply never emits `is_final && !speech_final`.

use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;

pub use xai_grok_voice::VoiceError;

/// Normalized server-to-client STT event. See the module docs for the pager mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttEvent {
    /// Transcript for the current utterance so far.
    Partial {
        text: String,
        /// This chunk will not change again (streaming providers that lock ~3 s chunks).
        is_final: bool,
        /// End of utterance: the pager commits `text` into the prompt.
        speech_final: bool,
    },
    /// The last transcript after end-of-audio. The session yields `None` afterwards.
    Done { text: String },
    /// Fatal for this session.
    Error { message: String },
}

/// Per-session parameters, resolved by the pager from `[voice]` config before opening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttSessionOptions {
    /// Input rate of the PCM16 LE mono audio the session will receive.
    pub sample_rate: u32,
    /// Concrete language code (`en`, `de`, …) or `None` for auto-detect.
    pub language: Option<String>,
    /// Silence after speech that ends an utterance (`speech_final`).
    pub endpointing_ms: u32,
    /// Whether the caller wants non-final partials at all.
    pub interim_results: bool,
}

impl Default for SttSessionOptions {
    fn default() -> Self {
        Self {
            sample_rate: xai_grok_voice::config::DEFAULT_SAMPLE_RATE,
            language: Some("en".into()),
            endpointing_ms: 400,
            interim_results: true,
        }
    }
}

/// A live transcription session. Mirrors the upstream `StreamingSttSession` surface so the
/// pipeline reader loop is identical for every provider.
pub trait SttSession: Send {
    /// Sender for raw PCM16 LE chunks. `None` once [`Self::finish_audio`] was called.
    fn audio_sender(&self) -> Option<mpsc::Sender<Vec<u8>>>;
    /// No more audio will follow; the session should flush and emit a final `Done`.
    fn finish_audio(&mut self);
    /// Next event, or `None` when the session is over.
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<SttEvent>> + Send + '_>>;
}

/// Future returned by [`SttBackend::open`].
pub type OpenFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn SttSession>, VoiceError>> + Send + 'a>>;

/// A speech-to-text provider. One backend lives for the whole pager process and opens a new
/// session per push-to-talk press.
pub trait SttBackend: Send + Sync + 'static {
    /// Stable id used in config, the picker, and logs (`local`, `openai`, `groq`, `deepgram`, `xai`).
    fn id(&self) -> &'static str;
    /// Open a session; resolves once the session is ready for audio (model loaded / handshake done).
    fn open(&self, opts: SttSessionOptions) -> OpenFuture<'_>;
}
