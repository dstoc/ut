//! Voice-activity detection for the status visualizer, built on RNNoise via
//! the `nnnoiseless` crate. Produces a smoothed speech probability that the
//! overlay uses to react to voice while ignoring background noise and hiss.

// Only the recorder (audio-capture) drives this; under a UI-only build it is
// compiled for tests but otherwise unused.
#![cfg_attr(not(feature = "audio-capture"), allow(dead_code))]

use nnnoiseless::DenoiseState;

/// RNNoise runs at a fixed 48 kHz on 480-sample frames; incoming audio is
/// resampled to this rate before analysis.
const VAD_SAMPLE_RATE: u32 = 48_000;

/// nnnoiseless expects samples in 16-bit PCM range, not `[-1.0, 1.0]`.
const I16_SCALE: f32 = 32_768.0;

/// Smoothing: rise quickly when speech appears, fall slowly so the trace does
/// not flicker between words.
const ATTACK: f32 = 0.60;
const RELEASE: f32 = 0.08;

/// A frame whose raw probability clears this counts as speech and (re)arms the
/// hangover.
const SPEECH_THRESHOLD: f32 = 0.5;

/// Keep treating audio as speech for this many 10 ms frames after it drops,
/// bridging the short gaps between words. 30 frames = 300 ms.
const HANGOVER_FRAMES: u32 = 30;

/// Streaming voice-activity detector.
///
/// Feed it the captured mono audio chunk by chunk via [`observe`]; it resamples
/// to 48 kHz, runs RNNoise on fixed 480-sample frames, and returns a smoothed
/// speech probability in `[0, 1]` suitable for gating the visualizer.
///
/// [`observe`]: VoiceActivityDetector::observe
pub struct VoiceActivityDetector {
    denoise: Box<DenoiseState<'static>>,
    /// Resampled-to-48k samples awaiting a full analysis frame.
    pending: Vec<f32>,
    /// Scratch denoised output; required by `process_frame`, otherwise unused.
    scratch: [f32; DenoiseState::FRAME_SIZE],
    probability: f32,
    hangover: u32,
}

impl VoiceActivityDetector {
    pub fn new() -> Self {
        Self {
            denoise: DenoiseState::new(),
            pending: Vec::with_capacity(DenoiseState::FRAME_SIZE * 2),
            scratch: [0.0; DenoiseState::FRAME_SIZE],
            probability: 0.0,
            hangover: 0,
        }
    }

    /// Observe a chunk of mono samples (range `[-1, 1]`) captured at
    /// `sample_rate`, returning the current smoothed speech probability.
    pub fn observe(&mut self, mono: &[f32], sample_rate: u32) -> f32 {
        resample_into(mono, sample_rate, VAD_SAMPLE_RATE, &mut self.pending);

        let frame_size = DenoiseState::FRAME_SIZE;
        let mut frame = [0.0f32; DenoiseState::FRAME_SIZE];
        let mut consumed = 0;
        while self.pending.len() - consumed >= frame_size {
            let chunk = &self.pending[consumed..consumed + frame_size];
            for (dst, &src) in frame.iter_mut().zip(chunk) {
                *dst = src * I16_SCALE;
            }
            consumed += frame_size;
            let probability = self.denoise.process_frame(&mut self.scratch, &frame);
            self.integrate(probability);
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }

        self.probability
    }

    /// Fold a raw per-frame probability into the smoothed value with fast
    /// attack, slow release, and a hangover hold.
    fn integrate(&mut self, raw: f32) {
        let raw = raw.clamp(0.0, 1.0);
        if raw >= SPEECH_THRESHOLD {
            self.hangover = HANGOVER_FRAMES;
        } else if self.hangover > 0 {
            self.hangover -= 1;
        }

        let target = if self.hangover > 0 {
            raw.max(SPEECH_THRESHOLD)
        } else {
            raw
        };
        let rate = if target > self.probability {
            ATTACK
        } else {
            RELEASE
        };
        self.probability += (target - self.probability) * rate;
    }
}

impl Default for VoiceActivityDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Append `input` (sampled at `source_rate`) to `out`, resampled to
/// `target_rate` with linear interpolation. Each chunk is resampled
/// independently; the tiny phase discontinuity at chunk boundaries is
/// irrelevant for voice detection.
fn resample_into(input: &[f32], source_rate: u32, target_rate: u32, out: &mut Vec<f32>) {
    if input.is_empty() {
        return;
    }
    if source_rate == target_rate || source_rate == 0 {
        out.extend_from_slice(input);
        return;
    }

    let ratio = target_rate as f64 / source_rate as f64;
    let out_len = ((input.len() as f64) * ratio).round().max(1.0) as usize;
    let last = *input.last().unwrap();
    out.reserve(out_len);
    for index in 0..out_len {
        let src_pos = index as f64 / ratio;
        let left = src_pos.floor() as usize;
        let frac = (src_pos - left as f64) as f32;
        let a = input.get(left).copied().unwrap_or(last);
        let b = input.get(left + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_yields_low_probability() {
        let mut vad = VoiceActivityDetector::new();
        // A few seconds of silence at 48 kHz.
        let silence = vec![0.0f32; 48_000];
        let probability = vad.observe(&silence, 48_000);
        assert!(probability < 0.1, "silence probability was {probability}");
    }

    #[test]
    fn probability_stays_in_unit_range() {
        let mut vad = VoiceActivityDetector::new();
        let tone: Vec<f32> = (0..48_000)
            .map(|i| (std::f32::consts::TAU * 200.0 * i as f32 / 48_000.0).sin() * 0.3)
            .collect();
        let probability = vad.observe(&tone, 48_000);
        assert!((0.0..=1.0).contains(&probability));
    }

    #[test]
    fn partial_chunk_accumulates_without_panicking() {
        let mut vad = VoiceActivityDetector::new();
        // Fewer samples than one 480-frame: nothing to analyze yet.
        let probability = vad.observe(&[0.0; 100], 48_000);
        assert_eq!(probability, 0.0);
        assert_eq!(vad.pending.len(), 100);
    }

    #[test]
    fn resample_changes_length_by_ratio() {
        let mut out = Vec::new();
        resample_into(&[0.0; 16_000], 16_000, 48_000, &mut out);
        // 16k -> 48k is 3x.
        assert!((out.len() as i64 - 48_000).abs() <= 1);
    }
}
