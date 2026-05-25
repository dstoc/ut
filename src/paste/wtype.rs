use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

pub fn press_ctrl_v() -> Result<()> {
    press_shortcut("ctrl+v")
}

pub fn press_shortcut(sequence: &str) -> Result<()> {
    let args = shortcut_to_wtype_args(sequence)?;
    run_wtype(&args, None)
}

pub fn type_text(text: &str) -> Result<()> {
    run_wtype(&["-".to_string()], Some(text.as_bytes()))
}

fn shortcut_to_wtype_args(sequence: &str) -> Result<Vec<String>> {
    let tokens: Vec<String> = sequence
        .split('+')
        .map(|token| token.trim().to_lowercase())
        .collect();

    if tokens.is_empty() || tokens.iter().any(|token| token.is_empty()) {
        anyhow::bail!("invalid paste shortcut: {sequence}");
    }

    let (key, modifiers) = tokens
        .split_last()
        .expect("tokens is known to be non-empty");

    let mut args = Vec::with_capacity(modifiers.len() * 4 + 2);
    for modifier in modifiers {
        args.push("-M".to_string());
        args.push(normalize_modifier(modifier)?);
    }

    args.push("-k".to_string());
    args.push(key.clone());

    for modifier in modifiers.iter().rev() {
        args.push("-m".to_string());
        args.push(normalize_modifier(modifier)?);
    }

    Ok(args)
}

fn normalize_modifier(token: &str) -> Result<String> {
    let modifier = match token {
        "ctrl" | "control" => "ctrl",
        "shift" => "shift",
        "alt" | "option" | "mod1" => "alt",
        "super" | "meta" | "win" | "mod4" => "super",
        _ => anyhow::bail!("unsupported paste shortcut modifier: {token}"),
    };

    Ok(modifier.to_string())
}

fn run_wtype(args: &[String], stdin: Option<&[u8]>) -> Result<()> {
    let mut child = Command::new("wtype")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .spawn()
        .context("failed to run wtype")?;

    if let Some(input) = stdin {
        if let Some(handle) = child.stdin.as_mut() {
            handle
                .write_all(input)
                .context("failed to write wtype input")?;
        }
        drop(child.stdin.take());
    }

    let status = child.wait().context("failed to wait for wtype")?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("wtype exited with {status}")
    }
}

#[cfg(test)]
mod tests {
    use super::shortcut_to_wtype_args;

    #[test]
    fn shortcut_args_match_expected_wtype_sequence() {
        let args = shortcut_to_wtype_args("ctrl+shift+v").expect("valid shortcut");
        assert_eq!(
            args,
            vec![
                "-M".to_string(),
                "ctrl".to_string(),
                "-M".to_string(),
                "shift".to_string(),
                "-k".to_string(),
                "v".to_string(),
                "-m".to_string(),
                "shift".to_string(),
                "-m".to_string(),
                "ctrl".to_string(),
            ]
        );
    }

    #[test]
    fn shortcut_args_reject_empty_segments() {
        assert!(shortcut_to_wtype_args("ctrl++v").is_err());
    }
}
