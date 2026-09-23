//! Workshop voice: local speech-to-text for `/voice` without xAI.
//!
//! The TUI keeps upstream's microphone child, keybinds, banner and prompt insertion. This crate
//! supplies what replaced the xAI WebSocket:
//!
//! - [`manifest`]: the pinned Whisper model tiers (`voice/MODEL.lock.json`, compiled in);
//! - [`store`]: the shared model directory, verification, and a resumable, retrying download;
//! - [`tier`]: silent per-machine model selection (Apple Silicon → turbo, else probe and step down);
//! - [`engine`]: the warm `voice-engine` helper process and its framed protocol;
//! - [`helper`]: fetching that helper from the release mirror when a default install has none;
//! - [`prefetch`]: the background setup (helper, then model) and its `Voice is getting ready` status;
//! - [`session`]: one `/voice` press end to end — resolve, self-heal, start, step down, open;
//! - [`doctor`]: the `/doctor` Voice facts.
//!
//! No speech model is linked here; the helper process holds the weights while it lives.

#![deny(clippy::indexing_slicing)]

pub mod doctor;
pub mod engine;
pub mod helper;
pub mod manifest;
pub mod prefetch;
pub mod protocol;
pub mod session;
pub mod store;
pub mod tier;

pub use engine::{EngineSession, ReadyInfo, SessionEvent, locate_engine};
pub use session::{OpenOptions, Opened, open};
pub use store::{ModelStatus, ModelStore, Progress};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Config(String),
    /// The `voice-engine` helper is not installed where the TUI looks for it.
    #[error(
        "voice engine not found (re-run the Workshop installer; it restores the helper without re-downloading the model)"
    )]
    EngineMissing,
    #[error("voice engine: {0}")]
    Engine(String),
    #[error("voice model: {0}")]
    Model(String),
    #[error("voice model download: {0}")]
    Download(String),
}

#[cfg(test)]
pub(crate) mod test_support {
    pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set/unset env vars for the closure, then restore. Callers hold [`ENV_LOCK`].
    pub fn with_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let saved: Vec<_> = vars
            .iter()
            .map(|(k, _)| ((*k).to_owned(), std::env::var_os(k)))
            .collect();
        for (k, v) in vars {
            // SAFETY: tests hold ENV_LOCK; no other thread touches these variables meanwhile.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        f();
        for (k, v) in saved {
            unsafe {
                match v {
                    Some(v) => std::env::set_var(&k, v),
                    None => std::env::remove_var(&k),
                }
            }
        }
    }
}
