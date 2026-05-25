use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

pub fn set_clipboard(text: &str) -> Result<()> {
    set_clipboard_bytes(text.as_bytes())
}

pub fn set_clipboard_bytes(bytes: &[u8]) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("failed to run wl-copy")?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(bytes)
            .context("failed to write clipboard payload")?;
    }
    drop(child.stdin.take());

    let status = child.wait().context("failed to wait for wl-copy")?;
    if status.success() {
        return Ok(());
    }

    anyhow::bail!("wl-copy exited with {status}");
}

pub fn get_clipboard() -> Result<Option<Vec<u8>>> {
    let output = Command::new("wl-paste")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run wl-paste")?;

    if output.status.success() {
        return Ok(Some(output.stdout));
    }

    if output.stdout.is_empty() && output.stderr.is_empty() {
        return Ok(None);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("wl-paste exited with {}: {}", output.status, stderr.trim());
}
