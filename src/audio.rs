use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

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

pub struct Recorder {
    stream: cpal::Stream,
    captured: Arc<Mutex<Vec<f32>>>,
    input_sample_rate: u32,
    input_channels: u16,
}

impl Recorder {
    pub fn start() -> Result<Self> {
        let host = cpal::default_host();
        let (device, supported) = select_input_device(&host)?;
        let stream_config: cpal::StreamConfig = supported.clone().into();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let err_fn = |err| eprintln!("cpal input error: {err}");

        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => {
                let captured = Arc::clone(&captured);
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| push_samples(&captured, data.iter().copied()),
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::I16 => {
                let captured = Arc::clone(&captured);
                device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _| push_samples(&captured, data.iter().map(i16_to_f32)),
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::U16 => {
                let captured = Arc::clone(&captured);
                device.build_input_stream(
                    &stream_config,
                    move |data: &[u16], _| push_samples(&captured, data.iter().map(u16_to_f32)),
                    err_fn,
                    None,
                )?
            }
            other => {
                anyhow::bail!("unsupported input sample format: {other:?}");
            }
        };

        stream.play().context("failed to start input stream")?;

        Ok(Self {
            stream,
            captured,
            input_sample_rate: stream_config.sample_rate,
            input_channels: stream_config.channels,
        })
    }

    pub fn finish(self) -> AudioPayload {
        let Recorder {
            stream,
            captured,
            input_sample_rate,
            input_channels,
        } = self;
        drop(stream);

        let raw = match Arc::try_unwrap(captured) {
            Ok(mutex) => mutex.into_inner().unwrap_or_default(),
            Err(shared) => shared.lock().map(|guard| guard.clone()).unwrap_or_default(),
        };
        let samples = normalize_to_mono_16k(&raw, input_sample_rate, input_channels);
        AudioPayload::new(TARGET_SAMPLE_RATE, TARGET_CHANNELS, samples)
    }
}

fn select_input_device(host: &cpal::Host) -> Result<(cpal::Device, cpal::SupportedStreamConfig)> {
    let mut attempts = Vec::new();

    if let Some(device) = host.default_input_device() {
        let name = device_name(&device);
        match device.default_input_config() {
            Ok(config) => return Ok((device, config)),
            Err(err) => attempts.push(format!("default device {name}: {err}")),
        }
    } else {
        attempts.push("no default input device available".to_string());
    }

    let devices = host
        .input_devices()
        .context("failed to enumerate input devices")?;

    for device in devices {
        let name = device_name(&device);
        match device.default_input_config() {
            Ok(config) => return Ok((device, config)),
            Err(err) => attempts.push(format!("input device {name}: {err}")),
        }
    }

    anyhow::bail!(
        "failed to find a usable input device: {}",
        attempts.join("; ")
    );
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|description| description.to_string())
        .unwrap_or_else(|_| "<unknown>".to_string())
}

fn push_samples<I>(captured: &Arc<Mutex<Vec<f32>>>, iter: I)
where
    I: IntoIterator<Item = f32>,
{
    if let Ok(mut buffer) = captured.lock() {
        buffer.extend(iter.into_iter().map(clamp_sample));
    }
}

fn i16_to_f32(sample: &i16) -> f32 {
    (*sample as f32) / i16::MAX as f32
}

fn u16_to_f32(sample: &u16) -> f32 {
    ((*sample as f32) - 32768.0) / 32768.0
}

fn clamp_sample(sample: f32) -> f32 {
    sample.clamp(-1.0, 1.0)
}

pub fn normalize_to_mono_16k(raw: &[f32], input_sample_rate: u32, input_channels: u16) -> Vec<f32> {
    if raw.is_empty() {
        return Vec::new();
    }

    let channels = usize::from(input_channels.max(1));
    let frame_count = raw.len() / channels;
    if frame_count == 0 {
        return Vec::new();
    }

    let mut mono = Vec::with_capacity(frame_count);
    for frame in raw.chunks_exact(channels) {
        let sum: f32 = frame.iter().copied().sum();
        mono.push(clamp_sample(sum / channels as f32));
    }

    if input_sample_rate == TARGET_SAMPLE_RATE {
        return mono;
    }

    resample_linear(&mono, input_sample_rate, TARGET_SAMPLE_RATE)
}

fn resample_linear(samples: &[f32], input_rate: u32, output_rate: u32) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    if input_rate == output_rate {
        return samples.iter().copied().map(clamp_sample).collect();
    }

    let ratio = output_rate as f64 / input_rate as f64;
    let output_len = ((samples.len() as f64) * ratio).round().max(1.0) as usize;
    let mut output = Vec::with_capacity(output_len);

    for index in 0..output_len {
        let src_pos = index as f64 / ratio;
        let left = src_pos.floor() as usize;
        let frac = (src_pos - left as f64) as f32;
        let a = samples
            .get(left)
            .copied()
            .unwrap_or_else(|| *samples.last().unwrap());
        let b = samples.get(left + 1).copied().unwrap_or(a);
        output.push(clamp_sample(a + (b - a) * frac));
    }

    output
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
    fn downmixes_and_resamples() {
        let raw = vec![
            0.0, 0.0, // frame 1
            1.0, -1.0, // frame 2
            0.5, 0.5, // frame 3
            -0.5, 0.5, // frame 4
        ];

        let samples = normalize_to_mono_16k(&raw, 8_000, 2);
        assert!(!samples.is_empty());
        assert!(samples.iter().all(|s| (-1.0..=1.0).contains(s)));
    }

    #[test]
    fn encodes_wav_header() {
        let wav = encode_wav_bytes(&[0.0, 1.0], TARGET_SAMPLE_RATE);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
    }
}
