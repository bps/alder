use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use plist::Value;
use tempfile::{NamedTempFile, TempPath};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_ATTRIBUTES: &[&str] = &[
    "kMDItemWhereFroms",
    "kMDItemTextContent",
    "kMDItemAuthors",
    "kMDItemFSLabel",
];

#[derive(Debug, Clone)]
pub struct SpotlightProvider {
    command: OsString,
    attrs: Vec<String>,
    timeout: Duration,
    require_macos: bool,
}

impl SpotlightProvider {
    pub fn new(command: impl Into<OsString>, attrs: Vec<String>, timeout: Duration) -> Self {
        Self {
            command: command.into(),
            attrs,
            timeout,
            require_macos: false,
        }
    }

    pub fn facts(&self, path: impl AsRef<Path>) -> Result<IndexMap<String, Value>, SpotlightError> {
        if self.require_macos && !cfg!(target_os = "macos") {
            return Err(SpotlightError::Unavailable);
        }

        let input = path
            .as_ref()
            .canonicalize()
            .map_err(|error| SpotlightError::io("canonicalize input", path.as_ref(), error))?;
        let stdout_path = temp_path().map_err(|error| SpotlightError::tempfile("stdout", error))?;
        let stderr_path = temp_path().map_err(|error| SpotlightError::tempfile("stderr", error))?;
        let stdout_file = File::create(&stdout_path)
            .map_err(|error| SpotlightError::io("create stdout tempfile", &stdout_path, error))?;
        let stderr_file = File::create(&stderr_path)
            .map_err(|error| SpotlightError::io("create stderr tempfile", &stderr_path, error))?;

        let mut command = Command::new(&self.command);
        command.arg("-plist").arg("-");
        for attr in &self.attrs {
            command.arg("-name").arg(attr);
        }
        command
            .arg(&input)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));

        let mut child = command
            .spawn()
            .map_err(|error| SpotlightError::spawn(self.command.clone(), error))?;
        let status = wait_with_timeout(&mut child, self.timeout)?;
        let stderr = read_lossy(&stderr_path).unwrap_or_default();

        if !status.success() {
            return Err(SpotlightError::Exit { status, stderr });
        }

        let bytes = fs::read(&stdout_path)
            .map_err(|error| SpotlightError::io("read mdls plist output", &stdout_path, error))?;
        let value = Value::from_reader_xml(bytes.as_slice()).map_err(SpotlightError::ParsePlist)?;
        let dictionary = value
            .as_dictionary()
            .ok_or(SpotlightError::UnexpectedPlistRoot)?;

        let mut facts = IndexMap::new();
        for (key, value) in dictionary {
            facts.insert(format!("spotlight.{key}"), value.clone());
        }

        Ok(facts)
    }
}

impl Default for SpotlightProvider {
    fn default() -> Self {
        Self {
            command: "mdls".into(),
            attrs: DEFAULT_ATTRIBUTES
                .iter()
                .map(|attr| (*attr).to_string())
                .collect(),
            timeout: DEFAULT_TIMEOUT,
            require_macos: true,
        }
    }
}

#[derive(Debug)]
pub enum SpotlightError {
    Unavailable,
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
    ParsePlist(plist::Error),
    UnexpectedPlistRoot,
}

impl SpotlightError {
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

impl fmt::Display for SpotlightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => write!(f, "Spotlight mdls provider is only available on macOS"),
            Self::Io { op, path, source } => {
                write!(f, "failed to {op} for {}: {source}", path.display())
            }
            Self::TempFile { purpose, source } => {
                write!(f, "failed to create {purpose} tempfile: {source}")
            }
            Self::Spawn { command, source } => write!(f, "failed to spawn {:?}: {source}", command),
            Self::Timeout { timeout } => write!(f, "mdls timed out after {timeout:?}"),
            Self::Exit { status, stderr } => {
                write!(f, "mdls exited with {status}: {}", stderr.trim())
            }
            Self::ParsePlist(error) => write!(f, "failed to parse mdls plist output: {error}"),
            Self::UnexpectedPlistRoot => write!(f, "mdls plist output was not a dictionary"),
        }
    }
}

impl std::error::Error for SpotlightError {}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<ExitStatus, SpotlightError> {
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| SpotlightError::io("wait for mdls", "mdls", error))?
        {
            return Ok(status);
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SpotlightError::Timeout { timeout });
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
    fn returns_spotlight_facts_from_fake_mdls_plist() {
        let temp_dir = tempfile::tempdir().unwrap();
        let script = fake_command(
            temp_dir.path(),
            r#"#!/bin/sh
cat <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>kMDItemTextContent</key>
  <string>hello spotlight</string>
  <key>kMDItemAuthors</key>
  <array><string>Ada</string><string>Grace</string></array>
</dict>
</plist>
PLIST
"#,
        );
        let input = temp_dir.path().join("input.pdf");
        fs::write(&input, b"%PDF fake").unwrap();
        let provider = SpotlightProvider::new(
            script,
            vec![
                "kMDItemTextContent".to_string(),
                "kMDItemAuthors".to_string(),
            ],
            Duration::from_secs(5),
        );

        let facts = provider.facts(&input).unwrap();

        assert_eq!(
            facts.get("spotlight.kMDItemTextContent"),
            Some(&Value::String("hello spotlight".to_string()))
        );
        assert!(matches!(
            facts.get("spotlight.kMDItemAuthors"),
            Some(Value::Array(authors)) if authors.len() == 2
        ));
    }

    #[cfg(unix)]
    #[test]
    fn reports_nonzero_exit_with_stderr() {
        let temp_dir = tempfile::tempdir().unwrap();
        let script = fake_command(
            temp_dir.path(),
            r#"#!/bin/sh
echo 'metadata unavailable' >&2
exit 3
"#,
        );
        let input = temp_dir.path().join("input.pdf");
        fs::write(&input, b"%PDF fake").unwrap();
        let provider = SpotlightProvider::new(
            script,
            vec!["kMDItemTextContent".to_string()],
            Duration::from_secs(5),
        );

        let error = provider.facts(&input).unwrap_err();

        assert!(matches!(error, SpotlightError::Exit { .. }));
        assert!(error.to_string().contains("metadata unavailable"));
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
        let provider = SpotlightProvider::new(
            script,
            vec!["kMDItemTextContent".to_string()],
            Duration::from_millis(50),
        );

        let error = provider.facts(&input).unwrap_err();

        assert!(matches!(error, SpotlightError::Timeout { .. }));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn default_provider_is_unavailable_off_macos() {
        let provider = SpotlightProvider::default();

        let error = provider.facts("does-not-need-to-exist.pdf").unwrap_err();

        assert!(matches!(error, SpotlightError::Unavailable));
    }

    #[cfg(unix)]
    fn fake_command(dir: &Path, content: &str) -> PathBuf {
        let script = dir.join("fake-mdls");
        fs::write(&script, content).unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        script
    }
}
