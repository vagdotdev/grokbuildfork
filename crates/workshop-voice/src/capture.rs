//! Audio sources for the pipeline: the real microphone (upstream's platform backends) and a
//! PCM replay source used by tests, the bench example, and future `workshop voice doctor` checks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::backend::VoiceError;

/// Stops a running capture and releases the device.
pub trait CaptureHandle: Send {
    fn stop(self: Box<Self>);
}

/// Something that produces PCM16 LE mono chunks at `sample_rate` into `pcm_tx` until stopped.
pub trait AudioCapture: Send + Sync + 'static {
    /// Blocking is allowed (the pipeline calls this on the blocking pool); return once audio flows.
    fn spawn(
        &self,
        sample_rate: u32,
        pcm_tx: mpsc::Sender<Vec<u8>>,
    ) -> Result<Box<dyn CaptureHandle>, VoiceError>;
}

/// The user's microphone via upstream's capture backends (`pw-record`/`parec`/`arecord` on Linux,
/// the `__mic-capture` helper on macOS, cpal on Windows).
#[cfg(feature = "audio")]
#[derive(Debug, Default, Clone, Copy)]
pub struct MicCapture;

#[cfg(feature = "audio")]
impl AudioCapture for MicCapture {
    fn spawn(
        &self,
        sample_rate: u32,
        pcm_tx: mpsc::Sender<Vec<u8>>,
    ) -> Result<Box<dyn CaptureHandle>, VoiceError> {
        let handle = xai_grok_voice::audio::spawn_pcm_capture(sample_rate, pcm_tx)?;
        Ok(Box::new(UpstreamHandle(Some(handle))))
    }
}

#[cfg(feature = "audio")]
struct UpstreamHandle(Option<xai_grok_voice::audio::CaptureHandle>);

#[cfg(feature = "audio")]
impl CaptureHandle for UpstreamHandle {
    fn stop(mut self: Box<Self>) {
        if let Some(handle) = self.0.take() {
            handle.stop();
        }
    }
}

/// Replays a fixed PCM16 LE buffer as if it were a microphone.
///
/// `realtime` paces chunks at wall-clock speed (what a user's mic does); `false` streams the
/// buffer as fast as the consumer accepts, for throughput measurements. After the buffer ends the
/// source keeps producing paced silence (a mic in a quiet room) until stopped, so the consumer's
/// endpointing sees trailing silence either way.
#[derive(Debug, Clone)]
pub struct PcmReplayCapture {
    pcm: Arc<Vec<u8>>,
    chunk_ms: u32,
    realtime: bool,
}

impl PcmReplayCapture {
    pub fn new(pcm: Vec<u8>) -> Self {
        Self {
            pcm: Arc::new(pcm),
            chunk_ms: 20,
            realtime: true,
        }
    }

    pub fn realtime(mut self, realtime: bool) -> Self {
        self.realtime = realtime;
        self
    }
}

struct ReplayHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CaptureHandle for ReplayHandle {
    fn stop(mut self: Box<Self>) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl AudioCapture for PcmReplayCapture {
    fn spawn(
        &self,
        sample_rate: u32,
        pcm_tx: mpsc::Sender<Vec<u8>>,
    ) -> Result<Box<dyn CaptureHandle>, VoiceError> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        let pcm = Arc::clone(&self.pcm);
        let chunk_bytes = (sample_rate as usize * self.chunk_ms as usize / 1000) * 2;
        let chunk_period = Duration::from_millis(self.chunk_ms as u64);
        let realtime = self.realtime;
        let thread = std::thread::Builder::new()
            .name("voice-replay-capture".into())
            .spawn(move || {
                let mut next = std::time::Instant::now();
                for chunk in pcm.chunks(chunk_bytes) {
                    if stop_t.load(Ordering::Acquire)
                        || pcm_tx.blocking_send(chunk.to_vec()).is_err()
                    {
                        return;
                    }
                    if realtime {
                        next += chunk_period;
                        if let Some(wait) = next.checked_duration_since(std::time::Instant::now()) {
                            std::thread::sleep(wait);
                        }
                    }
                }
                // Buffer exhausted: keep the "device" open and quiet until stopped.
                let silence = vec![0u8; chunk_bytes];
                while !stop_t.load(Ordering::Acquire) {
                    if pcm_tx.blocking_send(silence.clone()).is_err() {
                        return;
                    }
                    std::thread::sleep(chunk_period);
                }
            })
            .map_err(|e| VoiceError::Config(format!("replay capture thread: {e}")))?;
        Ok(Box::new(ReplayHandle {
            stop,
            thread: Some(thread),
        }))
    }
}
