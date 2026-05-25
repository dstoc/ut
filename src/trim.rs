use crate::audio::TARGET_SAMPLE_RATE;
use crate::config::RecordingConfig;

pub fn trim_silence(samples: &[f32], config: &RecordingConfig) -> Vec<f32> {
    if !config.trim_silence || samples.is_empty() {
        return samples.to_vec();
    }

    let sample_rate = TARGET_SAMPLE_RATE as usize;
    let padding = ms_to_samples(config.trim_padding_ms, sample_rate);
    let window = (sample_rate / 50).max(1);
    let step = (window / 2).max(1);
    let threshold = 0.01_f32;

    let start = trim_leading(samples, padding, window, step, threshold);
    let end = trim_trailing(samples, padding, window, step, threshold);

    if start >= end {
        Vec::new()
    } else {
        samples[start..end].to_vec()
    }
}

fn trim_leading(
    samples: &[f32],
    padding: usize,
    window: usize,
    step: usize,
    threshold: f32,
) -> usize {
    let mut offset = 0;
    while offset + window <= samples.len() {
        if window_rms(&samples[offset..offset + window]) > threshold {
            let boundary = first_active_sample(samples, offset, offset + window, threshold);
            return boundary.saturating_sub(padding);
        }
        offset += step;
    }
    samples.len()
}

fn trim_trailing(
    samples: &[f32],
    padding: usize,
    window: usize,
    step: usize,
    threshold: f32,
) -> usize {
    let mut offset = 0;
    while offset + window <= samples.len() {
        let end = samples.len() - offset;
        let start = end.saturating_sub(window);
        if window_rms(&samples[start..end]) > threshold {
            let boundary = last_active_sample(samples, start, end, threshold);
            return (boundary + padding).min(samples.len());
        }
        offset += step;
    }
    0
}

fn first_active_sample(samples: &[f32], start: usize, end: usize, threshold: f32) -> usize {
    for (index, sample) in samples[start..end].iter().enumerate() {
        if sample.abs() > threshold * 0.5 {
            return start + index;
        }
    }
    start
}

fn last_active_sample(samples: &[f32], start: usize, end: usize, threshold: f32) -> usize {
    for (index, sample) in samples[start..end].iter().enumerate().rev() {
        if sample.abs() > threshold * 0.5 {
            return start + index + 1;
        }
    }
    end
}

fn window_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_sq: f32 = samples.iter().map(|sample| sample * sample).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

fn ms_to_samples(ms: u32, sample_rate: usize) -> usize {
    ((ms as usize) * sample_rate) / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_leading_and_trailing_silence() {
        let config = RecordingConfig {
            max_seconds: 29,
            sample_rate: TARGET_SAMPLE_RATE,
            channels: 1,
            trim_silence: true,
            trim_padding_ms: 100,
        };

        let mut samples = vec![0.0; 1_000];
        samples.extend(vec![0.2; 1_000]);
        samples.extend(vec![0.0; 1_000]);

        let trimmed = trim_silence(&samples, &config);
        assert!(trimmed.len() < samples.len());
        assert!(trimmed.iter().any(|sample| sample.abs() > 0.1));
    }

    #[test]
    fn keeps_padding_on_both_sides_of_detected_speech() {
        let sample_rate = TARGET_SAMPLE_RATE as usize;
        let padding_samples = sample_rate / 10;
        let config = RecordingConfig {
            max_seconds: 29,
            sample_rate: TARGET_SAMPLE_RATE,
            channels: 1,
            trim_silence: true,
            trim_padding_ms: 100,
        };

        let mut samples = vec![0.0; sample_rate / 5];
        samples.extend(vec![0.2; sample_rate / 5]);
        samples.extend(vec![0.0; sample_rate / 5]);

        let trimmed = trim_silence(&samples, &config);

        assert!(trimmed.len() > sample_rate / 5);
        assert!(trimmed[..padding_samples]
            .iter()
            .all(|sample| sample.abs() < 0.001));
        assert!(trimmed[trimmed.len() - padding_samples..]
            .iter()
            .all(|sample| sample.abs() < 0.001));
    }
}
