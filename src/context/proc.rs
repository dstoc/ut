use crate::context::AppContext;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub fn enrich_context(mut context: AppContext) -> Result<AppContext> {
    if let Some(pid) = context.pid {
        context.exe = read_link(format!("/proc/{pid}/exe"))?;
        context.cwd = read_link(format!("/proc/{pid}/cwd"))?;
    }

    Ok(context)
}

fn read_link(path: impl Into<PathBuf>) -> Result<Option<String>> {
    let path = path.into();
    match fs::read_link(&path) {
        Ok(target) => Ok(Some(target.to_string_lossy().into_owned())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {path:?}")),
    }
}
