use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use thiserror::Error;

use super::external::{self, ExternalCommand, ExternalError};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct PdfTextProvider {
    command: ExternalCommand,
}

impl PdfTextProvider {
    pub fn new(command: impl Into<OsString>, timeout: Duration) -> Self {
        Self {
            command: ExternalCommand::new(command, timeout),
        }
    }

    pub fn text(&self, path: impl AsRef<Path>) -> Result<String, PdfTextError> {
        let input = path
            .as_ref()
            .canonicalize()
            .map_err(|error| ExternalError::io("canonicalize input", path.as_ref(), error))?;
        let output_path = external::temp_path("output")?;

        self.command.run(|command| {
            command
                .arg("-enc")
                .arg("UTF-8")
                .arg(&input)
                .arg(&output_path)
                .stdout(Stdio::null());
        })?;

        let bytes = external::read(&output_path, "read pdftotext output")?;
        String::from_utf8(bytes).map_err(PdfTextError::InvalidUtf8)
    }
}

impl Default for PdfTextProvider {
    fn default() -> Self {
        Self::new("pdftotext", DEFAULT_TIMEOUT)
    }
}

#[derive(Debug, Error)]
pub enum PdfTextError {
    #[error(transparent)]
    External(#[from] ExternalError),
    #[error("pdftotext output was not valid UTF-8: {0}")]
    InvalidUtf8(#[source] std::string::FromUtf8Error),
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

        assert!(matches!(
            error,
            PdfTextError::External(ExternalError::Exit { .. })
        ));
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

        assert!(matches!(
            error,
            PdfTextError::External(ExternalError::Timeout { .. })
        ));
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

        assert!(matches!(
            error,
            PdfTextError::External(ExternalError::Spawn { .. })
        ));
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
