//! Local, offline speech-to-text: whisper.cpp via `whisper-rs`, streamed.
//!
//! The upstream service endpoints and re-transcribes server-side; here the same happens on the
//! user's machine:
//!
//! 1. PCM16 arrives in ~20–64 ms chunks and is framed at 20 ms for the energy VAD.
//! 2. Speech onset opens an utterance buffer seeded with a short pre-roll so the first syllable
//!    survives the debounce.
//! 3. While speaking, every `step_ms` the whole utterance so far is re-decoded greedily and shown
//!    as an interim (`is_final: false`).
//! 4. `endpointing_ms` of silence (or the utterance hitting `max_utterance_secs`) triggers a final
//!    decode of the utterance → `speech_final: true`, which the pager commits into the prompt.
//! 5. End of audio (push-to-talk release) decodes whatever is left → `Done`.
//!
//! Model files live under [`models::models_dir`] and are fetched on first use (see
//! [`LocalWhisperBackend::open`] for the UX contract around the download).

pub mod engine;
pub mod models;
pub mod text;
pub mod vad;

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::backend::{OpenFuture, SttBackend, SttEvent, SttSession, SttSessionOptions, VoiceError};
use engine::{DecodeConfig, DecodeMode, WhisperEngine, pcm16le_to_f32};
pub use models::WhisperModel;
use vad::{EnergyVad, FRAME_SAMPLES, VadConfig};

#[derive(Debug, Clone, PartialEq)]
pub struct LocalOptions {
    pub model: WhisperModel,
    pub models_dir: PathBuf,
    /// Ask whisper.cpp for its GPU backend (Metal on macOS). Ignored on CPU-only builds.
    pub use_gpu: bool,
    /// Interim re-decode cadence while the user is speaking.
    pub step_ms: u32,
    /// Force an utterance boundary after this much continuous speech (whisper's window is 30 s;
    /// committing earlier keeps the final-decode latency bounded).
    pub max_utterance_secs: f32,
    /// Audio kept before detected onset so the debounce doesn't clip the first word.
    pub pre_roll_ms: u32,
    /// Beam width for committed text; 1 = greedy (fastest).
    pub final_beam: usize,
    /// Encoder context for interim decodes (`0` = full). See `DecodeConfig::interim_audio_ctx`.
    pub interim_audio_ctx: i32,
    /// Floor applied to the session's `endpointing_ms`. The hosted service endpoints at 400 ms;
    /// a local decode costs ~1 s, so committing on every 400 ms pause wastes decodes and chops
    /// sentences into fragments. 600 ms keeps phrases together at +0.2 s commit latency.
    pub min_endpointing_ms: u32,
    pub vad: VadConfig,
    /// Fetch a missing model on first use. Off means `open` fails with instructions instead.
    pub auto_download: bool,
}

impl LocalOptions {
    pub fn new(model: WhisperModel) -> Self {
        Self {
            model,
            models_dir: models::models_dir(),
            use_gpu: cfg!(target_os = "macos"),
            step_ms: 1_200,
            max_utterance_secs: 15.0,
            pre_roll_ms: 400,
            final_beam: 1,
            interim_audio_ctx: 768,
            min_endpointing_ms: 600,
            vad: VadConfig::default(),
            auto_download: true,
        }
    }
}

#[derive(Debug, Clone)]
enum DownloadState {
    Idle,
    Running { received: u64, total: u64 },
    Failed(String),
}

/// One per process; loads the model lazily and shares it across sessions.
pub struct LocalWhisperBackend {
    opts: LocalOptions,
    engine: tokio::sync::Mutex<Option<Arc<WhisperEngine>>>,
    download: Arc<Mutex<DownloadState>>,
}

impl std::fmt::Debug for LocalWhisperBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalWhisperBackend")
            .field("model", &self.opts.model.id())
            .field("models_dir", &self.opts.models_dir)
            .finish()
    }
}

impl LocalWhisperBackend {
    pub fn new(opts: LocalOptions) -> Self {
        Self {
            opts,
            engine: tokio::sync::Mutex::new(None),
            download: Arc::new(Mutex::new(DownloadState::Idle)),
        }
    }

    pub fn options(&self) -> &LocalOptions {
        &self.opts
    }

    /// Loaded engine, loading it on first call. Fails (without blocking a push-to-talk press on a
    /// multi-hundred-MB download) when the model is not on disk yet; see [`Self::open`].
    pub async fn engine(&self) -> Result<Arc<WhisperEngine>, VoiceError> {
        let mut slot = self.engine.lock().await;
        if let Some(engine) = slot.as_ref() {
            return Ok(Arc::clone(engine));
        }
        let model = self.opts.model;
        let dir = self.opts.models_dir.clone();
        if !model.is_present_in(&dir) {
            return Err(self.missing_model_error());
        }
        let path = model.path_in(&dir);
        let use_gpu = self.opts.use_gpu;
        let engine =
            tokio::task::spawn_blocking(move || WhisperEngine::load(model, &path, use_gpu))
                .await
                .map_err(|e| VoiceError::Config(format!("model load task: {e}")))??;
        let engine = Arc::new(engine);
        *slot = Some(Arc::clone(&engine));
        Ok(engine)
    }

    /// Kick off (or report on) the first-use download. The press that triggered it fails with a
    /// progress message; the pager shows it as a toast and the user presses again when done.
    fn missing_model_error(&self) -> VoiceError {
        let model = self.opts.model;
        let human_size = format!("{} MB", model.size_bytes() / 1_000_000);
        if !self.opts.auto_download {
            return VoiceError::Config(format!(
                "voice model {} ({human_size}) is not downloaded and auto-download is off; \
                 run `workshop voice download {}`",
                model.id(),
                model.id()
            ));
        }
        let mut state = self.download.lock().unwrap_or_else(|p| p.into_inner());
        match &*state {
            DownloadState::Running { received, total } => {
                let pct = if *total > 0 {
                    received * 100 / total
                } else {
                    0
                };
                VoiceError::Config(format!(
                    "still downloading voice model {} ({pct}% of {human_size}); press again when done",
                    model.id()
                ))
            }
            DownloadState::Failed(msg) => {
                let msg = msg.clone();
                *state = DownloadState::Idle;
                VoiceError::Config(format!(
                    "voice model download failed: {msg}; press again to retry"
                ))
            }
            DownloadState::Idle => {
                *state = DownloadState::Running {
                    received: 0,
                    total: model.size_bytes(),
                };
                let progress = Arc::clone(&self.download);
                let dir = self.opts.models_dir.clone();
                let dir_display = dir.display().to_string();
                tokio::spawn(async move {
                    let progress_cb = {
                        let progress = Arc::clone(&progress);
                        move |received: u64, total: u64| {
                            if let Ok(mut s) = progress.lock() {
                                *s = DownloadState::Running { received, total };
                            }
                        }
                    };
                    let result = models::ensure_model(model, &dir, Some(&progress_cb)).await;
                    if let Ok(mut s) = progress.lock() {
                        *s = match result {
                            Ok(_) => DownloadState::Idle,
                            Err(e) => DownloadState::Failed(e.to_string()),
                        };
                    }
                });
                VoiceError::Config(format!(
                    "downloading voice model {} ({human_size}) to {dir_display}; press again when done",
                    model.id()
                ))
            }
        }
    }
}

impl SttBackend for LocalWhisperBackend {
    fn id(&self) -> &'static str {
        "local"
    }

    fn open(&self, opts: SttSessionOptions) -> OpenFuture<'_> {
        Box::pin(async move {
            if opts.sample_rate != engine::WHISPER_SAMPLE_RATE {
                return Err(VoiceError::Config(format!(
                    "local whisper needs {} Hz input, got {}",
                    engine::WHISPER_SAMPLE_RATE,
                    opts.sample_rate
                )));
            }
            let engine = self.engine().await?;
            let session = LocalWhisperSession::start(engine, &self.opts, &opts)?;
            Ok(Box::new(session) as Box<dyn SttSession>)
        })
    }
}

struct WorkerOptions {
    step: Duration,
    endpoint_frames: usize,
    max_utterance_samples: usize,
    pre_roll_samples: usize,
    interim_results: bool,
    vad: VadConfig,
    decode: DecodeConfig,
}

pub struct LocalWhisperSession {
    audio_tx: Option<mpsc::Sender<Vec<u8>>>,
    event_rx: mpsc::Receiver<SttEvent>,
    cancel: Arc<AtomicBool>,
    bridge: tokio::task::JoinHandle<()>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl LocalWhisperSession {
    fn start(
        engine: Arc<WhisperEngine>,
        local: &LocalOptions,
        session: &SttSessionOptions,
    ) -> Result<Self, VoiceError> {
        let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(64);
        let (event_tx, event_rx) = mpsc::channel::<SttEvent>(64);
        let (sync_tx, sync_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
        let cancel = Arc::new(AtomicBool::new(false));

        // Async → sync bridge so the whisper thread can block on a timeout-driven receive.
        let bridge = tokio::spawn(async move {
            while let Some(chunk) = audio_rx.recv().await {
                if sync_tx.send(chunk).is_err() {
                    break;
                }
            }
        });

        let rate = session.sample_rate as usize;
        let endpointing_ms = session.endpointing_ms.max(local.min_endpointing_ms);
        let worker_opts = WorkerOptions {
            step: Duration::from_millis(local.step_ms as u64),
            endpoint_frames: ((endpointing_ms as usize) / 20).max(1),
            max_utterance_samples: (local.max_utterance_secs * rate as f32) as usize,
            pre_roll_samples: rate * local.pre_roll_ms as usize / 1000,
            interim_results: session.interim_results,
            vad: local.vad.clone(),
            decode: DecodeConfig {
                language: session.language.clone(),
                final_beam: local.final_beam,
                interim_audio_ctx: local.interim_audio_ctx,
                ..DecodeConfig::default()
            },
        };
        let cancel_w = Arc::clone(&cancel);
        let worker = std::thread::Builder::new()
            .name("workshop-voice-whisper".into())
            .spawn(move || run_worker(engine, worker_opts, sync_rx, event_tx, cancel_w))
            .map_err(|e| VoiceError::Config(format!("whisper worker thread: {e}")))?;

        Ok(Self {
            audio_tx: Some(audio_tx),
            event_rx,
            cancel,
            bridge,
            worker: Some(worker),
        })
    }
}

impl SttSession for LocalWhisperSession {
    fn audio_sender(&self) -> Option<mpsc::Sender<Vec<u8>>> {
        self.audio_tx.clone()
    }

    fn finish_audio(&mut self) {
        self.audio_tx.take();
    }

    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<SttEvent>> + Send + '_>> {
        Box::pin(self.event_rx.recv())
    }
}

impl Drop for LocalWhisperSession {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.bridge.abort();
        // Don't join: a decode may be mid-flight and Drop can run on the runtime.
        drop(self.worker.take());
    }
}

/// The whisper thread. Owns the VAD, the utterance buffer, and the decode state.
fn run_worker(
    engine: Arc<WhisperEngine>,
    opts: WorkerOptions,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    events: mpsc::Sender<SttEvent>,
    cancel: Arc<AtomicBool>,
) {
    let mut state = match engine.create_state() {
        Ok(s) => s,
        Err(e) => {
            let _ = events.blocking_send(SttEvent::Error {
                message: e.to_string(),
            });
            return;
        }
    };
    let mut decode_cfg = opts.decode.clone();
    let mut vad = EnergyVad::new(opts.vad.clone());
    let mut pending: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 64);
    let mut pre_roll: VecDeque<f32> =
        VecDeque::with_capacity(opts.pre_roll_samples + FRAME_SAMPLES);
    let mut utterance: Vec<f32> = Vec::new();
    let mut in_speech = false;
    let mut silence_frames = 0usize;
    let mut last_interim_at = Instant::now();
    let mut last_interim_len = 0usize;
    let mut last_interim_text = String::new();
    let mut finished = false;

    let emit = |ev: SttEvent| events.blocking_send(ev).is_ok();

    loop {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(chunk) => pending.extend(pcm16le_to_f32(&chunk)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => finished = true,
        }
        while let Ok(chunk) = rx.try_recv() {
            pending.extend(pcm16le_to_f32(&chunk));
        }

        let mut consumed = 0usize;
        while let Some(frame) = pending.get(consumed..consumed + FRAME_SAMPLES) {
            consumed += FRAME_SAMPLES;
            let speech = vad.is_speech(frame);
            if in_speech {
                utterance.extend_from_slice(frame);
                if speech {
                    silence_frames = 0;
                } else {
                    silence_frames += 1;
                }
                let endpoint = silence_frames >= opts.endpoint_frames;
                // Past the cap, cut at the next quiet frame so a word isn't split mid-syllable
                let too_long = utterance.len() >= opts.max_utterance_samples && !speech;
                if endpoint || too_long {
                    let endpoint_at = Instant::now();
                    let decoded =
                        engine.decode(&mut state, &utterance, &decode_cfg, DecodeMode::Final);
                    match decoded {
                        Ok(d) => {
                            tracing::debug!(
                                audio_secs = d.audio_secs,
                                decode_ms = d.elapsed.as_millis() as u64,
                                emit_ms = endpoint_at.elapsed().as_millis() as u64,
                                reason = if endpoint { "silence" } else { "max_utterance" },
                                "local stt utterance final"
                            );
                            if !d.text.is_empty() {
                                decode_cfg.initial_prompt = Some(d.text.clone());
                                if !emit(SttEvent::Partial {
                                    text: d.text,
                                    is_final: true,
                                    speech_final: true,
                                }) {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = emit(SttEvent::Error {
                                message: e.to_string(),
                            });
                            return;
                        }
                    }
                    utterance.clear();
                    pre_roll.clear();
                    in_speech = false;
                    silence_frames = 0;
                    last_interim_len = 0;
                    last_interim_text.clear();
                }
            } else {
                pre_roll.extend(frame.iter().copied());
                while pre_roll.len() > opts.pre_roll_samples + FRAME_SAMPLES {
                    pre_roll.pop_front();
                }
                if speech {
                    in_speech = true;
                    silence_frames = 0;
                    utterance.extend(pre_roll.drain(..));
                    last_interim_at = Instant::now();
                    last_interim_len = 0;
                }
            }
        }
        pending.drain(..consumed);

        let interim_due = in_speech
            && opts.interim_results
            && utterance.len() >= engine::WHISPER_SAMPLE_RATE as usize
            && utterance.len() > last_interim_len
            && last_interim_at.elapsed() >= opts.step;
        if interim_due {
            match engine.decode(&mut state, &utterance, &decode_cfg, DecodeMode::Interim) {
                Ok(d) => {
                    tracing::debug!(
                        audio_secs = d.audio_secs,
                        decode_ms = d.elapsed.as_millis() as u64,
                        "local stt interim"
                    );
                    if !d.text.is_empty() && d.text != last_interim_text {
                        last_interim_text = d.text.clone();
                        if !emit(SttEvent::Partial {
                            text: d.text,
                            is_final: false,
                            speech_final: false,
                        }) {
                            return;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "local stt interim decode failed");
                }
            }
            last_interim_at = Instant::now();
            last_interim_len = utterance.len();
        }

        if finished {
            // Push-to-talk released: whatever is buffered is the last utterance.
            let has_speech = in_speech && utterance.len() >= FRAME_SAMPLES * 15; // ≥ 300 ms
            let text = if has_speech {
                match engine.decode(&mut state, &utterance, &decode_cfg, DecodeMode::Final) {
                    Ok(d) => {
                        tracing::debug!(
                            audio_secs = d.audio_secs,
                            decode_ms = d.elapsed.as_millis() as u64,
                            "local stt done"
                        );
                        d.text
                    }
                    Err(e) => {
                        let _ = emit(SttEvent::Error {
                            message: e.to_string(),
                        });
                        return;
                    }
                }
            } else {
                String::new()
            };
            let _ = emit(SttEvent::Done { text });
            return;
        }
    }
}
