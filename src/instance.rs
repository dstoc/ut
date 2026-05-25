use anyhow::{Context, Result};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub root: PathBuf,
    pub control_socket: PathBuf,
    pub lock: PathBuf,
    pub state_path: PathBuf,
    pub current_wav: PathBuf,
}

impl RuntimePaths {
    pub fn resolve() -> Result<Self> {
        let root = runtime_root()?;
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create runtime dir at {root:?}"))?;

        Ok(Self {
            control_socket: root.join("control.sock"),
            lock: root.join("lock"),
            state_path: root.join("state.json"),
            current_wav: root.join("current.wav"),
            root,
        })
    }
}

pub fn lock_pid(path: impl AsRef<std::path::Path>) -> Result<Option<u32>> {
    let path = path.as_ref();
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(None);
    };

    Ok(text.trim().parse::<u32>().ok())
}

pub fn write_lock(path: impl AsRef<std::path::Path>, pid: u32) -> Result<()> {
    let path = path.as_ref();
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to create lock file at {path:?}"))?;
    writeln!(file, "{pid}").context("failed to write lock pid")?;
    file.flush().context("failed to flush lock file")?;
    Ok(())
}

pub fn remove_if_exists(path: impl AsRef<std::path::Path>) -> Result<()> {
    let path = path.as_ref();
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {path:?}")),
    }
}

pub fn pid_is_alive(pid: u32) -> Result<bool> {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return Ok(true);
    }

    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    if errno == libc::EPERM {
        return Ok(true);
    }
    if errno == libc::ESRCH {
        return Ok(false);
    }

    Err(std::io::Error::last_os_error()).with_context(|| format!("failed to probe pid {pid}"))
}

pub fn clean_stale_runtime(paths: &RuntimePaths) -> Result<()> {
    remove_if_exists(&paths.control_socket)?;
    remove_if_exists(&paths.lock)?;
    Ok(())
}

pub fn acquire_lock(paths: &RuntimePaths, pid: u32) -> Result<LockOutcome> {
    match write_lock(&paths.lock, pid) {
        Ok(()) => Ok(LockOutcome::Acquired),
        Err(err)
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == ErrorKind::AlreadyExists) =>
        {
            let owner_pid = lock_pid(&paths.lock)?;
            if let Some(owner_pid) = owner_pid {
                if pid_is_alive(owner_pid)? {
                    return Ok(LockOutcome::Busy(owner_pid));
                }
            }

            clean_stale_runtime(paths)?;
            write_lock(&paths.lock, pid)?;
            Ok(LockOutcome::Recovered)
        }
        Err(err) => Err(err),
    }
}

pub fn bind_control_socket(path: impl AsRef<std::path::Path>) -> Result<UnixListener> {
    let path = path.as_ref();
    remove_if_exists(path)?;
    UnixListener::bind(path).with_context(|| format!("failed to bind control socket at {path:?}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOutcome {
    Acquired,
    Recovered,
    Busy(u32),
}

fn runtime_root() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(dir).join("ut"));
    }

    if let Some(tmp) = env::var_os("TMPDIR") {
        return Ok(PathBuf::from(tmp).join("ut"));
    }

    Ok(env::temp_dir().join("ut"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir() -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        env::temp_dir().join(format!("ut-instance-test-{now}-{}", std::process::id()))
    }

    #[test]
    fn lock_file_round_trip() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let lock = dir.join("lock");

        write_lock(&lock, 42).unwrap();
        assert_eq!(lock_pid(&lock).unwrap(), Some(42));

        remove_if_exists(&lock).unwrap();
        assert_eq!(lock_pid(&lock).unwrap(), None);
        let _ = fs::remove_dir_all(dir);
    }
}
