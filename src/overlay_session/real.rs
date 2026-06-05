use anyhow::{Context, Result};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::{audio, config, overlay, state};

#[derive(Debug)]
pub(crate) struct OverlaySession {
    runtime: Option<OverlayRuntime>,
}

impl OverlaySession {
    pub(crate) fn start(config: &config::StatusUiConfig) -> Self {
        if !config.enabled {
            return Self { runtime: None };
        }

        match OverlayRuntime::start(config.clone()) {
            Ok(runtime) => Self {
                runtime: Some(runtime),
            },
            Err(err) => {
                eprintln!(
                    "warning: failed to start status overlay; continuing without it: {err:#}"
                );
                Self { runtime: None }
            }
        }
    }

    pub(crate) fn visualization_sender(
        &self,
    ) -> Option<mpsc::SyncSender<audio::AudioVisualizationSnapshot>> {
        self.runtime
            .as_ref()
            .and_then(|runtime| runtime.visualization_sender())
    }

    pub(crate) fn set_phase(&self, phase: state::SessionPhase) {
        if let Some(runtime) = self.runtime.as_ref() {
            runtime.set_phase(phase);
        }
    }

    pub(crate) fn finish_recording_stream(&mut self) {
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.finish_recording_stream();
        }
    }

    pub(crate) fn abort_now(mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.abort_now();
        }
    }

    pub(crate) fn finish_with_fade(mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.finish_with_fade();
        }
    }
}

impl Drop for OverlaySession {
    fn drop(&mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.abort_now();
        }
    }
}

#[derive(Debug)]
struct OverlayRuntime {
    handle: Arc<Mutex<Option<overlay::OverlayHandle>>>,
    snapshot_thread: Option<thread::JoinHandle<()>>,
    visualization_sender: Option<mpsc::SyncSender<audio::AudioVisualizationSnapshot>>,
    fade_out: Duration,
}

impl OverlayRuntime {
    fn start(config: config::StatusUiConfig) -> Result<Self> {
        let handle = Arc::new(Mutex::new(Some(overlay::spawn(config.clone())?)));
        let (visualization_sender, visualization_receiver) =
            mpsc::sync_channel::<audio::AudioVisualizationSnapshot>(8);
        let handle_for_thread = Arc::clone(&handle);
        let snapshot_thread = match thread::Builder::new()
            .name("ut-status-ui-audio".to_string())
            .spawn(move || {
                while let Ok(snapshot) = visualization_receiver.recv() {
                    if let Ok(guard) = handle_for_thread.lock() {
                        if let Some(handle) = guard.as_ref() {
                            let _ = handle.push_audio_snapshot(snapshot);
                        }
                    }
                }
            }) {
            Ok(thread) => thread,
            Err(err) => {
                if let Ok(mut guard) = handle.lock() {
                    if let Some(handle) = guard.take() {
                        let _ = handle.shutdown();
                    }
                }
                return Err(err).context("failed to spawn status overlay snapshot thread");
            }
        };

        Ok(Self {
            handle,
            snapshot_thread: Some(snapshot_thread),
            visualization_sender: Some(visualization_sender),
            fade_out: Duration::from_millis(config.fade_out_ms),
        })
    }

    fn visualization_sender(&self) -> Option<mpsc::SyncSender<audio::AudioVisualizationSnapshot>> {
        self.visualization_sender.clone()
    }

    fn set_phase(&self, phase: state::SessionPhase) {
        self.with_handle(|handle| {
            let _ = handle.set_phase(phase);
        });
    }

    fn finish_recording_stream(&mut self) {
        self.visualization_sender.take();
        self.join_snapshot_thread();
    }

    fn abort_now(&mut self) {
        self.with_handle(|handle| {
            let _ = handle.request_abort();
        });
        self.finish_immediately();
    }

    fn finish_with_fade(&mut self) {
        self.set_phase(state::SessionPhase::Idle);
        self.with_handle(|handle| {
            let _ = handle.request_fade_out();
        });
        thread::sleep(self.fade_out);
        self.finish_immediately();
    }

    fn finish_immediately(&mut self) {
        self.visualization_sender.take();
        self.join_snapshot_thread();
        if let Some(handle) = self.take_handle() {
            let _ = handle.shutdown();
        }
    }

    fn join_snapshot_thread(&mut self) {
        if let Some(thread) = self.snapshot_thread.take() {
            let _ = thread.join();
        }
    }

    fn take_handle(&mut self) -> Option<overlay::OverlayHandle> {
        let mut guard = self.handle.lock().ok()?;
        guard.take()
    }

    fn with_handle<F>(&self, f: F)
    where
        F: FnOnce(&overlay::OverlayHandle),
    {
        if let Ok(guard) = self.handle.lock() {
            if let Some(handle) = guard.as_ref() {
                f(handle);
            }
        }
    }
}
