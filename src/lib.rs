pub mod audio;
pub mod config;
pub mod context;
pub mod gemma;
pub mod instance;
pub mod ipc;
pub mod notify;
pub mod paste;
pub mod prompt;
pub mod state;
pub mod trim;

use anyhow::Result;
use std::env;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::Builder;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    Start,
    Stop,
    Toggle,
    Abort,
    Status,
    Health,
}

pub fn run(invocation: Invocation) -> Result<()> {
    let config = config::Config::load()?;

    match invocation {
        Invocation::Health => run_health_checks(&config),
        _ => {
            let runtime = instance::RuntimePaths::resolve()?;
            let pid = std::process::id();
            match invocation {
                Invocation::Start => start_invocation(runtime, pid, config),
                Invocation::Stop => {
                    dispatch_or_bootstrap(&runtime, ipc::ControlCommand::StopAndProcess, false)
                        .map(|_| ())
                }
                Invocation::Status => print_status(&runtime),
                Invocation::Abort => {
                    dispatch_or_bootstrap(&runtime, ipc::ControlCommand::Abort, false).map(|_| ())
                }
                Invocation::Toggle => {
                    match dispatch_or_bootstrap(
                        &runtime,
                        ipc::ControlCommand::StopAndProcess,
                        true,
                    )? {
                        DispatchOutcome::Delivered => Ok(()),
                        DispatchOutcome::LiveOwnerMissing => {
                            start_owner_session(runtime, pid, config)
                        }
                    }
                }
                Invocation::Health => unreachable!(),
            }
        }
    }
}

fn run_health_checks(config: &config::Config) -> Result<()> {
    let mut failures = Vec::new();

    record_health_check("config", check_config(config), &mut failures);
    for command in ["swaymsg", "wl-copy", "wl-paste", "wtype", "notify-send"] {
        record_health_check(
            &format!("dependency:{command}"),
            check_command(command),
            &mut failures,
        );
    }
    record_health_check("audio", check_audio(), &mut failures);

    if failures.is_empty() {
        println!("health: ok");
        Ok(())
    } else {
        anyhow::bail!("health check failed: {}", failures.join("; "))
    }
}

fn record_health_check(name: &str, result: Result<()>, failures: &mut Vec<String>) {
    match result {
        Ok(()) => println!("ok   {name}"),
        Err(err) => {
            println!("fail {name}: {err}");
            failures.push(format!("{name}: {err}"));
        }
    }
}

fn check_config(config: &config::Config) -> Result<()> {
    config.validate()
}

fn check_command(command: &str) -> Result<()> {
    if command_exists(command) {
        Ok(())
    } else {
        anyhow::bail!("not found in PATH");
    }
}

fn command_exists(command: &str) -> bool {
    if command.contains('/') {
        return Path::new(command).is_file();
    }

    env::var_os("PATH")
        .map(|paths| {
            env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(command);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

fn check_audio() -> Result<()> {
    let recorder = audio::Recorder::start()?;
    thread::sleep(Duration::from_millis(100));
    let _ = recorder.finish();
    Ok(())
}

fn start_invocation(
    runtime: instance::RuntimePaths,
    pid: u32,
    config: config::Config,
) -> Result<()> {
    match ipc::send_command(&runtime.control_socket, ipc::ControlCommand::Status) {
        Ok(ipc::ControlResponse::Status(phase)) => {
            anyhow::bail!("session already active: {}", phase.as_str());
        }
        Ok(ipc::ControlResponse::Ack) => {
            anyhow::bail!("session already active");
        }
        Ok(ipc::ControlResponse::Error(err)) => Err(anyhow::anyhow!(err)),
        Err(err) => {
            if runtime.control_socket.exists() {
                if let Some(owner_pid) = stale_owner_pid(&runtime)? {
                    if instance::pid_is_alive(owner_pid)? {
                        return Err(err);
                    }
                }
            }
            instance::clean_stale_runtime(&runtime)?;
            state::clear_session(&runtime.state_path)?;
            start_owner_session(runtime, pid, config)
        }
    }
}

fn print_status(runtime: &instance::RuntimePaths) -> Result<()> {
    match ipc::send_command(&runtime.control_socket, ipc::ControlCommand::Status) {
        Ok(ipc::ControlResponse::Status(phase)) => {
            println!("{}", phase.as_str());
            Ok(())
        }
        Ok(ipc::ControlResponse::Ack) => {
            let state = state::read_state_or_default(&runtime.state_path);
            println!("{}", state.session.phase.as_str());
            Ok(())
        }
        Ok(ipc::ControlResponse::Error(err)) => Err(anyhow::anyhow!(err)),
        Err(_) => {
            let state = state::read_state_or_default(&runtime.state_path);
            println!("{}", state.session.phase.as_str());
            Ok(())
        }
    }
}

enum DispatchOutcome {
    Delivered,
    LiveOwnerMissing,
}

fn dispatch_or_bootstrap(
    runtime: &instance::RuntimePaths,
    command: ipc::ControlCommand,
    allow_owner_bootstrap: bool,
) -> Result<DispatchOutcome> {
    match ipc::send_command(&runtime.control_socket, command) {
        Ok(ipc::ControlResponse::Ack) => Ok(DispatchOutcome::Delivered),
        Ok(ipc::ControlResponse::Status(phase)) => {
            println!("{}", phase.as_str());
            Ok(DispatchOutcome::Delivered)
        }
        Ok(ipc::ControlResponse::Error(err)) => Err(anyhow::anyhow!(err)),
        Err(err) => {
            if !runtime.control_socket.exists() {
                instance::clean_stale_runtime(runtime)?;
                state::clear_session(&runtime.state_path)?;
                return Ok(DispatchOutcome::LiveOwnerMissing);
            }

            if let Some(pid) = stale_owner_pid(runtime)? {
                if instance::pid_is_alive(pid)? {
                    return Err(err);
                }
            }

            instance::clean_stale_runtime(runtime)?;
            state::clear_session(&runtime.state_path)?;
            if allow_owner_bootstrap {
                Ok(DispatchOutcome::LiveOwnerMissing)
            } else {
                Ok(DispatchOutcome::LiveOwnerMissing)
            }
        }
    }
}

fn stale_owner_pid(runtime: &instance::RuntimePaths) -> Result<Option<u32>> {
    match instance::lock_pid(&runtime.lock)? {
        Some(pid) => Ok(Some(pid)),
        None => {
            let state = state::read_state_or_default(&runtime.state_path);
            Ok(state.session.pid)
        }
    }
}

fn start_owner_session(
    runtime: instance::RuntimePaths,
    pid: u32,
    config: config::Config,
) -> Result<()> {
    match instance::acquire_lock(&runtime, pid)? {
        instance::LockOutcome::Busy(owner_pid) => {
            anyhow::bail!("session already owned by pid {owner_pid}");
        }
        instance::LockOutcome::Acquired | instance::LockOutcome::Recovered => {}
    }

    let socket = match instance::bind_control_socket(&runtime.control_socket) {
        Ok(socket) => socket,
        Err(err) => {
            let _ = instance::remove_if_exists(&runtime.lock);
            return Err(err);
        }
    };

    let initial_context = capture_start_context();
    write_session_metadata(
        &runtime.state_path,
        Some(pid),
        state::SessionPhase::Recording,
        |session| {
            session.start_context = Some(initial_context.clone());
            session.stop_context = None;
            session.pre_paste_context = None;
            session.last_error = None;
        },
    )?;

    let control = Arc::new(ipc::ControlState::new(
        runtime.state_path.clone(),
        pid,
        state::SessionPhase::Recording,
    ));
    let server = ipc::spawn_control_server(socket, Arc::clone(&control));

    let session_result = run_session_loop(&runtime, pid, &config, &control, initial_context);
    if session_result.is_err() {
        let _ =
            state::write_session_phase(&runtime.state_path, Some(pid), state::SessionPhase::Idle);
    }
    control.shutdown();
    let server_result = server.join().expect("control server thread panicked");
    let cleanup_result = cleanup_owner_runtime(&runtime);

    cleanup_result?;
    server_result?;
    session_result?;
    Ok(())
}

fn run_session_loop(
    runtime: &instance::RuntimePaths,
    pid: u32,
    config: &config::Config,
    control: &Arc<ipc::ControlState>,
    start_context: context::AppContext,
) -> Result<()> {
    let recorder = audio::Recorder::start()?;
    let stop_reason = wait_for_recording_exit(
        control,
        Duration::from_secs(config.recording.max_seconds as u64),
    );

    if control.has_abort_request() || matches!(stop_reason, RecordingStopReason::Aborted) {
        state::write_session_phase(&runtime.state_path, Some(pid), state::SessionPhase::Idle)?;
        return Ok(());
    }

    let stop_context = capture_context();
    write_session_metadata(
        &runtime.state_path,
        Some(pid),
        state::SessionPhase::Processing,
        |session| {
            session.stop_context = Some(stop_context);
        },
    )?;

    let mut payload = recorder.finish();
    if config.recording.trim_silence {
        payload.samples = trim::trim_silence(&payload.samples, &config.recording);
    }

    if payload.samples.is_empty() {
        state::write_session_phase(&runtime.state_path, Some(pid), state::SessionPhase::Idle)?;
        return Ok(());
    }

    if control.has_abort_request() {
        state::write_session_phase(&runtime.state_path, Some(pid), state::SessionPhase::Idle)?;
        return Ok(());
    }

    let prompt = prompt::build_prompt(&start_context, config);
    let request = gemma::DictationRequest {
        audio: payload,
        prompt,
    };
    let client: Arc<dyn gemma::GemmaClient> = Arc::new(gemma::HttpGemmaClient::new(&config.model));

    match dictation_with_abort(client, request, control)? {
        Some(response) => {
            if control.has_abort_request() {
                state::write_session_phase(
                    &runtime.state_path,
                    Some(pid),
                    state::SessionPhase::Idle,
                )?;
                return Ok(());
            }

            let pre_paste_context = capture_context();
            let focus_safe = can_auto_paste(&start_context, &pre_paste_context);
            let app_rule = config.app_rule_for_context(&start_context);
            write_session_metadata(
                &runtime.state_path,
                Some(pid),
                state::SessionPhase::Pasting,
                |session| {
                    session.pre_paste_context = Some(pre_paste_context);
                },
            )?;

            if control.has_abort_request() {
                state::write_session_phase(
                    &runtime.state_path,
                    Some(pid),
                    state::SessionPhase::Idle,
                )?;
                return Ok(());
            }

            match paste::paste_text(
                &response.text,
                focus_safe,
                &config.paste,
                app_rule.and_then(|rule| rule.paste_keys.as_deref()),
            ) {
                Ok(paste::PasteOutcome::Pasted) => {}
                Ok(paste::PasteOutcome::CopiedOnly) => {
                    let _ = notify::focus_changed();
                }
                Err(paste::PasteError::ClipboardUnavailable(message)) => {
                    write_session_metadata(
                        &runtime.state_path,
                        Some(pid),
                        state::SessionPhase::Pasting,
                        |session| {
                            session.last_error = Some(message.clone());
                        },
                    )?;
                    let _ = notify::paste_copy_failed(&message);
                    state::write_session_phase(
                        &runtime.state_path,
                        Some(pid),
                        state::SessionPhase::Idle,
                    )?;
                    return Err(anyhow::anyhow!(message));
                }
                Err(paste::PasteError::AutomationFailed(message)) => {
                    write_session_metadata(
                        &runtime.state_path,
                        Some(pid),
                        state::SessionPhase::Pasting,
                        |session| {
                            session.last_error = Some(message.clone());
                        },
                    )?;
                    let _ = notify::manual_paste_required(&message);
                    state::write_session_phase(
                        &runtime.state_path,
                        Some(pid),
                        state::SessionPhase::Idle,
                    )?;
                    return Err(anyhow::anyhow!(message));
                }
            }
        }
        None => {
            state::write_session_phase(&runtime.state_path, Some(pid), state::SessionPhase::Idle)?;
            return Ok(());
        }
    }

    state::write_session_phase(&runtime.state_path, Some(pid), state::SessionPhase::Idle)?;
    Ok(())
}

fn capture_start_context() -> context::AppContext {
    capture_context()
}

fn capture_context() -> context::AppContext {
    let Some(context) = context::sway::focused_context().ok().flatten() else {
        return context::AppContext::default();
    };

    match context::proc::enrich_context(context.clone()) {
        Ok(enriched) => enriched,
        Err(_) => context,
    }
}

fn can_auto_paste(
    start_context: &context::AppContext,
    pre_paste_context: &context::AppContext,
) -> bool {
    matches!(
        (&start_context.container_id, &pre_paste_context.container_id),
        (Some(start_container_id), Some(pre_paste_container_id)) if start_container_id == pre_paste_container_id
    )
}

enum RecordingStopReason {
    Stopped,
    TimedOut,
    Aborted,
}

fn wait_for_recording_exit(
    control: &Arc<ipc::ControlState>,
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

fn dictation_with_abort(
    client: Arc<dyn gemma::GemmaClient>,
    request: gemma::DictationRequest,
    control: &Arc<ipc::ControlState>,
) -> Result<Option<gemma::DictationResponse>> {
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
                anyhow::bail!("Gemma worker exited unexpectedly")
            }
        }
    }
}

fn write_session_metadata<F>(
    path: &std::path::Path,
    pid: Option<u32>,
    phase: state::SessionPhase,
    update: F,
) -> Result<()>
where
    F: FnOnce(&mut state::SessionState),
{
    let mut file = state::read_state_or_default(path);
    file.session.pid = pid;
    file.session.phase = phase;
    update(&mut file.session);
    state::write_state(path, &file)
}

fn cleanup_owner_runtime(runtime: &instance::RuntimePaths) -> Result<()> {
    instance::remove_if_exists(&runtime.control_socket)?;
    instance::remove_if_exists(&runtime.lock)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn auto_paste_requires_matching_container_ids() {
        let mut start_context = context::AppContext::default();
        start_context.container_id = Some("123".to_string());

        let mut matching_pre_paste_context = context::AppContext::default();
        matching_pre_paste_context.container_id = Some("123".to_string());

        let mut missing_pre_paste_context = context::AppContext::default();
        missing_pre_paste_context.container_id = None;

        assert!(can_auto_paste(&start_context, &matching_pre_paste_context));
        assert!(!can_auto_paste(&start_context, &missing_pre_paste_context));
    }

    #[derive(Debug)]
    struct SlowGemmaClient;

    #[async_trait]
    impl gemma::GemmaClient for SlowGemmaClient {
        async fn dictate(
            &self,
            _request: gemma::DictationRequest,
        ) -> Result<gemma::DictationResponse> {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(gemma::DictationResponse {
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
        let control = Arc::new(ipc::ControlState::new(
            state_path,
            std::process::id(),
            state::SessionPhase::Processing,
        ));
        let control_for_abort = Arc::clone(&control);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            control_for_abort.abort();
        });

        let request = gemma::DictationRequest {
            audio: audio::AudioPayload::new(16_000, 1, vec![0.0]),
            prompt: "prompt".to_string(),
        };

        let start = Instant::now();
        let result = dictation_with_abort(Arc::new(SlowGemmaClient), request, &control).unwrap();

        assert!(result.is_none());
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
