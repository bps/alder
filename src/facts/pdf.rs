use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{NamedTempFile, TempPath};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct PdfTextProvider {
    command: OsString,
    timeout: Duration,
}

impl PdfTextProvider {
    pub fn new(command: impl Into<OsString>, timeout: Duration) -> Self {
        Self {
            command: command.into(),
            timeout,
        }
    }

    pub fn text(&self, path: impl AsRef<Path>) -> Result<String, PdfTextError> {
        let input = path
            .as_ref()
            .canonicalize()
            .map_err(|error| PdfTextError::io("canonicalize input", path.as_ref(), error))?;
        let output_path = temp_path().map_err(|error| PdfTextError::tempfile("output", error))?;
        let stderr_path = temp_path().map_err(|error| PdfTextError::tempfile("stderr", error))?;
        let stderr_file = File::create(&stderr_path)
            .map_err(|error| PdfTextError::io("create stderr tempfile", &stderr_path, error))?;

        let mut child = Command::new(&self.command)
            .arg("-enc")
            .arg("UTF-8")
            .arg(&input)
            .arg(&output_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .map_err(|error| PdfTextError::spawn(self.command.clone(), error))?;

        let status = wait_with_timeout(&mut child, self.timeout)?;
        let stderr = read_lossy(&stderr_path).unwrap_or_default();

        if !status.success() {
            return Err(PdfTextError::Exit { status, stderr });
        }

        let bytes = fs::read(&output_path)
            .map_err(|error| PdfTextError::io("read pdftotext output", &output_path, error))?;
        String::from_utf8(bytes).map_err(PdfTextError::InvalidUtf8)
    }
}

impl Default for PdfTextProvider {
    fn default() -> Self {
        Self::new("pdftotext", DEFAULT_TIMEOUT)
    }
}

#[derive(Debug)]
pub enum PdfTextError {
    Io {
        op: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    TempFile {
        purpose: &'static str,
        source: io::Error,
    },
    Spawn {
        command: OsString,
        source: io::Error,
    },
    Timeout {
        timeout: Duration,
    },
    Exit {
        status: ExitStatus,
        stderr: String,
    },
    InvalidUtf8(std::string::FromUtf8Error),
}

impl PdfTextError {
    fn io(op: &'static str, path: impl AsRef<Path>, source: io::Error) -> Self {
        Self::Io {
            op,
            path: path.as_ref().to_path_buf(),
            source,
        }
    }

    fn tempfile(purpose: &'static str, source: io::Error) -> Self {
        Self::TempFile { purpose, source }
    }

    fn spawn(command: OsString, source: io::Error) -> Self {
        Self::Spawn { command, source }
    }
}

impl fmt::Display for PdfTextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { op, path, source } => {
                write!(f, "failed to {op} for {}: {source}", path.display())
            }
            Self::TempFile { purpose, source } => {
                write!(f, "failed to create {purpose} tempfile: {source}")
            }
            Self::Spawn { command, source } => {
                write!(f, "failed to spawn {:?}: {source}", command)
            }
            Self::Timeout { timeout } => write!(f, "pdftotext timed out after {timeout:?}"),
            Self::Exit { status, stderr } => {
                write!(f, "pdftotext exited with {status}: {}", stderr.trim())
            }
            Self::InvalidUtf8(error) => write!(f, "pdftotext output was not valid UTF-8: {error}"),
        }
    }
}

impl std::error::Error for PdfTextError {}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<ExitStatus, PdfTextError> {
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| PdfTextError::io("wait for pdftotext", "pdftotext", error))?
        {
            return Ok(status);
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PdfTextError::Timeout { timeout });
        }

        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn temp_path() -> io::Result<TempPath> {
    Ok(NamedTempFile::new()?.into_temp_path())
}

fn read_lossy(path: impl AsRef<Path>) -> io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    #[test]
    fn returns_text_from_fake_pdftotext() {
        let temp_dir = tempfile::tempdir().unwrap();
        let script = fake_command(
            temp_dir.path(),
            r#"#!/bin/sh
printf 'extracted text\n' > "$4"
"#,
        );
        let input = temp_dir.path().join("input.pdf");
        fs::write(&input, b"%PDF fake").unwrap();
        let provider = PdfTextProvider::new(script, Duration::from_secs(5));

        let text = provider.text(&input).unwrap();

        assert_eq!(text, "extracted text\n");
    }

    #[cfg(unix)]
    #[test]
    fn passes_paths_as_arguments_without_shell_interpolation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let script = fake_command(
            temp_dir.path(),
            r#"#!/bin/sh
printf '%s\n' "$3" > "$4"
"#,
        );
        let input = temp_dir.path().join("$(danger).pdf");
        fs::write(&input, b"%PDF fake").unwrap();
        let provider = PdfTextProvider::new(script, Duration::from_secs(5));

        let text = provider.text(&input).unwrap();

        assert!(text.contains("$(danger).pdf"));
    }

    #[cfg(unix)]
    #[test]
    fn reports_nonzero_exit_with_stderr() {
        let temp_dir = tempfile::tempdir().unwrap();
        let script = fake_command(
            temp_dir.path(),
            r#"#!/bin/sh
echo 'not a pdf' >&2
exit 7
"#,
        );
        let input = temp_dir.path().join("input.pdf");
        fs::write(&input, b"not a pdf").unwrap();
        let provider = PdfTextProvider::new(script, Duration::from_secs(5));

        let error = provider.text(&input).unwrap_err();

        assert!(matches!(error, PdfTextError::Exit { .. }));
        assert!(error.to_string().contains("not a pdf"));
    }

    #[cfg(unix)]
    #[test]
    fn times_out() {
        let temp_dir = tempfile::tempdir().unwrap();
        let script = fake_command(
            temp_dir.path(),
            r#"#!/bin/sh
sleep 2
"#,
        );
        let input = temp_dir.path().join("input.pdf");
        fs::write(&input, b"%PDF fake").unwrap();
        let provider = PdfTextProvider::new(script, Duration::from_millis(50));

        let error = provider.text(&input).unwrap_err();

        assert!(matches!(error, PdfTextError::Timeout { .. }));
    }

    #[test]
    fn reports_missing_command() {
        let temp_dir = tempfile::tempdir().unwrap();
        let input = temp_dir.path().join("input.pdf");
        fs::write(&input, b"%PDF fake").unwrap();
        let provider = PdfTextProvider::new(
            temp_dir.path().join("definitely-not-pdftotext"),
            Duration::from_secs(5),
        );

        let error = provider.text(&input).unwrap_err();

        assert!(matches!(error, PdfTextError::Spawn { .. }));
    }

    #[cfg(unix)]
    fn fake_command(dir: &Path, content: &str) -> PathBuf {
        let script = dir.join("fake-pdftotext");
        fs::write(&script, content).unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        script
    }
}
