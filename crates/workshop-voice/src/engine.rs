//! The `voice-engine` helper as a warm sibling process.
//!
//! One helper per TUI process at most (it holds the model weights). It is started on the first
//! `/voice`, reused while alive, and exits by itself after five idle minutes (`--idle-timeout-secs`)
//! or when the TUI closes its stdin. A new press after that starts a fresh one. Two helpers never
//! hold two copies of the model: the warm slot is a process-global.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::Error;
use crate::protocol::{self, EngineMessage, FRAME_AUDIO, FRAME_QUIT, FRAME_STOP};

pub const ENGINE_BIN: &str = if cfg!(windows) {
    "voice-engine.exe"
} else {
    "voice-engine"
};
/// Override for tests, CI and unusual layouts.
pub const ENGINE_ENV: &str = "WORKSHOP_VOICE_ENGINE";
/// Cold start budget: model load (SSD, hundreds of ms; slow disks a few seconds) plus the
/// one-second probe decode, which on a weak CPU with the largest tier can take tens of seconds.
const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// The final decode of a long utterance on a slow CPU; beyond this the session reports an error.
const FINAL_TIMEOUT: Duration = Duration::from_secs(120);
const IDLE_TIMEOUT_SECS: u64 = 300;

/// Find the helper: `$WORKSHOP_VOICE_ENGINE`, beside the running executable (resolved and as
/// invoked, so both the versioned file and the `bin/` symlink layout work), `<workshop home>/bin`,
/// then `PATH`.
pub fn locate_engine() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(ENGINE_ENV).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join(ENGINE_BIN));
    }
    if let Some(argv0) = std::env::args_os().next() {
        let argv0 = PathBuf::from(argv0);
        if let Some(dir) = argv0.parent().filter(|d| !d.as_os_str().is_empty()) {
            candidates.push(dir.join(ENGINE_BIN));
        }
    }
    candidates.push(xai_dirs::grok_home().join("bin").join(ENGINE_BIN));
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(ENGINE_BIN)));
    }
    candidates.into_iter().find(|c| c.is_file())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    pub engine: PathBuf,
    pub model: PathBuf,
    pub tier: String,
    /// Concrete Whisper code passed to the helper as its default; `None` = detect per utterance.
    pub language: Option<String>,
    pub use_gpu: bool,
}

/// What a helper start reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyInfo {
    /// A fresh process was spawned (and probed) for this call.
    pub cold: bool,
    pub load_ms: u64,
    pub probe_ms: u64,
    pub gpu: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Partial { text: String, decode_ms: u64 },
    Final { text: String, decode_ms: u64 },
    Error(String),
}

struct Warm {
    child: tokio::process::Child,
    frames_tx: mpsc::Sender<Vec<u8>>,
    current: Arc<std::sync::Mutex<Option<mpsc::Sender<SessionEvent>>>>,
    config: EngineConfig,
    info: ReadyInfo,
    reader: tokio::task::JoinHandle<()>,
}

impl Warm {
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None)) && !self.reader.is_finished()
    }

    async fn stop(mut self) {
        let _ = self
            .frames_tx
            .try_send(protocol::encode_frame(FRAME_QUIT, &[]));
        // Give it a moment to exit on `quit`, then make sure.
        if tokio::time::timeout(Duration::from_millis(500), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
        self.reader.abort();
    }
}

static WARM: Mutex<Option<Warm>> = Mutex::const_new(None);

/// Whether a warm helper for exactly this configuration is running.
pub async fn warm_matches(config: &EngineConfig) -> bool {
    let mut slot = WARM.lock().await;
    match slot.as_mut() {
        Some(w) => w.alive() && w.config == *config,
        None => false,
    }
}

/// Start the helper for `config` unless one is already warm for it. Returns after `ready`.
pub async fn start_helper(config: &EngineConfig) -> Result<ReadyInfo, Error> {
    let mut slot = WARM.lock().await;
    if let Some(w) = slot.as_mut() {
        if w.alive() && w.config == *config {
            return Ok(ReadyInfo {
                cold: false,
                ..w.info
            });
        }
        // Different model, or it idled out: replace it.
        if let Some(old) = slot.take() {
            old.stop().await;
        }
    }
    let warm = spawn(config).await?;
    let info = warm.info;
    *slot = Some(warm);
    Ok(info)
}

/// Kill the warm helper (TUI shutdown, or before stepping down a tier).
pub async fn shutdown() {
    let mut slot = WARM.lock().await;
    if let Some(w) = slot.take() {
        w.stop().await;
    }
}

async fn spawn(config: &EngineConfig) -> Result<Warm, Error> {
    let mut cmd = tokio::process::Command::new(&config.engine);
    cmd.arg("--model")
        .arg(&config.model)
        .arg("--idle-timeout-secs")
        .arg(IDLE_TIMEOUT_SECS.to_string());
    if let Some(lang) = config.language.as_deref().filter(|l| *l != "auto") {
        cmd.arg("--language").arg(lang);
        cmd.arg("--probe-language").arg(lang);
    }
    if !config.use_gpu {
        cmd.arg("--no-gpu");
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    xai_tty_utils::detach_command(&mut cmd);
    // The child is owned by the warm slot: quit/killed on shutdown and tier changes, and it exits
    // by itself on stdin EOF or after the idle timeout.
    #[allow(clippy::disallowed_methods)]
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Engine(format!("could not start {}: {e}", config.engine.display())))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Engine("voice engine stdin unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Engine("voice engine stdout unavailable".into()))?;
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(target: "voice_engine", "{line}");
                if line.contains("probe") {
                    crate::doctor::record_engine_note(line);
                }
            }
        });
    }

    let (frames_tx, mut frames_rx) = mpsc::channel::<Vec<u8>>(256);
    tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(frame) = frames_rx.recv().await {
            if stdin.write_all(&frame).await.is_err() {
                break;
            }
        }
        let _ = stdin.shutdown().await;
    });

    let current: Arc<std::sync::Mutex<Option<mpsc::Sender<SessionEvent>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let (ready_tx, ready_rx) = oneshot::channel::<Result<ReadyInfo, Error>>();
    let route = Arc::clone(&current);
    let reader = tokio::spawn(async move {
        let mut ready_tx = Some(ready_tx);
        let mut lines = BufReader::new(stdout).lines();
        loop {
            let line = match lines.next_line().await {
                Ok(Some(l)) => l,
                _ => break,
            };
            let Some(msg) = protocol::parse_line(&line) else {
                continue;
            };
            match msg {
                EngineMessage::Ready {
                    load_ms,
                    probe_ms,
                    gpu,
                    ..
                } => {
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(Ok(ReadyInfo {
                            cold: true,
                            load_ms,
                            probe_ms,
                            gpu,
                        }));
                    }
                }
                EngineMessage::Partial { text, decode_ms } => {
                    deliver(&route, SessionEvent::Partial { text, decode_ms });
                }
                EngineMessage::Final {
                    text, decode_ms, ..
                } => {
                    deliver(&route, SessionEvent::Final { text, decode_ms });
                }
                EngineMessage::Error { message } => {
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(Err(Error::Model(message)));
                    } else {
                        deliver(&route, SessionEvent::Error(message));
                    }
                }
                EngineMessage::Unknown => {}
            }
        }
        // stdout closed: the helper is gone.
        deliver(&route, SessionEvent::Error("voice engine exited".into()));
        if let Some(tx) = ready_tx.take() {
            let _ = tx.send(Err(Error::Engine(
                "voice engine exited before it was ready".into(),
            )));
        }
    });

    let info = match tokio::time::timeout(READY_TIMEOUT, ready_rx).await {
        Ok(Ok(Ok(info))) => info,
        Ok(Ok(Err(e))) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            reader.abort();
            return Err(refine_start_error(e, child.try_wait().ok().flatten()));
        }
        Ok(Err(_)) | Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            reader.abort();
            return Err(Error::Engine(format!(
                "voice engine did not become ready within {} s",
                READY_TIMEOUT.as_secs()
            )));
        }
    };
    tracing::info!(
        tier = %config.tier,
        load_ms = info.load_ms,
        probe_ms = info.probe_ms,
        gpu = info.gpu,
        "voice engine ready"
    );
    Ok(Warm {
        child,
        frames_tx,
        current,
        config: config.clone(),
        info,
        reader,
    })
}

fn refine_start_error(e: Error, status: Option<std::process::ExitStatus>) -> Error {
    // Exit code 3 is the helper's "model could not be loaded"; anything else is the helper itself.
    match (&e, status.and_then(|s| s.code())) {
        (Error::Model(_), _) | (_, Some(3)) => Error::Model(e.to_string()),
        _ => e,
    }
}

fn deliver(route: &std::sync::Mutex<Option<mpsc::Sender<SessionEvent>>>, ev: SessionEvent) {
    if let Ok(guard) = route.lock()
        && let Some(tx) = guard.as_ref()
    {
        let _ = tx.try_send(ev);
    }
}

/// One utterance on the warm helper. Mirrors upstream's `StreamingSttSession` surface.
pub struct EngineSession {
    audio_tx: Option<mpsc::Sender<Vec<u8>>>,
    events_rx: mpsc::Receiver<SessionEvent>,
    finished: bool,
    done: bool,
}

/// Open an utterance on the running helper (call [`start_helper`] first).
pub async fn open_session(language: Option<&str>) -> Result<EngineSession, Error> {
    let mut slot = WARM.lock().await;
    let Some(warm) = slot.as_mut() else {
        return Err(Error::Engine("voice engine is not running".into()));
    };
    if !warm.alive() {
        return Err(Error::Engine("voice engine is not running".into()));
    }
    let (events_tx, events_rx) = mpsc::channel::<SessionEvent>(64);
    if let Ok(mut guard) = warm.current.lock() {
        *guard = Some(events_tx);
    }
    warm.frames_tx
        .send(protocol::start_frame(language))
        .await
        .map_err(|_| Error::Engine("voice engine stdin closed".into()))?;

    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(64);
    let frames = warm.frames_tx.clone();
    tokio::spawn(async move {
        while let Some(chunk) = audio_rx.recv().await {
            if frames
                .send(protocol::encode_frame(FRAME_AUDIO, &chunk))
                .await
                .is_err()
            {
                return;
            }
        }
        // Every audio sender dropped (`finish_audio` + mic stopped): finalize.
        let _ = frames.send(protocol::encode_frame(FRAME_STOP, &[])).await;
    });
    Ok(EngineSession {
        audio_tx: Some(audio_tx),
        events_rx,
        finished: false,
        done: false,
    })
}

impl EngineSession {
    pub fn audio_sender(&self) -> Option<mpsc::Sender<Vec<u8>>> {
        self.audio_tx.clone()
    }

    /// No more audio: the helper runs the final decode once the mic's sender is gone too.
    pub fn finish_audio(&mut self) {
        self.audio_tx.take();
        self.finished = true;
    }

    /// Next event; `None` after the final (or an error) was delivered.
    pub async fn recv(&mut self) -> Option<SessionEvent> {
        if self.done {
            return None;
        }
        let ev = if self.finished {
            match tokio::time::timeout(FINAL_TIMEOUT, self.events_rx.recv()).await {
                Ok(ev) => ev,
                Err(_) => Some(SessionEvent::Error(format!(
                    "voice engine did not finish within {} s",
                    FINAL_TIMEOUT.as_secs()
                ))),
            }
        } else {
            self.events_rx.recv().await
        };
        if matches!(
            ev,
            Some(SessionEvent::Final { .. }) | Some(SessionEvent::Error(_)) | None
        ) {
            self.done = true;
        }
        ev
    }
}

/// `voice-engine --version` output, for the installer's health print and `/doctor`. Blocking.
pub fn engine_version(engine: &Path) -> Result<String, Error> {
    let mut cmd = std::process::Command::new(engine);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    xai_tty_utils::detach_std_command(&mut cmd);
    // Short-lived probe; waited on below.
    #[allow(clippy::disallowed_methods)]
    let out = cmd
        .output()
        .map_err(|e| Error::Engine(format!("run {}: {e}", engine.display())))?;
    if !out.status.success() {
        return Err(Error::Engine(format!(
            "{} --version exited with {}",
            engine.display(),
            out.status
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
