use crate::audio;
use crate::audio::trim;
use crate::config::Config;
use crate::context;
use crate::dictation;
use crate::ipc::ControlState;
use crate::notify;
use crate::overlay_session::OverlaySession;
use crate::paste;
use crate::prompt;
use crate::state::SessionPhase;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::runtime::Builder;
use tokio::sync::oneshot;

pub enum RecordingStopReason {
    Stopped,
    TimedOut,
    Aborted,
}

pub fn wait_for_recording_exit(
    control: &Arc<ControlState>,
    max_duration: Duration,
) -> RecordingStopReason {
    let deadline = Instant::now() + max_duration;
    loop {
        if control.has_abort_request() || control.should_shutdown() {
            return RecordingStopReason::Aborted;
        }

        if control.has_stop_request() {
            return RecordingStopReason::Stopped;
        }

        if Instant::now() >= deadline {
            return RecordingStopReason::TimedOut;
        }

        control.wait_for_activity(Duration::from_millis(50));
    }
}

pub fn dictation_with_abort(
    client: Arc<dyn dictation::DictationClient>,
    request: dictation::DictationRequest,
    control: &Arc<ControlState>,
) -> Result<Option<dictation::DictationResponse>> {
    let (tx, rx) = mpsc::channel();
    let (cancel_tx, cancel_rx) = oneshot::channel();
    thread::spawn(move || {
        let runtime = match Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(err) => {
                let _ = tx.send(Err(err.into()));
                return;
            }
        };

        let result = runtime.block_on(async move {
            tokio::select! {
                result = client.dictate(request) => Some(result),
                _ = cancel_rx => None,
            }
        });

        if let Some(result) = result {
            let _ = tx.send(result);
        }
    });

    let mut cancel_tx = Some(cancel_tx);

    loop {
        if control.has_abort_request() || control.should_shutdown() {
            if let Some(cancel_tx) = cancel_tx.take() {
                let _ = cancel_tx.send(());
            }
            return Ok(None);
        }

        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(result) => return result.map(Some),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("dictation worker exited unexpectedly")
            }
        }
    }
}

pub fn can_auto_paste(
    start_context: &context::AppContext,
    pre_paste_context: &context::AppContext,
) -> bool {
    matches!(
        (&start_context.container_id, &pre_paste_context.container_id),
        (Some(start_container_id), Some(pre_paste_container_id)) if start_container_id == pre_paste_container_id
    )
}

pub(crate) fn capture_context() -> context::AppContext {
    let Some(context) = context::sway::focused_context().ok().flatten() else {
        return context::AppContext::default();
    };

    match context::proc::enrich_context(context.clone()) {
        Ok(enriched) => enriched,
        Err(_) => context,
    }
}

/// Write the recorded audio to `dir` as a timestamped WAV, returning the path.
/// Used by `ut start --save-to <dir>` for debugging what the model actually
/// received. Creates the directory if needed.
fn save_debug_audio(dir: &Path, payload: &audio::AudioPayload) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create debug audio directory {}", dir.display()))?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{}.wav", format_utc_timestamp(now)));

    let bytes = audio::encode_wav_bytes(&payload.samples, payload.sample_rate);
    std::fs::write(&path, bytes)
        .with_context(|| format!("failed to write debug audio {}", path.display()))?;
    Ok(path)
}

/// Format a Unix timestamp (seconds) as `YYYY-MM-DD_HH-MM-SS` in UTC, suitable
/// for use as a filename. Dependency-free so it works without a date crate.
fn format_utc_timestamp(unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let seconds_of_day = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}_{hour:02}-{minute:02}-{second:02}")
}

/// Convert a count of days since the Unix epoch to a `(year, month, day)` civil
/// date (UTC). Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

pub struct Session<'a> {
    config: &'a Config,
    control: Arc<ControlState>,
    overlay: OverlaySession,
    start_context: context::AppContext,
    save_to: Option<PathBuf>,
}

impl<'a> Session<'a> {
    pub fn new(
        config: &'a Config,
        control: Arc<ControlState>,
        start_context: context::AppContext,
        save_to: Option<PathBuf>,
    ) -> Self {
        let overlay = OverlaySession::start(&config.status_ui);
        Self {
            config,
            control,
            overlay,
            start_context,
            save_to,
        }
    }

    pub fn run(mut self) -> Result<()> {
        let recorder =
            audio::Recorder::start_with_visualization(self.overlay.visualization_sender())?;
        self.overlay.set_phase(SessionPhase::Recording);
        let stop_reason = wait_for_recording_exit(
            &self.control,
            Duration::from_secs(self.config.recording.max_seconds as u64),
        );

        if self.control.has_abort_request() || matches!(stop_reason, RecordingStopReason::Aborted) {
            // Stop capture before aborting: the real recorder owns a `cpal::Stream`
            // whose `Drop` halts the audio device. The stub recorder holds no such
            // resource, so the drop is a no-op there (hence the lint allow).
            #[allow(clippy::drop_non_drop)]
            drop(recorder);
            return self.abort_to_idle();
        }

        let stop_context = capture_context();
        self.control.transition(SessionPhase::Processing);
        self.control
            .record_metadata(|s| s.stop_context = Some(stop_context));
        self.overlay.set_phase(SessionPhase::Processing);

        let mut payload = recorder.finish();
        self.overlay.finish_recording_stream();
        if self.config.recording.trim_silence {
            payload.samples = trim::trim_silence(&payload.samples, &self.config.recording);
        }

        if payload.samples.is_empty() {
            self.overlay.finish_with_fade();
            self.control.transition(SessionPhase::Idle);
            return Ok(());
        }

        if let Some(dir) = &self.save_to {
            match save_debug_audio(dir, &payload) {
                Ok(path) => eprintln!("saved debug audio to {}", path.display()),
                Err(err) => eprintln!("warning: failed to save debug audio: {err:#}"),
            }
        }

        if self.control.has_abort_request() {
            return self.abort_to_idle();
        }

        let prompt = prompt::build_prompt(&self.start_context, self.config);
        let request = dictation::DictationRequest {
            audio: payload,
            prompt,
        };
        let client: Arc<dyn dictation::DictationClient> =
            Arc::new(dictation::HttpDictationClient::new(&self.config.model));

        match dictation_with_abort(client, request, &self.control)? {
            Some(response) => {
                if self.control.has_abort_request() {
                    return self.abort_to_idle();
                }

                let pre_paste_context = capture_context();
                let focus_safe = can_auto_paste(&self.start_context, &pre_paste_context);
                let app_rule = self.config.app_rule_for_context(&self.start_context);
                self.control.transition(SessionPhase::Pasting);
                self.control
                    .record_metadata(|s| s.pre_paste_context = Some(pre_paste_context));
                self.overlay.set_phase(SessionPhase::Pasting);

                if self.control.has_abort_request() {
                    return self.abort_to_idle();
                }

                match paste::paste_text(
                    &response.text,
                    focus_safe,
                    &self.config.paste,
                    app_rule.and_then(|rule| rule.paste_keys.as_deref()),
                ) {
                    Ok(paste::PasteOutcome::Pasted) => {}
                    Ok(paste::PasteOutcome::CopiedOnly) => {
                        let _ = notify::focus_changed();
                    }
                    Err(error) => {
                        self.overlay.abort_now();
                        let message = match &error {
                            paste::PasteError::ClipboardUnavailable(m)
                            | paste::PasteError::AutomationFailed(m) => m.clone(),
                        };
                        self.control
                            .record_metadata(|s| s.last_error = Some(message.clone()));
                        match &error {
                            paste::PasteError::ClipboardUnavailable(m) => {
                                let _ = notify::paste_copy_failed(m);
                            }
                            paste::PasteError::AutomationFailed(m) => {
                                let _ = notify::manual_paste_required(m);
                            }
                        }
                        self.control.transition(SessionPhase::Idle);
                        return Err(anyhow::anyhow!(message));
                    }
                }
            }
            None => {
                return self.abort_to_idle();
            }
        }

        self.overlay.finish_with_fade();
        self.control.transition(SessionPhase::Idle);
        Ok(())
    }

    fn abort_to_idle(self) -> Result<()> {
        self.overlay.abort_now();
        self.control.transition(SessionPhase::Idle);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn formats_utc_timestamp_for_filename() {
        // 0 == 1970-01-01T00:00:00Z
        assert_eq!(format_utc_timestamp(0), "1970-01-01_00-00-00");
        // 1_700_000_000 == 2023-11-14T22:13:20Z
        assert_eq!(format_utc_timestamp(1_700_000_000), "2023-11-14_22-13-20");
    }

    #[test]
    fn saves_debug_audio_with_wav_header() {
        let dir = std::env::temp_dir().join(format!("ut-save-test-{}", std::process::id()));
        let payload = audio::AudioPayload::new(16_000, 1, vec![0.0, 0.5, -0.5]);

        let path = save_debug_audio(&dir, &payload).expect("save");
        assert!(path.extension().is_some_and(|ext| ext == "wav"));
        let bytes = std::fs::read(&path).expect("read back");
        assert_eq!(&bytes[..4], b"RIFF");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_paste_requires_matching_container_ids() {
        let start_context = context::AppContext {
            container_id: Some("123".to_string()),
            ..Default::default()
        };

        let matching_pre_paste_context = context::AppContext {
            container_id: Some("123".to_string()),
            ..Default::default()
        };

        let missing_pre_paste_context = context::AppContext::default();

        assert!(can_auto_paste(&start_context, &matching_pre_paste_context));
        assert!(!can_auto_paste(&start_context, &missing_pre_paste_context));
    }

    #[derive(Debug)]
    struct SlowDictationClient;

    #[async_trait]
    impl dictation::DictationClient for SlowDictationClient {
        async fn dictate(
            &self,
            _request: dictation::DictationRequest,
        ) -> Result<dictation::DictationResponse> {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(dictation::DictationResponse {
                text: "done".to_string(),
            })
        }
    }

    fn unique_state_path() -> std::path::PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ut-test-state-{}-{}.json", std::process::id(), id))
    }

    #[test]
    fn abort_returns_without_waiting_for_dictation_completion() {
        let state_path = unique_state_path();
        let control = Arc::new(crate::ipc::ControlState::new(
            state_path,
            std::process::id(),
            crate::state::SessionPhase::Processing,
        ));
        let control_for_abort = Arc::clone(&control);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            control_for_abort.abort();
        });

        let request = dictation::DictationRequest {
            audio: audio::AudioPayload::new(16_000, 1, vec![0.0]),
            prompt: "prompt".to_string(),
        };

        let start = Instant::now();
        let result =
            dictation_with_abort(Arc::new(SlowDictationClient), request, &control).unwrap();

        assert!(result.is_none());
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
