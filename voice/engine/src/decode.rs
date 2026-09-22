//! whisper.cpp decode policy for dictation.
//!
//! - Interim: greedy, no temperature fallback, reduced encoder context (`audio_ctx` 768 ≈ 15 s),
//!   over the last few seconds of the open utterance. Preview only.
//! - Final: greedy with whisper.cpp's default temperature fallback over the whole utterance,
//!   full context. The only text the prompt commits.
//! - Both: `no_speech_thold` 0.6 plus a per-segment `no_speech_prob` filter and non-speech
//!   marker stripping, so a quiet room does not become "Thanks for watching".
//! - No initial prompt (v1): prompts are a common hallucination source.

use std::path::Path;
use std::time::{Duration, Instant};

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

pub const SAMPLE_RATE: usize = 16_000;
/// `whisper_full` returns nothing for < 1 s of audio; pad shorter buffers with silence.
const MIN_DECODE_SAMPLES: usize = SAMPLE_RATE * 11 / 10;
const NO_SPEECH_THRESHOLD: f32 = 0.6;
/// Encoder context for interim decodes; covers ~15 s at about half the full-context cost.
const INTERIM_AUDIO_CTX: i32 = 768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Interim,
    Final,
}

pub struct Decoded {
    pub text: String,
    pub elapsed: Duration,
    /// Language whisper settled on (the requested code, or the auto-detected one).
    pub language: Option<String>,
}

pub struct Engine {
    ctx: WhisperContext,
    threads: i32,
    gpu: bool,
}

impl Engine {
    pub fn load(model: &Path, use_gpu: bool, threads: usize) -> Result<(Self, Duration), String> {
        // Route (and drop) whisper.cpp / ggml C logs: stdout is the protocol channel and stderr
        // is reserved for the one slow-CPU line the parent may show.
        whisper_rs::install_logging_hooks();
        let mut params = WhisperContextParameters::default();
        params.use_gpu(use_gpu);
        params.flash_attn(use_gpu);
        let started = Instant::now();
        let ctx = WhisperContext::new_with_params(model, params)
            .map_err(|e| format!("model load failed for {}: {e}", model.display()))?;
        Ok((
            Self {
                ctx,
                threads: threads.clamp(1, 16) as i32,
                gpu: use_gpu,
            },
            started.elapsed(),
        ))
    }

    pub fn gpu(&self) -> bool {
        self.gpu
    }

    pub fn create_state(&self) -> Result<WhisperState, String> {
        self.ctx
            .create_state()
            .map_err(|e| format!("whisper state: {e}"))
    }

    /// Decode 16 kHz mono f32 samples. Blocking.
    pub fn decode(
        &self,
        state: &mut WhisperState,
        samples: &[f32],
        language: Option<&str>,
        mode: Mode,
    ) -> Result<Decoded, String> {
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

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_timestamps(true);
        params.set_single_segment(false);
        params.set_no_context(true);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_no_speech_thold(NO_SPEECH_THRESHOLD);
        // `None` is whisper.cpp's auto-detect; the literal "auto" never goes over this boundary.
        params.set_language(language.filter(|l| !l.is_empty() && *l != "auto"));
        if mode == Mode::Interim {
            params.set_temperature_inc(0.0);
            params.set_audio_ctx(INTERIM_AUDIO_CTX);
        }

        state
            .full(params, input)
            .map_err(|e| format!("whisper decode: {e}"))?;
        // Auto-detection costs a second encoder pass; the caller pins the detected code for the
        // rest of the utterance so only the first decode pays it.
        let language = match language {
            Some(l) if !l.is_empty() && l != "auto" => Some(l.to_owned()),
            _ => whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_owned),
        };

        let mut text = String::new();
        for segment in state.as_iter() {
            if segment.no_speech_probability() > NO_SPEECH_THRESHOLD {
                continue;
            }
            let raw = segment
                .to_str_lossy()
                .map_err(|e| format!("segment text: {e}"))?;
            let piece = clean_segment(&raw);
            if piece.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&piece);
        }
        Ok(Decoded {
            text,
            elapsed: started.elapsed(),
            language,
        })
    }
}

/// PCM s16le bytes → f32 in [-1, 1). A trailing odd byte is dropped.
pub fn pcm16le_to_f32(bytes: &[u8], out: &mut Vec<f32>) {
    out.reserve(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        out.push(i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0);
    }
}

/// Strip non-speech markers (`[BLANK_AUDIO]`, `(applause)`, `♪`) and collapse whitespace.
pub fn clean_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut square = 0usize;
    let mut round = 0usize;
    for ch in raw.chars() {
        match ch {
            '[' => square += 1,
            ']' => square = square.saturating_sub(1),
            '(' => round += 1,
            ')' => round = round.saturating_sub(1),
            '♪' | '♫' => {}
            _ if square > 0 || round > 0 => {}
            _ => out.push(ch),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_conversion() {
        let mut out = Vec::new();
        pcm16le_to_f32(&[0x00, 0x00, 0xff, 0x7f, 0x00, 0x80, 0x01], &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 32767.0 / 32768.0).abs() < 1e-6);
        assert_eq!(out[2], -1.0);
    }

    #[test]
    fn cleaning_drops_markers() {
        assert_eq!(clean_segment(" [BLANK_AUDIO]"), "");
        assert_eq!(clean_segment("(applause) thank you"), "thank you");
        assert_eq!(clean_segment("♪ la ♪  la"), "la la");
    }
}
