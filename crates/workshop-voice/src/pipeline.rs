//! Provider-neutral port of upstream `xai_grok_voice::pipeline`.
//!
//! Same contract the pager already drives — [`VoiceCommand`] in, [`VoiceEvent`] out — with the
//! xAI WebSocket session swapped for any [`SttBackend`] and the mic for any [`AudioCapture`].
//! Behavior the pager relies on is kept verbatim: press/release racing, the pre-connect PCM
//! backlog, the 10 s no-speech watchdog, and the interim-stitching rules
//! (`is_final` deltas lock into a prefix, only `speech_final` / `Done` commit).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
pub use xai_grok_voice::{VoiceCommand, VoiceEvent};

use crate::backend::{SttBackend, SttEvent, SttSession, SttSessionOptions, VoiceError};
use crate::capture::{AudioCapture, CaptureHandle};

/// How long a session may run without any transcript before it is torn down.
/// Same value and rationale as upstream (`NO_SPEECH_TIMEOUT`).
pub const NO_SPEECH_TIMEOUT: Duration = Duration::from_secs(10);

/// Hard cap on the pre-open PCM backlog (memory safety), as upstream.
const BACKLOG_MAX_CHUNKS: usize = 1024;

#[derive(Debug, Clone)]
pub struct PipelineOptions {
    pub session: SttSessionOptions,
    pub no_speech_timeout: Duration,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            session: SttSessionOptions::default(),
            no_speech_timeout: NO_SPEECH_TIMEOUT,
        }
    }
}

/// Everything a pipeline needs that the pager resolves at spawn time.
pub struct PipelineDeps {
    pub backend: Arc<dyn SttBackend>,
    pub capture: Arc<dyn AudioCapture>,
    pub options: PipelineOptions,
}

struct ActivePtt {
    finish_tx: mpsc::Sender<()>,
    reader: JoinHandle<()>,
}

/// Run until [`VoiceCommand::Shutdown`]. Drop-in for `xai_grok_voice::run_voice_pipeline`.
pub async fn run_voice_pipeline(
    deps: PipelineDeps,
    mut cmd_rx: mpsc::Receiver<VoiceCommand>,
    event_tx: mpsc::Sender<VoiceEvent>,
) {
    let deps = Arc::new(deps);
    let mut active: Option<ActivePtt> = None;

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            VoiceCommand::Shutdown => break,
            VoiceCommand::PttPress => {
                if let Some(prev) = active.take() {
                    prev.reader.abort();
                }
                // Race the start against a release so a quick tap never leaves a hot mic
                tokio::select! {
                    biased;
                    session = open_session(&deps, &event_tx) => {
                        active = session;
                    }
                    next = cmd_rx.recv() => match next {
                        Some(VoiceCommand::PttRelease) => {}
                        Some(VoiceCommand::Shutdown) | None => break,
                        Some(VoiceCommand::PttPress) => {
                            active = open_session(&deps, &event_tx).await;
                        }
                    },
                }
            }
            VoiceCommand::PttRelease => {
                let Some(session) = active.as_ref() else {
                    continue;
                };
                let _ = session.finish_tx.send(()).await;
            }
        }
    }

    if let Some(session) = active {
        session.reader.abort();
    }
}

async fn open_session(
    deps: &Arc<PipelineDeps>,
    event_tx: &mpsc::Sender<VoiceEvent>,
) -> Option<ActivePtt> {
    match start_capture_session(deps, event_tx).await {
        Ok(session) => Some(session),
        Err(e) => {
            let _ = event_tx
                .send(VoiceEvent::Error {
                    message: e.to_string(),
                    hint: None,
                })
                .await;
            None
        }
    }
}

/// Buffer mic chunks until the session's audio sender arrives, then flush in order and stream live.
async fn forward_pcm(
    mut mic_rx: mpsc::Receiver<Vec<u8>>,
    mut audio_tx_rx: tokio::sync::oneshot::Receiver<mpsc::Sender<Vec<u8>>>,
) {
    let mut backlog: VecDeque<Vec<u8>> = VecDeque::new();
    let audio_tx = loop {
        tokio::select! {
            chunk = mic_rx.recv() => match chunk {
                Some(c) => {
                    if backlog.len() == BACKLOG_MAX_CHUNKS {
                        backlog.pop_front();
                    }
                    backlog.push_back(c);
                }
                None => return,
            },
            tx = &mut audio_tx_rx => match tx {
                Ok(tx) => break tx,
                Err(_) => return,
            },
        }
    };
    for chunk in backlog {
        if audio_tx.send(chunk).await.is_err() {
            return;
        }
    }
    while let Some(chunk) = mic_rx.recv().await {
        if audio_tx.send(chunk).await.is_err() {
            break;
        }
    }
}

fn no_speech_error() -> (String, Option<String>) {
    (
        "No speech was detected. Voice stopped.".to_owned(),
        Some(xai_grok_voice::probe::mic_fix_help().to_owned()),
    )
}

async fn start_capture_session(
    deps: &Arc<PipelineDeps>,
    event_tx: &mpsc::Sender<VoiceEvent>,
) -> Result<ActivePtt, VoiceError> {
    let sample_rate = deps.options.session.sample_rate;
    let (mic_tx, mic_rx) = mpsc::channel::<Vec<u8>>(64);
    let capture = Arc::clone(&deps.capture);
    // Open the device concurrently with the backend open (model load / cloud handshake), like upstream
    let capture_task = tokio::task::spawn_blocking(move || capture.spawn(sample_rate, mic_tx));

    let (audio_tx_tx, audio_tx_rx) = tokio::sync::oneshot::channel::<mpsc::Sender<Vec<u8>>>();
    tokio::spawn(forward_pcm(mic_rx, audio_tx_rx));

    let open = deps.backend.open(deps.options.session.clone());
    let (open_res, capture_res) = tokio::join!(open, capture_task);

    let capture = match capture_res {
        Ok(Ok(handle)) => handle,
        Ok(Err(e)) => return Err(e),
        Err(join_err) => {
            return Err(VoiceError::Config(format!(
                "voice capture task failed: {join_err}"
            )));
        }
    };
    let stt = open_res?;

    let audio_tx = stt
        .audio_sender()
        .ok_or_else(|| VoiceError::Stt("STT audio sender unavailable".into()))?;
    let _ = audio_tx_tx.send(audio_tx);

    let (finish_tx, finish_rx) = mpsc::channel::<()>(1);
    let reader = tokio::spawn(reader_loop(
        stt,
        capture,
        finish_rx,
        event_tx.clone(),
        deps.options.no_speech_timeout,
    ));
    Ok(ActivePtt { finish_tx, reader })
}

/// Upstream's reader loop against the trait. Kept as a free function so its stitching rules are
/// testable with a scripted [`SttSession`] and no audio device.
pub(crate) async fn reader_loop(
    mut stt: Box<dyn SttSession>,
    capture: Box<dyn CaptureHandle>,
    mut finish_rx: mpsc::Receiver<()>,
    out: mpsc::Sender<VoiceEvent>,
    no_speech_timeout: Duration,
) {
    let mut capture = Some(capture);
    let stop_capture = |capture: &mut Option<Box<dyn CaptureHandle>>| {
        if let Some(handle) = capture.take() {
            handle.stop();
        }
    };
    let no_speech_deadline = tokio::time::Instant::now() + no_speech_timeout;
    let mut awaiting_speech = true;
    let mut locked_prefix = String::new();
    loop {
        tokio::select! {
            msg = finish_rx.recv() => {
                if msg.is_some() {
                    awaiting_speech = false;
                    stop_capture(&mut capture);
                    stt.finish_audio();
                } else {
                    return;
                }
            }
            _ = tokio::time::sleep_until(no_speech_deadline), if awaiting_speech => {
                stop_capture(&mut capture);
                stt.finish_audio();
                let (message, hint) = no_speech_error();
                let _ = out.send(VoiceEvent::Error { message, hint }).await;
                return;
            }
            ev = stt.recv() => {
                match ev {
                    Some(SttEvent::Partial { text, is_final, speech_final }) => {
                        let trimmed = text.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        awaiting_speech = false;
                        let event = if speech_final {
                            locked_prefix.clear();
                            VoiceEvent::UtteranceFinal { text }
                        } else if is_final {
                            if !locked_prefix.is_empty() {
                                locked_prefix.push(' ');
                            }
                            locked_prefix.push_str(trimmed);
                            VoiceEvent::InterimTranscript { text: locked_prefix.clone() }
                        } else if locked_prefix.is_empty() {
                            VoiceEvent::InterimTranscript { text: trimmed.to_owned() }
                        } else {
                            VoiceEvent::InterimTranscript { text: format!("{locked_prefix} {trimmed}") }
                        };
                        if out.send(event).await.is_err() {
                            return;
                        }
                    }
                    Some(SttEvent::Done { text }) => {
                        locked_prefix.clear();
                        if !text.trim().is_empty() {
                            awaiting_speech = false;
                            let _ = out.send(VoiceEvent::UtteranceFinal { text }).await;
                        }
                    }
                    Some(SttEvent::Error { message }) => {
                        let _ = out.send(VoiceEvent::Error { message, hint: None }).await;
                        return;
                    }
                    None => return,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;

    use super::*;

    /// Scripted session: yields the given events, then ends.
    struct Scripted {
        audio_tx: Option<mpsc::Sender<Vec<u8>>>,
        events: VecDeque<SttEvent>,
        done: bool,
    }

    impl SttSession for Scripted {
        fn audio_sender(&self) -> Option<mpsc::Sender<Vec<u8>>> {
            self.audio_tx.clone()
        }
        fn finish_audio(&mut self) {
            self.audio_tx.take();
            self.done = true;
        }
        fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<SttEvent>> + Send + '_>> {
            Box::pin(async move {
                if let Some(ev) = self.events.pop_front() {
                    return Some(ev);
                }
                if self.done {
                    return None;
                }
                // Nothing scripted and not finished: park like an open socket
                std::future::pending::<()>().await;
                None
            })
        }
    }

    struct NoopHandle;
    impl CaptureHandle for NoopHandle {
        fn stop(self: Box<Self>) {}
    }

    fn partial(text: &str, is_final: bool, speech_final: bool) -> SttEvent {
        SttEvent::Partial {
            text: text.into(),
            is_final,
            speech_final,
        }
    }

    async fn run_script(events: Vec<SttEvent>, finish: bool) -> Vec<VoiceEvent> {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (finish_tx, finish_rx) = mpsc::channel(1);
        let (out_tx, mut out_rx) = mpsc::channel(32);
        let session = Scripted {
            audio_tx: Some(audio_tx),
            events: events.into(),
            done: false,
        };
        let reader = tokio::spawn(reader_loop(
            Box::new(session),
            Box::new(NoopHandle),
            finish_rx,
            out_tx,
            Duration::from_secs(10),
        ));
        if finish {
            finish_tx.send(()).await.unwrap();
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
        let mut got = Vec::new();
        while let Ok(ev) = out_rx.try_recv() {
            got.push(ev);
        }
        got
    }

    /// Interim → locked delta → speech_final commit, exactly as upstream stitches them.
    #[tokio::test]
    async fn stitches_locked_prefix_and_commits_on_speech_final() {
        let got = run_script(
            vec![
                partial("hello", false, false),
                partial("hello there", true, false),
                partial("how are", false, false),
                partial("hello there how are you", true, true),
                SttEvent::Done {
                    text: String::new(),
                },
            ],
            true,
        )
        .await;
        assert_eq!(
            got,
            vec![
                VoiceEvent::InterimTranscript {
                    text: "hello".into()
                },
                VoiceEvent::InterimTranscript {
                    text: "hello there".into()
                },
                VoiceEvent::InterimTranscript {
                    text: "hello there how are".into()
                },
                VoiceEvent::UtteranceFinal {
                    text: "hello there how are you".into()
                },
            ]
        );
    }

    /// A local backend that only emits whole-utterance interims never grows a locked prefix.
    #[tokio::test]
    async fn whole_utterance_interims_replace_each_other() {
        let got = run_script(
            vec![
                partial("and so", false, false),
                partial("and so my fellow", false, false),
                SttEvent::Done {
                    text: "And so my fellow Americans.".into(),
                },
            ],
            true,
        )
        .await;
        assert_eq!(
            got,
            vec![
                VoiceEvent::InterimTranscript {
                    text: "and so".into()
                },
                VoiceEvent::InterimTranscript {
                    text: "and so my fellow".into()
                },
                VoiceEvent::UtteranceFinal {
                    text: "And so my fellow Americans.".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn empty_partials_and_empty_done_emit_nothing() {
        let got = run_script(
            vec![
                partial("   ", false, false),
                SttEvent::Done { text: "  ".into() },
            ],
            true,
        )
        .await;
        assert!(got.is_empty(), "{got:?}");
    }

    #[tokio::test]
    async fn backend_error_becomes_toast_and_ends_session() {
        let got = run_script(
            vec![SttEvent::Error {
                message: "model exploded".into(),
            }],
            false,
        )
        .await;
        assert_eq!(
            got,
            vec![VoiceEvent::Error {
                message: "model exploded".into(),
                hint: None
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_speech_watchdog_fires_with_mic_hint() {
        let (audio_tx, _audio_rx) = mpsc::channel(4);
        let (_finish_tx, finish_rx) = mpsc::channel(1);
        let (out_tx, mut out_rx) = mpsc::channel(32);
        let session = Scripted {
            audio_tx: Some(audio_tx),
            events: VecDeque::new(),
            done: false,
        };
        let reader = tokio::spawn(reader_loop(
            Box::new(session),
            Box::new(NoopHandle),
            finish_rx,
            out_tx,
            Duration::from_secs(10),
        ));
        // Paused clock: the runtime auto-advances to the watchdog deadline once everything is idle.
        tokio::time::timeout(Duration::from_secs(60), reader)
            .await
            .expect("watchdog should end the reader")
            .expect("reader task panicked");
        let ev = out_rx.try_recv().expect("watchdog error");
        match ev {
            VoiceEvent::Error { message, hint } => {
                assert_eq!(message, "No speech was detected. Voice stopped.");
                assert!(hint.is_some());
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
