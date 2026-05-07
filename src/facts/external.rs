use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{NamedTempFile, TempPath};
use thiserror::Error;

const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct ExternalCommand {
    cmd: OsString,
    timeout: Duration,
}

impl ExternalCommand {
    pub fn new(command: impl Into<OsString>, timeout: Duration) -> Self {
        Self {
            cmd: command.into(),
            timeout,
        }
    }

    pub fn run(&self, configure: impl FnOnce(&mut Command)) -> Result<(), ExternalError> {
        let stderr_path = temp_path("stderr")?;
        let stderr_file = File::create(&stderr_path)
            .map_err(|error| ExternalError::io("create stderr tempfile", &stderr_path, error))?;

        let mut command = Command::new(&self.cmd);
        command
            .stdin(Stdio::null())
            .stderr(Stdio::from(stderr_file));
        configure(&mut command);

        let mut child = command.spawn().map_err(|error| ExternalError::Spawn {
            command: self.cmd.clone(),
            source: error,
        })?;
        let status = wait_with_timeout(&mut child, &self.cmd, self.timeout)?;
        let stderr = read_lossy(&stderr_path).unwrap_or_default();

        if !status.success() {
            return Err(ExternalError::Exit {
                command: self.cmd.clone(),
                status,
                stderr,
            });
        }

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ExternalError {
    #[error("failed to {op} for {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("failed to create {purpose} tempfile: {source}")]
    TempFile {
        purpose: &'static str,
        source: io::Error,
    },
    #[error("failed to spawn {command:?}: {source}")]
    Spawn {
        command: OsString,
        source: io::Error,
    },
    #[error("{command:?} timed out after {timeout:?}")]
    Timeout {
        command: OsString,
        timeout: Duration,
    },
    #[error("{command:?} exited with {status}: {}", stderr.trim())]
    Exit {
        command: OsString,
        status: ExitStatus,
        stderr: String,
    },
}

impl ExternalError {
    pub fn io(op: &'static str, path: impl AsRef<Path>, source: io::Error) -> Self {
        Self::Io {
            op,
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}

pub fn temp_path(purpose: &'static str) -> Result<TempPath, ExternalError> {
    NamedTempFile::new()
        .map(|file| file.into_temp_path())
        .map_err(|source| ExternalError::TempFile { purpose, source })
}

pub fn read(path: impl AsRef<Path>, op: &'static str) -> Result<Vec<u8>, ExternalError> {
    fs::read(path.as_ref()).map_err(|error| ExternalError::io(op, path, error))
}

pub fn read_lossy(path: impl AsRef<Path>) -> io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    command: &OsStr,
    timeout: Duration,
) -> Result<ExitStatus, ExternalError> {
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| ExternalError::io("wait for command", command, error))?
        {
            return Ok(status);
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ExternalError::Timeout {
                command: command.to_os_string(),
                timeout,
            });
        }

        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}
