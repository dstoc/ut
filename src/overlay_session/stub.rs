use std::sync::mpsc;

use crate::{audio, config, state};

/// No-op status session used when the crate is built without the `ui`
/// feature. It mirrors the real session's API so the `Session` lifecycle
/// compiles and runs unchanged, just without an overlay.
#[derive(Debug, Default)]
pub(crate) struct OverlaySession;

impl OverlaySession {
    pub(crate) fn start(config: &config::StatusUiConfig) -> Self {
        if config.enabled {
            eprintln!(
                "warning: [status_ui] enabled=true is ignored because this binary was built without the `ui` feature"
            );
        }
        Self
    }

    pub(crate) fn visualization_sender(
        &self,
    ) -> Option<mpsc::SyncSender<audio::AudioVisualizationSnapshot>> {
        None
    }

    pub(crate) fn set_phase(&self, _phase: state::SessionPhase) {}

    pub(crate) fn finish_recording_stream(&mut self) {}

    pub(crate) fn abort_now(self) {}

    pub(crate) fn finish_with_fade(self) {}
}
