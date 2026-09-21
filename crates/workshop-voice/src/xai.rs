//! The inherited xAI streaming STT (`wss://api.x.ai/v1/stt`) as an [`SttBackend`].
//!
//! Compiled only with the `xai` feature and constructed only by the optional xAI provider plugin
//! after the user connected it. It proves the trait fits the wire protocol the UX was written
//! for: the upstream session is wrapped, not reimplemented.

use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;
use xai_grok_voice::stt::{StreamingSttEvent, StreamingSttSession};
use xai_grok_voice::{SharedVoiceAuth, VoiceConfig};

use crate::backend::{OpenFuture, SttBackend, SttEvent, SttSession, SttSessionOptions, VoiceError};

pub struct XaiSttBackend {
    config: VoiceConfig,
    auth: SharedVoiceAuth,
}

impl std::fmt::Debug for XaiSttBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XaiSttBackend")
            .field("api_base", &self.config.api_base)
            .finish()
    }
}

impl XaiSttBackend {
    /// `config` is upstream's `[voice]` config with the xAI provider's `api_base`; `auth` is the
    /// xAI connection's bearer provider (API key or session token), refreshed per open.
    pub fn new(config: VoiceConfig, auth: SharedVoiceAuth) -> Self {
        Self { config, auth }
    }
}

impl SttBackend for XaiSttBackend {
    fn id(&self) -> &'static str {
        "xai"
    }

    fn open(&self, opts: SttSessionOptions) -> OpenFuture<'_> {
        Box::pin(async move {
            let mut config = self.config.clone();
            config.sample_rate = opts.sample_rate;
            config.stt_endpointing_ms = opts.endpointing_ms;
            config.stt_interim_results = opts.interim_results;
            // Upstream resolves `auto` to a concrete catalog code at connect time
            config.language = opts
                .language
                .unwrap_or_else(|| xai_grok_voice::STT_LANGUAGE_AUTO.to_owned());
            let bearer = self.auth.bearer().await.ok_or_else(|| {
                VoiceError::Auth("xAI voice: the xAI connection has no usable credential".into())
            })?;
            let inner = StreamingSttSession::connect(&config, &bearer).await?;
            Ok(Box::new(XaiSession(inner)) as Box<dyn SttSession>)
        })
    }
}

struct XaiSession(StreamingSttSession);

impl SttSession for XaiSession {
    fn audio_sender(&self) -> Option<mpsc::Sender<Vec<u8>>> {
        self.0.audio_sender()
    }

    fn finish_audio(&mut self) {
        self.0.finish_audio();
    }

    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<SttEvent>> + Send + '_>> {
        Box::pin(async move {
            loop {
                return match self.0.recv().await? {
                    // `connect` already consumed the handshake; a late one is noise
                    StreamingSttEvent::Ready => continue,
                    StreamingSttEvent::Partial(p) => Some(SttEvent::Partial {
                        text: p.text,
                        is_final: p.is_final,
                        speech_final: p.speech_final,
                    }),
                    StreamingSttEvent::Done { text } => Some(SttEvent::Done { text }),
                    StreamingSttEvent::Error { message } => Some(SttEvent::Error { message }),
                };
            }
        })
    }
}
