//! Frame-level energy voice-activity detector with an adaptive noise floor.
//!
//! Good enough to endpoint dictation on a desk mic (the upstream xAI service does this
//! server-side with `endpointing=400`). The production upgrade is Silero VAD through
//! `whisper_rs::WhisperVadContext` (885 KB model, same download path); this module keeps the
//! prototype dependency-free and its thresholds measurable.

/// 20 ms at 16 kHz.
pub const FRAME_SAMPLES: usize = 320;

#[derive(Debug, Clone, PartialEq)]
pub struct VadConfig {
    /// A frame counts as speech when its RMS exceeds `noise_floor * ratio`.
    pub ratio: f32,
    /// …and at least this absolute RMS (full-scale 1.0), so digital silence never triggers.
    pub abs_min: f32,
    /// Consecutive speech frames needed to declare onset (debounce).
    pub onset_frames: usize,
    /// Initial noise floor before any frames were seen.
    pub initial_floor: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            ratio: 2.5,
            abs_min: 0.002,
            onset_frames: 3,
            initial_floor: 0.004,
        }
    }
}

#[derive(Debug)]
pub struct EnergyVad {
    cfg: VadConfig,
    noise_floor: f32,
    speech_run: usize,
    frames_seen: usize,
}

impl EnergyVad {
    pub fn new(cfg: VadConfig) -> Self {
        Self {
            noise_floor: cfg.initial_floor,
            cfg,
            speech_run: 0,
            frames_seen: 0,
        }
    }

    pub fn threshold(&self) -> f32 {
        (self.noise_floor * self.cfg.ratio).max(self.cfg.abs_min)
    }

    pub fn noise_floor(&self) -> f32 {
        self.noise_floor
    }

    /// Classify one frame. Returns `true` once `onset_frames` consecutive loud frames were seen,
    /// then for every loud frame until a quiet one resets the run.
    pub fn is_speech(&mut self, frame: &[f32]) -> bool {
        let rms = rms(frame);
        self.frames_seen += 1;
        if rms > self.threshold() {
            self.speech_run += 1;
        } else {
            self.speech_run = 0;
            // Track the quiet level: fast to drop, slow to rise, so a pause mid-sentence
            // doesn't lift the floor into the speech band.
            let alpha = if rms < self.noise_floor { 0.2 } else { 0.02 };
            self.noise_floor =
                (self.noise_floor + alpha * (rms - self.noise_floor)).clamp(1e-5, 0.05);
        }
        // The first frames calibrate the floor; treat them as quiet.
        self.frames_seen > 5 && self.speech_run >= self.cfg.onset_frames
    }
}

pub fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f32 = frame.iter().map(|s| s * s).sum();
    (sum / frame.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amp: f32) -> Vec<f32> {
        (0..FRAME_SAMPLES)
            .map(|i| amp * (i as f32 * 0.3).sin())
            .collect()
    }

    #[test]
    fn silence_never_triggers_and_speech_needs_onset_frames() {
        let mut vad = EnergyVad::new(VadConfig::default());
        for _ in 0..20 {
            assert!(!vad.is_speech(&vec![0.0; FRAME_SAMPLES]));
        }
        let loud = tone(0.2);
        assert!(!vad.is_speech(&loud));
        assert!(!vad.is_speech(&loud));
        assert!(
            vad.is_speech(&loud),
            "third consecutive loud frame is onset"
        );
        assert!(!vad.is_speech(&vec![0.0; FRAME_SAMPLES]));
    }

    #[test]
    fn floor_adapts_to_room_noise() {
        let mut vad = EnergyVad::new(VadConfig::default());
        let noise = tone(0.01);
        for _ in 0..200 {
            vad.is_speech(&noise);
        }
        let floor = vad.noise_floor();
        assert!((0.005..0.012).contains(&floor), "floor {floor}");
        assert!(
            vad.threshold() > rms(&noise),
            "noise must sit under the threshold"
        );
        let quiet_speech = tone(0.03);
        for _ in 0..3 {
            vad.is_speech(&quiet_speech);
        }
        assert!(vad.is_speech(&quiet_speech));
    }
}
