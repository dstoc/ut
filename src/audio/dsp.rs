use std::time::Duration;

use super::{clamp_sample, AudioVisualizationSnapshot};

#[cfg_attr(not(feature = "audio-capture"), allow(dead_code))]
pub(crate) fn build_visualization_snapshot(
    mono_frames: &[f32],
    sample_rate: u32,
    frame_index: u64,
) -> Option<AudioVisualizationSnapshot> {
    if mono_frames.is_empty() {
        return None;
    }

    let mut peak = 0.0f32;
    let mut envelope = 0.0f32;
    let mut envelope_sum = 0.0f32;
    let mut transient_peak = 0.0f32;
    let mut previous_envelope = 0.0f32;

    for &sample in mono_frames.iter() {
        let sample = clamp_sample(sample);
        let abs = sample.abs();
        peak = peak.max(abs);

        if abs >= envelope {
            envelope += (abs - envelope) * 0.35;
        } else {
            envelope += (abs - envelope) * 0.08;
        }
        envelope_sum += envelope;
        let delta = (envelope - previous_envelope).max(0.0);
        transient_peak = transient_peak.max(delta);
        previous_envelope = envelope;
    }

    let level = compress_activity(envelope_sum / mono_frames.len() as f32, 5.0);
    let transient = compress_activity(transient_peak, 8.0);

    Some(AudioVisualizationSnapshot {
        frame_index,
        sample_rate,
        level,
        transient,
        peak,
        voice_probability: 0.0,
    })
}

pub(crate) fn compress_activity(value: f32, gain: f32) -> f32 {
    if !value.is_finite() || value <= 0.0 {
        0.0
    } else {
        (1.0 - (-value * gain).exp()).clamp(0.0, 1.0)
    }
}

#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub(crate) fn normalize_level(value: f32, floor: f32, ceiling: f32) -> f32 {
    ((value - floor) / (ceiling - floor)).clamp(0.0, 1.0)
}

#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub(crate) fn lerp(current: f32, target: f32, t: f32) -> f32 {
    current + (target - current) * t
}

#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub(crate) fn smoothing_factor(dt: Duration, speed: f32) -> f32 {
    let seconds = dt.as_secs_f32().max(1.0 / 240.0);
    (1.0 - (-speed * seconds).exp()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::super::TARGET_SAMPLE_RATE;
    use super::*;

    #[test]
    fn builds_visualization_snapshot_from_mono_frames() {
        let snapshot = build_visualization_snapshot(&[0.0, 0.5, -0.5, 1.0], TARGET_SAMPLE_RATE, 42)
            .expect("snapshot");

        assert_eq!(snapshot.frame_index, 42);
        assert_eq!(snapshot.sample_rate, TARGET_SAMPLE_RATE);
        assert_eq!(snapshot.peak, 1.0);
        assert!(snapshot.level >= 0.0 && snapshot.level <= 1.0);
        assert!(snapshot.transient >= 0.0 && snapshot.transient <= 1.0);
    }

    #[test]
    fn sustained_tone_drives_level_without_spectral_bands() {
        let sample_rate = TARGET_SAMPLE_RATE;
        let frequency = 2_400.0f32;
        let samples: Vec<f32> = (0..512)
            .map(|index| {
                let phase = std::f32::consts::TAU * frequency * index as f32 / sample_rate as f32;
                phase.sin() * 0.8
            })
            .collect();

        let snapshot = build_visualization_snapshot(&samples, sample_rate, 7).expect("snapshot");

        assert!(snapshot.level > 0.0);
        assert!(snapshot.transient >= 0.0);
        assert!(snapshot.peak > 0.0);
    }
}
