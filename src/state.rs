use crate::context::AppContext;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    #[default]
    Idle,
    Recording,
    Processing,
    Pasting,
}

impl SessionPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionPhase::Idle => "idle",
            SessionPhase::Recording => "recording",
            SessionPhase::Processing => "processing",
            SessionPhase::Pasting => "pasting",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionState {
    #[serde(default)]
    pub phase: SessionPhase,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub start_context: Option<AppContext>,
    #[serde(default)]
    pub stop_context: Option<AppContext>,
    #[serde(default)]
    pub pre_paste_context: Option<AppContext>,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateFile {
    #[serde(default = "default_state_version")]
    pub version: u32,
    #[serde(default)]
    pub session: SessionState,
}

fn default_state_version() -> u32 {
    1
}

pub fn read_state(path: impl AsRef<Path>) -> Result<StateFile> {
    let path = path.as_ref();
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read state at {path:?}"))?;
    Ok(
        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse state at {path:?}"))?,
    )
}

pub fn write_state(path: impl AsRef<Path>, state: &StateFile) -> Result<()> {
    let path = path.as_ref();
    let text = serde_json::to_string_pretty(state)?;
    fs::write(path, text).with_context(|| format!("failed to write state at {path:?}"))?;
    Ok(())
}

pub fn read_state_or_default(path: impl AsRef<Path>) -> StateFile {
    read_state(path).unwrap_or_default()
}

pub fn write_session_phase(
    path: impl AsRef<Path>,
    pid: Option<u32>,
    phase: SessionPhase,
) -> Result<()> {
    let path = path.as_ref();
    let mut state = read_state(path).unwrap_or_default();
    state.session.pid = pid;
    state.session.phase = phase;
    write_state(path, &state)
}

pub fn clear_session(path: impl AsRef<Path>) -> Result<()> {
    write_session_phase(path, None, SessionPhase::Idle)
}

impl Default for StateFile {
    fn default() -> Self {
        Self {
            version: default_state_version(),
            session: SessionState::default(),
        }
    }
}
