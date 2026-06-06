use serde::{Deserialize, Serialize};

#[cfg(feature = "audio-capture")]
mod recorder;
#[cfg(feature = "audio-capture")]
pub use recorder::Recorder;

#[cfg(any(feature = "audio-capture", feature = "ui"))]
pub(crate) mod dsp;

#[cfg(any(feature = "audio-capture", feature = "ui"))]
pub(crate) mod vad;

pub mod trim;

#[cfg(not(feature = "audio-capture"))]
mod recorder_stub;
#[cfg(not(feature = "audio-capture"))]
pub use recorder_stub::Recorder;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;
pub const TARGET_CHANNELS: u16 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioPayload {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl AudioPayload {
    pub fn new(sample_rate: u32, channels: u16, samples: Vec<f32>) -> Self {
        Self {
            sample_rate,
            channels,
            samples,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioVisualizationSnapshot {
    pub frame_index: u64,
    pub sample_rate: u32,
    /// Loudness envelope from the DSP; the overlay overwrites it with the gated
    /// voice pulse (the value the shader reads as `audio.x`).
    pub level: f32,
    /// Onset/transient envelope; drives the swirl-rate accumulation.
    pub transient: f32,
    /// Sharper companion to `level` (the shader reads it as `audio.y`).
    pub peak: f32,
    /// Smoothed speech probability in `[0, 1]` from the voice-activity
    /// detector; the overlay uses it to gate the pulse.
    pub voice_probability: f32,
}

fn clamp_sample(sample: f32) -> f32 {
    sample.clamp(-1.0, 1.0)
}

pub fn encode_wav_bytes(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    let data_bytes = (samples.len() * 2) as u32;
    let riff_size = 36 + data_bytes;

    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_size.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());

    for &sample in samples {
        let pcm = (clamp_sample(sample) * i16::MAX as f32).round() as i16;
        bytes.extend_from_slice(&pcm.to_le_bytes());
    }

    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_wav_header() {
        let wav = encode_wav_bytes(&[0.0, 1.0], TARGET_SAMPLE_RATE);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
    }
}
