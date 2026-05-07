use std::ffi::OsString;
use std::fs::File;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use indexmap::IndexMap;
use plist::Value;
use thiserror::Error;

use super::external::{self, ExternalCommand, ExternalError};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_ATTRIBUTES: &[&str] = &[
    "kMDItemWhereFroms",
    "kMDItemTextContent",
    "kMDItemAuthors",
    "kMDItemFSLabel",
];

#[derive(Debug, Clone)]
pub struct SpotlightProvider {
    command: ExternalCommand,
    attrs: Vec<String>,
    require_macos: bool,
}

impl SpotlightProvider {
    pub fn new(command: impl Into<OsString>, attrs: Vec<String>, timeout: Duration) -> Self {
        Self {
            command: ExternalCommand::new(command, timeout),
            attrs,
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
            .map_err(|error| ExternalError::io("canonicalize input", path.as_ref(), error))?;
        let stdout_path = external::temp_path("stdout")?;
        let stdout_file = File::create(&stdout_path)
            .map_err(|error| ExternalError::io("create stdout tempfile", &stdout_path, error))?;

        self.command.run(|command| {
            command.arg("-plist").arg("-");
            for attr in &self.attrs {
                command.arg("-name").arg(attr);
            }
            command.arg(&input).stdout(Stdio::from(stdout_file));
        })?;

        let bytes = external::read(&stdout_path, "read mdls plist output")?;
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
            command: ExternalCommand::new("mdls", DEFAULT_TIMEOUT),
            attrs: DEFAULT_ATTRIBUTES
                .iter()
                .map(|attr| (*attr).to_string())
                .collect(),
            require_macos: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum SpotlightError {
    #[error("Spotlight mdls provider is only available on macOS")]
    Unavailable,
    #[error(transparent)]
    External(#[from] ExternalError),
    #[error("failed to parse mdls plist output: {0}")]
    ParsePlist(#[source] plist::Error),
    #[error("mdls plist output was not a dictionary")]
    UnexpectedPlistRoot,
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::external::ExternalError;
    use std::fs;
    use std::path::PathBuf;

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

        assert!(matches!(
            error,
            SpotlightError::External(ExternalError::Exit { .. })
        ));
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

        assert!(matches!(
            error,
            SpotlightError::External(ExternalError::Timeout { .. })
        ));
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
