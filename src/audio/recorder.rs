use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc::SyncSender,
    Arc, Mutex,
};

use super::dsp;
use super::{
    clamp_sample, AudioPayload, AudioVisualizationSnapshot, TARGET_CHANNELS, TARGET_SAMPLE_RATE,
};

pub struct Recorder {
    stream: cpal::Stream,
    captured: Arc<Mutex<Vec<f32>>>,
    input_sample_rate: u32,
    input_channels: u16,
}

impl Recorder {
    pub fn start() -> Result<Self> {
        Self::start_with_visualization(None)
    }

    pub fn start_with_visualization(
        visualization_sink: Option<SyncSender<AudioVisualizationSnapshot>>,
    ) -> Result<Self> {
        let host = cpal::default_host();
        let (device, supported) = select_input_device(&host)?;
        let stream_config: cpal::StreamConfig = supported.clone().into();
        let input_sample_rate: u32 = stream_config.sample_rate.into();
        let input_channels: u16 = stream_config.channels.into();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let frame_counter = Arc::new(AtomicU64::new(0));
        let err_fn = |err| eprintln!("cpal input error: {err}");

        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => {
                let captured = Arc::clone(&captured);
                let frame_counter = Arc::clone(&frame_counter);
                let visualization_sink = visualization_sink.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| {
                        push_samples_with_visualization(
                            &captured,
                            visualization_sink.as_ref(),
                            &frame_counter,
                            input_sample_rate,
                            input_channels,
                            data,
                            |sample| *sample,
                        )
                    },
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::I16 => {
                let captured = Arc::clone(&captured);
                let frame_counter = Arc::clone(&frame_counter);
                let visualization_sink = visualization_sink.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _| {
                        push_samples_with_visualization(
                            &captured,
                            visualization_sink.as_ref(),
                            &frame_counter,
                            input_sample_rate,
                            input_channels,
                            data,
                            i16_to_f32,
                        )
                    },
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::U16 => {
                let captured = Arc::clone(&captured);
                let frame_counter = Arc::clone(&frame_counter);
                let visualization_sink = visualization_sink.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[u16], _| {
                        push_samples_with_visualization(
                            &captured,
                            visualization_sink.as_ref(),
                            &frame_counter,
                            input_sample_rate,
                            input_channels,
                            data,
                            u16_to_f32,
                        )
                    },
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
            input_sample_rate,
            input_channels,
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

fn i16_to_f32(sample: &i16) -> f32 {
    (*sample as f32) / i16::MAX as f32
}

fn u16_to_f32(sample: &u16) -> f32 {
    ((*sample as f32) - 32768.0) / 32768.0
}

fn push_samples_with_visualization<T, F>(
    captured: &Arc<Mutex<Vec<f32>>>,
    visualization_sink: Option<&SyncSender<AudioVisualizationSnapshot>>,
    frame_counter: &Arc<AtomicU64>,
    input_sample_rate: u32,
    input_channels: u16,
    data: &[T],
    convert: F,
) where
    F: Fn(&T) -> f32,
{
    let channels = usize::from(input_channels.max(1));
    if data.is_empty() || channels == 0 {
        return;
    }

    let frame_count = data.len() / channels;
    if frame_count == 0 {
        return;
    }

    let mut mono_frames = Vec::with_capacity(frame_count);

    if let Ok(mut buffer) = captured.lock() {
        for frame in data.chunks_exact(channels) {
            let mut sum = 0.0f32;
            for sample in frame {
                let sample = clamp_sample(convert(sample));
                buffer.push(sample);
                sum += sample;
            }
            mono_frames.push(clamp_sample(sum / channels as f32));
        }
    } else {
        return;
    }

    let frame_index =
        frame_counter.fetch_add(frame_count as u64, Ordering::Relaxed) + frame_count as u64;
    if let Some(snapshot) =
        dsp::build_visualization_snapshot(&mono_frames, input_sample_rate, frame_index)
    {
        let _ = try_publish_visualization_snapshot(visualization_sink, snapshot);
    }
}

fn try_publish_visualization_snapshot(
    visualization_sink: Option<&SyncSender<AudioVisualizationSnapshot>>,
    snapshot: AudioVisualizationSnapshot,
) -> bool {
    visualization_sink.is_some_and(|sink| sink.try_send(snapshot).is_ok())
}

fn normalize_to_mono_16k(raw: &[f32], input_sample_rate: u32, input_channels: u16) -> Vec<f32> {
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

#[cfg(test)]
mod tests {
    use super::super::{VISUALIZATION_BAND_COUNT, VISUALIZATION_BIN_COUNT};
    use super::*;
    use std::sync::mpsc::sync_channel;

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
    fn publishes_visualization_snapshot_without_blocking() {
        let (sink, receiver) = sync_channel(1);
        sink.send(AudioVisualizationSnapshot {
            frame_index: 1,
            sample_rate: TARGET_SAMPLE_RATE,
            rms: 0.25,
            peak: 0.5,
            level: 0.25,
            transient: 0.0,
            bands: [0.0; VISUALIZATION_BAND_COUNT],
            waveform: [0.0; VISUALIZATION_BIN_COUNT],
        })
        .unwrap();

        let published = try_publish_visualization_snapshot(
            Some(&sink),
            AudioVisualizationSnapshot {
                frame_index: 2,
                sample_rate: TARGET_SAMPLE_RATE,
                rms: 0.75,
                peak: 1.0,
                level: 1.0,
                transient: 1.0,
                bands: [1.0; VISUALIZATION_BAND_COUNT],
                waveform: [1.0; VISUALIZATION_BIN_COUNT],
            },
        );

        assert!(!published);
        let snapshot = receiver.try_recv().expect("first snapshot");
        assert_eq!(snapshot.frame_index, 1);
        assert!(receiver.try_recv().is_err());
    }
}
