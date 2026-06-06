use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc::{sync_channel, Receiver, SyncSender},
    Arc, Mutex,
};
use std::thread::JoinHandle;

use super::dsp;
use super::{
    clamp_sample, AudioPayload, AudioVisualizationSnapshot, TARGET_CHANNELS, TARGET_SAMPLE_RATE,
};

/// How many downmixed buffers may queue for the visualization worker before
/// the audio callback starts dropping them. Visualization is best-effort, so a
/// slow worker must never stall or unbound-allocate on the realtime thread.
const VISUALIZATION_QUEUE_DEPTH: usize = 8;

/// Target ALSA period length. A larger period gives the capture ring more
/// headroom to absorb scheduling jitter (UI rendering, the transcription
/// request, general load) before it overruns and ALSA reports POLLERR/xrun.
const TARGET_BUFFER_MILLIS: u32 = 200;

/// Up-front capacity for the capture buffer, so it doesn't reallocate (copying
/// the whole growing buffer) on the realtime audio thread mid-recording.
const CAPTURE_RESERVE_SECONDS: usize = 60;

/// A downmixed capture buffer handed from the realtime audio callback to the
/// visualization worker thread, where the (expensive) DSP actually runs.
struct VisualizationFrame {
    mono_frames: Vec<f32>,
    sample_rate: u32,
    frame_index: u64,
}

pub struct Recorder {
    stream: cpal::Stream,
    captured: Arc<Mutex<Vec<f32>>>,
    input_sample_rate: u32,
    input_channels: u16,
    visualization_worker: Option<JoinHandle<()>>,
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
        let mut stream_config: cpal::StreamConfig = supported.clone().into();
        let input_sample_rate: u32 = stream_config.sample_rate;
        let input_channels: u16 = stream_config.channels;

        // Ask for a larger period than the device default when the device
        // advertises a usable range, clamped to what it actually supports.
        if let cpal::SupportedBufferSize::Range { min, max } = supported.buffer_size() {
            let target = (input_sample_rate / 1000 * TARGET_BUFFER_MILLIS).clamp(*min, *max);
            stream_config.buffer_size = cpal::BufferSize::Fixed(target);
        }

        let reserve = input_sample_rate as usize
            * usize::from(input_channels.max(1))
            * CAPTURE_RESERVE_SECONDS;
        let captured = Arc::new(Mutex::new(Vec::with_capacity(reserve)));
        let frame_counter = Arc::new(AtomicU64::new(0));
        let err_fn = |err| eprintln!("cpal input error: {err}");

        // When a sink is connected, offload the visualization DSP to a worker
        // thread. The realtime audio callback only downmixes and hands the
        // buffer off over a bounded channel; it never runs the DSP itself.
        let (visualization_tx, visualization_worker) = match visualization_sink {
            Some(sink) => {
                let (tx, rx) = sync_channel::<VisualizationFrame>(VISUALIZATION_QUEUE_DEPTH);
                (Some(tx), Some(spawn_visualization_worker(rx, sink)))
            }
            None => (None, None),
        };

        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => {
                let captured = Arc::clone(&captured);
                let frame_counter = Arc::clone(&frame_counter);
                let visualization_tx = visualization_tx.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| {
                        push_samples(
                            &captured,
                            visualization_tx.as_ref(),
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
                let visualization_tx = visualization_tx.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _| {
                        push_samples(
                            &captured,
                            visualization_tx.as_ref(),
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
                let visualization_tx = visualization_tx.clone();
                device.build_input_stream(
                    &stream_config,
                    move |data: &[u16], _| {
                        push_samples(
                            &captured,
                            visualization_tx.as_ref(),
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
            visualization_worker,
        })
    }

    pub fn finish(self) -> AudioPayload {
        let Recorder {
            stream,
            captured,
            input_sample_rate,
            input_channels,
            visualization_worker,
        } = self;
        // Dropping the stream drops the capture callback and with it the last
        // visualization sender, so the worker's channel closes and it exits.
        drop(stream);
        if let Some(worker) = visualization_worker {
            let _ = worker.join();
        }

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

/// Realtime audio callback body: copy every captured sample into `captured`
/// (the recording) and, when a visualization worker is connected, hand it the
/// downmixed buffer. This stays cheap and bounded — no DSP, no blocking — so it
/// reliably meets the audio device's callback deadline.
fn push_samples<T, F>(
    captured: &Arc<Mutex<Vec<f32>>>,
    visualization_tx: Option<&SyncSender<VisualizationFrame>>,
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

    // Only build the mono buffer when a worker will actually consume it.
    let mut mono_frames = if visualization_tx.is_some() {
        Vec::with_capacity(frame_count)
    } else {
        Vec::new()
    };

    if let Ok(mut buffer) = captured.lock() {
        for frame in data.chunks_exact(channels) {
            let mut sum = 0.0f32;
            for sample in frame {
                let sample = clamp_sample(convert(sample));
                buffer.push(sample);
                sum += sample;
            }
            if visualization_tx.is_some() {
                mono_frames.push(clamp_sample(sum / channels as f32));
            }
        }
    } else {
        return;
    }

    let Some(tx) = visualization_tx else {
        return;
    };

    let frame_index =
        frame_counter.fetch_add(frame_count as u64, Ordering::Relaxed) + frame_count as u64;
    // Best-effort: if the worker is behind and the queue is full, drop this
    // frame rather than block the realtime thread.
    let _ = tx.try_send(VisualizationFrame {
        mono_frames,
        sample_rate: input_sample_rate,
        frame_index,
    });
}

/// Off-thread visualization DSP: receives downmixed buffers from the audio
/// callback, runs the band analysis, and forwards snapshots to the UI sink.
/// Exits when the channel closes (the recorder's stream, and thus the sender,
/// is dropped).
fn spawn_visualization_worker(
    frames: Receiver<VisualizationFrame>,
    sink: SyncSender<AudioVisualizationSnapshot>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut vad = super::vad::VoiceActivityDetector::new();
        while let Ok(frame) = frames.recv() {
            let voice_probability = vad.observe(&frame.mono_frames, frame.sample_rate);
            if let Some(mut snapshot) = dsp::build_visualization_snapshot(
                &frame.mono_frames,
                frame.sample_rate,
                frame.frame_index,
            ) {
                snapshot.voice_probability = voice_probability;
                let _ = try_publish_visualization_snapshot(Some(&sink), snapshot);
            }
        }
    })
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
            level: 0.25,
            transient: 0.0,
            peak: 0.5,
            voice_probability: 0.0,
        })
        .unwrap();

        let published = try_publish_visualization_snapshot(
            Some(&sink),
            AudioVisualizationSnapshot {
                frame_index: 2,
                sample_rate: TARGET_SAMPLE_RATE,
                level: 1.0,
                transient: 1.0,
                peak: 1.0,
                voice_probability: 1.0,
            },
        );

        assert!(!published);
        let snapshot = receiver.try_recv().expect("first snapshot");
        assert_eq!(snapshot.frame_index, 1);
        assert!(receiver.try_recv().is_err());
    }
}
