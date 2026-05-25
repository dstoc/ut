use anyhow::Result;
use std::env;
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::{audio, config};

pub(crate) fn run_health_checks(config: &config::Config) -> Result<()> {
    let mut failures = Vec::new();

    record_health_check("config", check_config(config), &mut failures);
    warn_status_ui_ignored_for_health(config);
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

#[cfg(feature = "ui")]
fn warn_status_ui_ignored_for_health(_config: &config::Config) {}

#[cfg(not(feature = "ui"))]
fn warn_status_ui_ignored_for_health(config: &config::Config) {
    if config.status_ui.enabled {
        eprintln!(
            "warning: [status_ui] enabled=true is ignored in ut health because this binary was built without the `ui` feature"
        );
    }
}
