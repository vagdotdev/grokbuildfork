//! Speech-to-text sessions: the Workshop local engine (default) and the inherited xAI streaming
//! client (`wss://…/v1/stt`, opt-in via `provider = "xai"`).

mod local;
mod streaming;
mod types;

pub use local::{LocalSttSession, whisper_language};
pub use streaming::{StreamingSttEvent, StreamingSttSession};
pub use types::{SttServerEvent, SttTranscriptPartial};

/// The session `pipeline.rs` drives; both variants speak [`StreamingSttEvent`].
pub enum SttSession {
    Local(LocalSttSession),
    Xai(StreamingSttSession),
}

impl SttSession {
    pub fn audio_sender(&self) -> Option<tokio::sync::mpsc::Sender<Vec<u8>>> {
        match self {
            SttSession::Local(s) => s.audio_sender(),
            SttSession::Xai(s) => s.audio_sender(),
        }
    }

    pub fn finish_audio(&mut self) {
        match self {
            SttSession::Local(s) => s.finish_audio(),
            SttSession::Xai(s) => s.finish_audio(),
        }
    }

    pub async fn recv(&mut self) -> Option<StreamingSttEvent> {
        match self {
            SttSession::Local(s) => s.recv().await,
            SttSession::Xai(s) => s.recv().await,
        }
    }
}
