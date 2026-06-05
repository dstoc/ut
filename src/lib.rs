pub mod audio;
pub mod config;
pub mod context;
pub mod dictation;
pub mod dispatch;
mod health;
pub mod instance;
pub mod ipc;
pub mod notify;
#[cfg(feature = "ui")]
pub mod overlay;
mod overlay_session;
pub mod paste;
pub mod prompt;
pub mod session;
pub mod state;

use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Start {
        /// Optional directory to save the recorded audio (WAV) for debugging.
        save_to: Option<PathBuf>,
    },
    Stop,
    Toggle,
    Abort,
    Status,
    Health,
}

pub fn run(invocation: Invocation) -> Result<()> {
    dispatch::run(invocation)
}
