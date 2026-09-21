//! Thin wrapper over whisper-rs: one loaded model (`WhisperEngine`, shared across sessions) and
//! per-session decode state. All C-side logging is routed into `tracing`.

use std::path::Path;
use std::sync::Once;
use std::time::{Duration, Instant};

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::backend::VoiceError;
use crate::local::models::WhisperModel;

/// Whisper's fixed input rate.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// `whisper_full` silently returns zero segments for < 1 s of audio; pad shorter inputs.
const MIN_DECODE_SAMPLES: usize = (WHISPER_SAMPLE_RATE as usize * 11) / 10;

static LOG_HOOKS: Once = Once::new();

/// How to decode: fast (interim previews) or careful (committed text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeMode {
    /// Greedy, no temperature fallback. Shown in the overlay, never committed.
    Interim,
    /// Greedy with whisper.cpp's default temperature fallback (or beam search when `beam > 1`).
    Final,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodeConfig {
    /// `None` = auto-detect (multilingual models only).
    pub language: Option<String>,
    /// Beam width for `Final`; `<= 1` means greedy.
    pub final_beam: usize,
    pub n_threads: usize,
    /// Text of the previous utterance, fed as the initial prompt for continuity (casing, names).
    pub initial_prompt: Option<String>,
    /// Encoder context for `Interim` decodes; `0` = whisper's full 1500 (30 s). 768 covers 15 s of
    /// audio at roughly half the encoder cost (the whisper.cpp `stream` example's setting).
    pub interim_audio_ctx: i32,
}

impl Default for DecodeConfig {
    fn default() -> Self {
        Self {
            language: Some("en".into()),
            final_beam: 1,
            n_threads: default_threads(),
            initial_prompt: None,
            interim_audio_ctx: 0,
        }
    }
}

pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub text: String,
    pub elapsed: Duration,
    /// Seconds of audio decoded (for real-time-factor reporting).
    pub audio_secs: f32,
    pub segments: usize,
}

pub struct WhisperEngine {
    ctx: WhisperContext,
    model: WhisperModel,
}

impl std::fmt::Debug for WhisperEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperEngine")
            .field("model", &self.model.id())
            .finish()
    }
}

impl WhisperEngine {
    /// Load `model` from `path`. `use_gpu` is honored on builds with a GPU backend (Metal on
    /// macOS); elsewhere whisper.cpp falls back to CPU on its own.
    pub fn load(model: WhisperModel, path: &Path, use_gpu: bool) -> Result<Self, VoiceError> {
        LOG_HOOKS.call_once(whisper_rs::install_logging_hooks);
        let mut params = WhisperContextParameters::default();
        params.use_gpu(use_gpu);
        params.flash_attn(use_gpu);
        let started = Instant::now();
        let ctx = WhisperContext::new_with_params(path, params).map_err(|e| {
            VoiceError::Config(format!(
                "load whisper model {} from {}: {e}",
                model.id(),
                path.display()
            ))
        })?;
        tracing::info!(
            model = model.id(),
            ms = started.elapsed().as_millis() as u64,
            whisper_cpp = whisper_rs::WHISPER_CPP_VERSION,
            "whisper model loaded"
        );
        Ok(Self { ctx, model })
    }

    pub fn model(&self) -> WhisperModel {
        self.model
    }

    pub fn is_multilingual(&self) -> bool {
        self.ctx.is_multilingual()
    }

    pub fn create_state(&self) -> Result<WhisperState, VoiceError> {
        self.ctx
            .create_state()
            .map_err(|e| VoiceError::Stt(format!("whisper state: {e}")))
    }

    /// Decode 16 kHz mono f32 samples. Blocking; call off the async runtime.
    pub fn decode(
        &self,
        state: &mut WhisperState,
        samples: &[f32],
        cfg: &DecodeConfig,
        mode: DecodeMode,
    ) -> Result<Decoded, VoiceError> {
        let started = Instant::now();
        let padded: Vec<f32>;
        let input: &[f32] = if samples.len() < MIN_DECODE_SAMPLES {
            padded = {
                let mut v = samples.to_vec();
                v.resize(MIN_DECODE_SAMPLES, 0.0);
                v
            };
            &padded
        } else {
            samples
        };

        let strategy = match mode {
            DecodeMode::Final if cfg.final_beam > 1 => SamplingStrategy::BeamSearch {
                beam_size: cfg.final_beam as i32,
                patience: -1.0,
            },
            _ => SamplingStrategy::Greedy { best_of: 1 },
        };
        let mut params = FullParams::new(strategy);
        params.set_n_threads(cfg.n_threads as i32);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_timestamps(true);
        params.set_single_segment(false);
        params.set_no_context(true);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        // English-only checkpoints reject other codes; multilingual ones take a code or "auto".
        let language: Option<&str> = if self.model.english_only() {
            Some("en")
        } else {
            Some(cfg.language.as_deref().unwrap_or("auto"))
        };
        params.set_language(language);
        if mode == DecodeMode::Interim {
            // No temperature fallback: a preview is not worth a second pass.
            params.set_temperature_inc(0.0);
            if cfg.interim_audio_ctx > 0 {
                params.set_audio_ctx(cfg.interim_audio_ctx);
            }
        }
        if let Some(prompt) = cfg.initial_prompt.as_deref().filter(|p| !p.is_empty()) {
            params.set_initial_prompt(prompt);
        }

        state
            .full(params, input)
            .map_err(|e| VoiceError::Stt(format!("whisper decode: {e}")))?;

        let mut text = String::new();
        let mut segments = 0usize;
        for segment in state.as_iter() {
            // Whisper emits confident garbage on silence; the no-speech head flags it.
            if segment.no_speech_probability() > 0.6 {
                continue;
            }
            let piece = segment
                .to_str_lossy()
                .map_err(|e| VoiceError::Stt(format!("whisper segment text: {e}")))?;
            let piece = super::text::clean_segment(&piece);
            if piece.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&piece);
            segments += 1;
        }
        Ok(Decoded {
            text,
            elapsed: started.elapsed(),
            audio_secs: samples.len() as f32 / WHISPER_SAMPLE_RATE as f32,
            segments,
        })
    }
}

/// PCM16 LE bytes → f32 samples in [-1, 1). Odd trailing bytes are dropped.
pub fn pcm16le_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|b| {
            let mut pair = [0u8; 2];
            pair.copy_from_slice(b);
            i16::from_le_bytes(pair) as f32 / 32768.0
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_conversion_is_little_endian_and_scaled() {
        let bytes = [0x00, 0x00, 0xff, 0x7f, 0x00, 0x80, 0x01];
        let out = pcm16le_to_f32(&bytes);
        assert_eq!(out.len(), 3);
        assert_eq!(out.first().copied(), Some(0.0));
        assert!((out.get(1).copied().unwrap_or(9.0) - (32767.0 / 32768.0)).abs() < 1e-6);
        assert_eq!(out.get(2).copied(), Some(-1.0));
    }
}
