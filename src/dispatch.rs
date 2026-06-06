use crate::config;
use crate::instance;
use crate::ipc;
use crate::session;
use crate::state;
use crate::Invocation;
use anyhow::Result;
use std::sync::Arc;

pub fn run(invocation: Invocation) -> Result<()> {
    let config = config::Config::load()?;

    if let Invocation::Health = invocation {
        return crate::health::run_health_checks(&config);
    }

    let runtime = instance::RuntimePaths::resolve()?;
    let pid = std::process::id();
    match invocation {
        Invocation::Start { save_to } => start_invocation(runtime, pid, config, save_to),
        Invocation::Stop => {
            dispatch_or_bootstrap(&runtime, ipc::ControlCommand::StopAndProcess).map(|_| ())
        }
        Invocation::Status => print_status(&runtime),
        Invocation::Abort => {
            dispatch_or_bootstrap(&runtime, ipc::ControlCommand::Abort).map(|_| ())
        }
        Invocation::Toggle => {
            match dispatch_or_bootstrap(&runtime, ipc::ControlCommand::StopAndProcess)? {
                DispatchOutcome::Delivered => Ok(()),
                DispatchOutcome::LiveOwnerMissing => {
                    start_owner_session(runtime, pid, config, None)
                }
            }
        }
        Invocation::Health => unreachable!(),
    }
}

fn start_invocation(
    runtime: instance::RuntimePaths,
    pid: u32,
    config: config::Config,
    save_to: Option<std::path::PathBuf>,
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
            start_owner_session(runtime, pid, config, save_to)
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
            Ok(DispatchOutcome::LiveOwnerMissing)
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

fn start_owner_session(
    runtime: instance::RuntimePaths,
    pid: u32,
    config: config::Config,
    save_to: Option<std::path::PathBuf>,
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

    let initial_context = session::capture_context();
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

    let session = session::Session::new(&config, Arc::clone(&control), initial_context, save_to);
    let session_result = session.run();
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

fn cleanup_owner_runtime(runtime: &instance::RuntimePaths) -> Result<()> {
    instance::remove_if_exists(&runtime.control_socket)?;
    instance::remove_if_exists(&runtime.lock)?;
    Ok(())
}
