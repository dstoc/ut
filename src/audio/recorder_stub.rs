use anyhow::Result;
use std::sync::mpsc::SyncSender;

use super::{AudioPayload, AudioVisualizationSnapshot, TARGET_CHANNELS, TARGET_SAMPLE_RATE};

/// Stub recorder used when the crate is built without the `audio-capture`
/// feature (e.g. on systems lacking ALSA development headers). It preserves the
/// public API but captures no audio: `start` succeeds and `finish` yields an
/// empty payload, so callers can exercise the full pipeline without audio
/// hardware.
pub struct Recorder;

impl Recorder {
    pub fn start() -> Result<Self> {
        Self::start_with_visualization(None)
    }

    pub fn start_with_visualization(
        _visualization_sink: Option<SyncSender<AudioVisualizationSnapshot>>,
    ) -> Result<Self> {
        Ok(Self)
    }

    pub fn finish(self) -> AudioPayload {
        AudioPayload::new(TARGET_SAMPLE_RATE, TARGET_CHANNELS, Vec::new())
    }
}
