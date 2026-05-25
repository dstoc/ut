use std::time::Duration;

use super::{clamp_sample, AudioVisualizationSnapshot, VISUALIZATION_BAND_COUNT, VISUALIZATION_BIN_COUNT};

#[cfg_attr(not(feature = "audio-capture"), allow(dead_code))]
pub(crate) fn build_visualization_snapshot(
    mono_frames: &[f32],
    sample_rate: u32,
    frame_index: u64,
) -> Option<AudioVisualizationSnapshot> {
    if mono_frames.is_empty() {
        return None;
    }

    let mut waveform = [0.0f32; VISUALIZATION_BIN_COUNT];
    let mut waveform_counts = [0u16; VISUALIZATION_BIN_COUNT];
    let mut rms_sum = 0.0f32;
    let mut peak = 0.0f32;
    let mut envelope = 0.0f32;
    let mut envelope_sum = 0.0f32;
    let mut transient_peak = 0.0f32;
    let mut previous_envelope = 0.0f32;

    for (index, &sample) in mono_frames.iter().enumerate() {
        let sample = clamp_sample(sample);
        let abs = sample.abs();
        rms_sum += sample * sample;
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

        let bin = (index * VISUALIZATION_BIN_COUNT) / mono_frames.len();
        let bin = bin.min(VISUALIZATION_BIN_COUNT - 1);
        waveform[bin] += abs;
        waveform_counts[bin] = waveform_counts[bin].saturating_add(1);
    }

    for (value, count) in waveform.iter_mut().zip(waveform_counts) {
        if count > 0 {
            *value /= count as f32;
        }
    }

    let rms = (rms_sum / mono_frames.len() as f32).sqrt();
    let level = compress_activity(envelope_sum / mono_frames.len() as f32, 5.0);
    let transient = compress_activity(transient_peak, 8.0);
    let bands = compute_band_energies(mono_frames, sample_rate);

    Some(AudioVisualizationSnapshot {
        frame_index,
        sample_rate,
        rms,
        peak,
        level,
        transient,
        bands,
        waveform,
    })
}

#[cfg_attr(not(feature = "audio-capture"), allow(dead_code))]
pub(crate) fn compute_band_energies(
    sample_slice: &[f32],
    sample_rate: u32,
) -> [f32; VISUALIZATION_BAND_COUNT] {
    let mut bands = [0.0f32; VISUALIZATION_BAND_COUNT];
    if sample_slice.len() < 2 || sample_rate == 0 {
        return bands;
    }

    let nyquist = sample_rate as f32 * 0.5;
    let centers = [0.04f32, 0.08, 0.15, 0.28, 0.48, 0.74];

    for (band_index, center_fraction) in centers.into_iter().enumerate() {
        let center_hz = (nyquist * center_fraction).clamp(30.0, nyquist * 0.95);
        let probe_frequencies = [
            center_hz * 0.75,
            center_hz,
            (center_hz * 1.35).min(nyquist * 0.95),
        ];
        let mut energy = 0.0f32;

        for frequency in probe_frequencies {
            energy += goertzel_energy(sample_slice, sample_rate, frequency);
        }

        bands[band_index] = compress_activity(energy / 3.0, 12.0);
    }

    bands
}

#[cfg_attr(not(feature = "audio-capture"), allow(dead_code))]
pub(crate) fn goertzel_energy(samples: &[f32], sample_rate: u32, frequency: f32) -> f32 {
    if samples.len() < 2 || sample_rate == 0 || frequency <= 0.0 {
        return 0.0;
    }

    let omega = std::f32::consts::TAU * frequency / sample_rate as f32;
    let coeff = 2.0 * omega.cos();
    let mut q1 = 0.0f32;
    let mut q2 = 0.0f32;
    let sample_count = samples.len() as f32;
    let window_denominator = (samples.len() - 1) as f32;

    for (index, &sample) in samples.iter().enumerate() {
        let window = if window_denominator == 0.0 {
            1.0
        } else {
            0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / window_denominator).cos()
        };
        let x = clamp_sample(sample) * window;
        let q0 = coeff * q1 - q2 + x;
        q2 = q1;
        q1 = q0;
    }

    let real = q1 - q2 * omega.cos();
    let imag = q2 * omega.sin();
    (real * real + imag * imag).sqrt() / sample_count
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
pub(crate) fn normalize_level_signed(value: f32, floor: f32, ceiling: f32) -> f32 {
    let sign = value.signum();
    sign * normalize_level(value.abs(), floor, ceiling)
}

#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub(crate) fn lerp(current: f32, target: f32, t: f32) -> f32 {
    current + (target - current) * t
}

#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub(crate) fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge0 == edge1 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }

    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub(crate) fn smoothing_factor(dt: Duration, speed: f32) -> f32 {
    let seconds = dt.as_secs_f32().max(1.0 / 240.0);
    (1.0 - (-speed * seconds).exp()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::TARGET_SAMPLE_RATE;

    #[test]
    fn builds_visualization_snapshot_from_mono_frames() {
        let snapshot =
            build_visualization_snapshot(&[0.0, 0.5, -0.5, 1.0], TARGET_SAMPLE_RATE, 42)
                .expect("snapshot");

        assert_eq!(snapshot.frame_index, 42);
        assert_eq!(snapshot.sample_rate, TARGET_SAMPLE_RATE);
        assert!((snapshot.rms - 0.61237246).abs() < 1e-6);
        assert_eq!(snapshot.peak, 1.0);
        assert!(snapshot.level >= 0.0 && snapshot.level <= 1.0);
        assert!(snapshot.transient >= 0.0 && snapshot.transient <= 1.0);
        assert_eq!(snapshot.bands.len(), VISUALIZATION_BAND_COUNT);
        assert!(snapshot.bands.iter().all(|band| (0.0..=1.0).contains(band)));
        assert_eq!(snapshot.waveform.len(), VISUALIZATION_BIN_COUNT);
        assert!(snapshot
            .waveform
            .iter()
            .all(|value| (0.0..=1.0).contains(value)));
        assert!(snapshot.waveform.iter().any(|value| *value > 0.0));
    }

    #[test]
    fn builds_visualization_snapshot_with_band_energy() {
        let sample_rate = TARGET_SAMPLE_RATE;
        let frequency = 2_400.0f32;
        let samples: Vec<f32> = (0..512)
            .map(|index| {
                let phase =
                    std::f32::consts::TAU * frequency * index as f32 / sample_rate as f32;
                phase.sin() * 0.8
            })
            .collect();

        let snapshot = build_visualization_snapshot(&samples, sample_rate, 7).expect("snapshot");

        assert!(snapshot.bands.iter().any(|band| *band > 0.0));
        assert!(snapshot.bands[4] > snapshot.bands[0]);
        assert!(snapshot.level > 0.0);
        assert!(snapshot.transient >= 0.0);
    }
}
