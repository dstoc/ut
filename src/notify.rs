use anyhow::{Context, Result};
use std::process::{Command, Stdio};

pub fn notify(title: &str, body: &str) -> Result<()> {
    let status = Command::new("notify-send")
        .arg(title)
        .arg(body)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => {
            eprintln!("ut: notify-send exited with {status}: {title}: {body}");
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("ut: notify-send missing: {title}: {body}");
            Ok(())
        }
        Err(err) => Err(err).context("failed to run notify-send"),
    }
}

pub fn focus_changed() -> Result<()> {
    notify(
        "ut",
        "Focus changed while dictation was processing; the generated text was copied to the clipboard instead of pasted.",
    )
}

pub fn manual_paste_required(details: &str) -> Result<()> {
    notify(
        "ut",
        &format!("Paste automation failed; the generated text is on the clipboard. Paste it manually. {details}"),
    )
}

pub fn clipboard_restore_unavailable(details: &str) -> Result<()> {
    notify(
        "ut",
        &format!("Clipboard restore is unavailable; continuing without restoring the previous clipboard. {details}"),
    )
}

pub fn clipboard_restore_failed(details: &str) -> Result<()> {
    notify(
        "ut",
        &format!("Failed to restore the previous clipboard after paste. {details}"),
    )
}

pub fn paste_copy_failed(details: &str) -> Result<()> {
    notify(
        "ut",
        &format!("Failed to copy generated text to the clipboard. {details}"),
    )
}
