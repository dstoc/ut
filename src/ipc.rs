use crate::state::SessionPhase;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlCommand {
    StopAndProcess,
    Abort,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ControlResponse {
    Ack,
    Status(crate::state::SessionPhase),
    Error(String),
}

#[derive(Debug)]
pub struct ControlState {
    inner: Mutex<ControlInner>,
    wake: Condvar,
    state_path: PathBuf,
    pid: u32,
}

#[derive(Debug, Clone, Copy)]
struct ControlInner {
    phase: crate::state::SessionPhase,
    stop_requested: bool,
    abort_requested: bool,
    shutdown: bool,
}

impl ControlState {
    pub fn new(state_path: PathBuf, pid: u32, initial_phase: SessionPhase) -> Self {
        Self {
            inner: Mutex::new(ControlInner {
                phase: initial_phase,
                stop_requested: false,
                abort_requested: false,
                shutdown: false,
            }),
            wake: Condvar::new(),
            state_path,
            pid,
        }
    }

    fn persist_phase(&self, phase: SessionPhase) {
        let _ = crate::state::write_session_phase(&self.state_path, Some(self.pid), phase);
    }

    pub fn transition(&self, phase: SessionPhase) {
        let mut inner = self.inner.lock().expect("control state poisoned");
        inner.phase = phase;
        self.persist_phase(phase);
    }

    pub fn record_metadata(&self, update: impl FnOnce(&mut crate::state::SessionState)) {
        let inner = self.inner.lock().expect("control state poisoned");
        let mut file = crate::state::read_state_or_default(&self.state_path);
        file.session.pid = Some(self.pid);
        file.session.phase = inner.phase;
        update(&mut file.session);
        let _ = crate::state::write_state(&self.state_path, &file);
    }

    pub fn phase(&self) -> SessionPhase {
        self.inner.lock().expect("control state poisoned").phase
    }

    pub fn stop_and_process(&self) {
        let mut inner = self.inner.lock().expect("control state poisoned");
        if !inner.abort_requested {
            inner.phase = SessionPhase::Processing;
            inner.stop_requested = true;
            self.persist_phase(inner.phase);
        }
        self.wake.notify_all();
    }

    pub fn abort(&self) {
        let mut inner = self.inner.lock().expect("control state poisoned");
        inner.abort_requested = true;
        inner.stop_requested = false;
        inner.phase = SessionPhase::Idle;
        self.persist_phase(inner.phase);
        self.wake.notify_all();
    }

    pub fn shutdown(&self) {
        let mut inner = self.inner.lock().expect("control state poisoned");
        inner.shutdown = true;
        self.wake.notify_all();
    }

    pub fn should_shutdown(&self) -> bool {
        self.inner.lock().expect("control state poisoned").shutdown
    }

    pub fn wait_for_activity(&self, timeout: Duration) {
        let inner = self.inner.lock().expect("control state poisoned");
        let _ = self
            .wake
            .wait_timeout(inner, timeout)
            .expect("control state poisoned");
    }

    pub fn has_abort_request(&self) -> bool {
        self.inner
            .lock()
            .expect("control state poisoned")
            .abort_requested
    }

    pub fn has_stop_request(&self) -> bool {
        self.inner
            .lock()
            .expect("control state poisoned")
            .stop_requested
    }

    pub fn clear_stop_request(&self) {
        let mut inner = self.inner.lock().expect("control state poisoned");
        inner.stop_requested = false;
        self.wake.notify_all();
    }
}

pub fn send_command(path: impl AsRef<Path>, command: ControlCommand) -> Result<ControlResponse> {
    let mut stream = UnixStream::connect(path.as_ref())
        .with_context(|| format!("failed to connect to control socket at {:?}", path.as_ref()))?;

    let payload = serde_json::to_vec(&command)?;
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

pub fn spawn_control_server(
    listener: std::os::unix::net::UnixListener,
    state: Arc<ControlState>,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || run_control_server(listener, state))
}

fn run_control_server(
    listener: std::os::unix::net::UnixListener,
    state: Arc<ControlState>,
) -> Result<()> {
    listener
        .set_nonblocking(true)
        .context("failed to set control socket nonblocking")?;

    loop {
        if state.should_shutdown() {
            break;
        }

        match listener.accept() {
            Ok((stream, _)) => {
                handle_control_connection(stream, &state)?;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                return Err(err).context("failed to accept control connection");
            }
        }
    }

    Ok(())
}

fn handle_control_connection(stream: UnixStream, state: &Arc<ControlState>) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut stream = reader.into_inner();

    let command: ControlCommand =
        serde_json::from_str(line.trim()).context("invalid control command")?;
    let response = match command {
        ControlCommand::StopAndProcess => {
            state.stop_and_process();
            ControlResponse::Ack
        }
        ControlCommand::Abort => {
            state.abort();
            ControlResponse::Ack
        }
        ControlCommand::Status => ControlResponse::Status(state.phase()),
    };

    let payload = serde_json::to_vec(&response)?;
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_socket_path() -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("ut-ipc-test-{stamp}-{}.sock", std::process::id()))
    }

    #[test]
    fn status_round_trip_over_socket() {
        let socket = unique_socket_path();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let state = Arc::new(ControlState::new(
            std::env::temp_dir().join("ut-ipc-test-state.json"),
            std::process::id(),
            crate::state::SessionPhase::Recording,
        ));
        let server = spawn_control_server(listener, Arc::clone(&state));

        let response = send_command(&socket, ControlCommand::Status).unwrap();
        assert_eq!(
            response,
            ControlResponse::Status(crate::state::SessionPhase::Recording)
        );

        state.shutdown();
        server.join().unwrap().unwrap();
        let _ = fs::remove_file(socket);
    }
}
