use crate::config::{FocusMismatchAction, PasteConfig, PasteMethod};
use crate::notify;
use std::fmt;
use std::thread;
use std::time::Duration;

pub mod clipboard;
pub mod wtype;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteOutcome {
    Pasted,
    CopiedOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteError {
    ClipboardUnavailable(String),
    AutomationFailed(String),
}

impl fmt::Display for PasteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PasteError::ClipboardUnavailable(message) => {
                write!(f, "clipboard unavailable: {message}")
            }
            PasteError::AutomationFailed(message) => {
                write!(f, "paste automation failed: {message}")
            }
        }
    }
}

impl std::error::Error for PasteError {}

pub fn paste_text(
    text: &str,
    focus_safe: bool,
    config: &PasteConfig,
    paste_keys: Option<&str>,
) -> std::result::Result<PasteOutcome, PasteError> {
    if !focus_safe {
        return match config.on_focus_changed {
            FocusMismatchAction::Copy => {
                clipboard::set_clipboard(text)
                    .map_err(|err| PasteError::ClipboardUnavailable(err.to_string()))?;
                Ok(PasteOutcome::CopiedOnly)
            }
        };
    }

    match config.method {
        PasteMethod::Clipboard => paste_via_clipboard(text, config, paste_keys),
        PasteMethod::Wtype => paste_via_wtype(text, config),
    }
}

fn paste_via_clipboard(
    text: &str,
    config: &PasteConfig,
    paste_keys: Option<&str>,
) -> std::result::Result<PasteOutcome, PasteError> {
    let previous_clipboard = if config.restore_clipboard {
        match clipboard::get_clipboard() {
            Ok(clipboard) => clipboard,
            Err(err) => {
                let _ = notify::clipboard_restore_unavailable(&err.to_string());
                None
            }
        }
    } else {
        None
    };

    clipboard::set_clipboard(text)
        .map_err(|err| PasteError::ClipboardUnavailable(err.to_string()))?;
    let result = match paste_keys {
        Some(sequence) => wtype::press_shortcut(sequence),
        None => wtype::press_ctrl_v(),
    };
    result.map_err(|err| PasteError::AutomationFailed(err.to_string()))?;

    if let Some(previous_clipboard) = previous_clipboard {
        thread::sleep(Duration::from_millis(config.restore_delay_ms));
        if let Err(err) = clipboard::set_clipboard_bytes(&previous_clipboard) {
            let _ = notify::clipboard_restore_failed(&err.to_string());
        }
    }

    Ok(PasteOutcome::Pasted)
}

fn paste_via_wtype(
    text: &str,
    config: &PasteConfig,
) -> std::result::Result<PasteOutcome, PasteError> {
    let previous_clipboard = if config.restore_clipboard {
        match clipboard::get_clipboard() {
            Ok(clipboard) => clipboard,
            Err(err) => {
                let _ = notify::clipboard_restore_unavailable(&err.to_string());
                None
            }
        }
    } else {
        None
    };

    if let Err(err) = clipboard::set_clipboard(text) {
        let _ = notify::paste_copy_failed(&err.to_string());
    }
    wtype::type_text(text).map_err(|err| PasteError::AutomationFailed(err.to_string()))?;

    if let Some(previous_clipboard) = previous_clipboard {
        thread::sleep(Duration::from_millis(config.restore_delay_ms));
        if let Err(err) = clipboard::set_clipboard_bytes(&previous_clipboard) {
            let _ = notify::clipboard_restore_failed(&err.to_string());
        }
    }

    Ok(PasteOutcome::Pasted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_error_formats_useful_messages() {
        let clipboard = PasteError::ClipboardUnavailable("wl-copy missing".to_string());
        let automation = PasteError::AutomationFailed("wtype missing".to_string());

        assert!(clipboard.to_string().contains("clipboard unavailable"));
        assert!(automation.to_string().contains("paste automation failed"));
    }
}
