use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug)]
pub struct FileFacts {
    path: PathBuf,
    name: String,
    stem: String,
    ext: String,
    size: u64,
    created_at: Option<SystemTime>,
    modified_at: Option<SystemTime>,
    sha256: OnceLock<Result<String, FileFactError>>,
}

impl FileFacts {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, FileFactError> {
        let path = std::path::absolute(path.as_ref())
            .map_err(|error| FileFactError::io("make path absolute", path.as_ref(), error))?;
        let metadata = fs::metadata(&path)
            .map_err(|error| FileFactError::io("read metadata", &path, error))?;

        if !metadata.is_file() {
            return Err(FileFactError::not_regular_file(&path));
        }

        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!(".{ext}"))
            .unwrap_or_default();

        Ok(Self {
            path,
            name,
            stem,
            ext,
            size: metadata.len(),
            created_at: metadata.created().ok(),
            modified_at: metadata.modified().ok(),
            sha256: OnceLock::new(),
        })
    }

    pub fn get(&self, name: &str) -> Result<Option<FileFactValue>, FileFactError> {
        let value = match name {
            "file.path" => FileFactValue::Path(self.path.clone()),
            "file.name" => FileFactValue::String(self.name.clone()),
            "file.stem" => FileFactValue::String(self.stem.clone()),
            "file.ext" => FileFactValue::String(self.ext.clone()),
            "file.size" => FileFactValue::Unsigned(self.size),
            "file.created_at" => match self.created_at {
                Some(time) => FileFactValue::Time(time),
                None => return Ok(None),
            },
            "file.modified_at" => match self.modified_at {
                Some(time) => FileFactValue::Time(time),
                None => return Ok(None),
            },
            "file.sha256" => FileFactValue::String(self.sha256()?.to_string()),
            _ => return Ok(None),
        };

        Ok(Some(value))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn stem(&self) -> &str {
        &self.stem
    }

    pub fn ext(&self) -> &str {
        &self.ext
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn created_at(&self) -> Option<SystemTime> {
        self.created_at
    }

    pub fn modified_at(&self) -> Option<SystemTime> {
        self.modified_at
    }

    pub fn sha256(&self) -> Result<&str, FileFactError> {
        self.sha256
            .get_or_init(|| sha256_file(&self.path))
            .as_ref()
            .map(String::as_str)
            .map_err(Clone::clone)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileFactValue {
    Path(PathBuf),
    String(String),
    Unsigned(u64),
    Time(SystemTime),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FileFactError {
    #[error("failed to {op} for {} ({kind:?}): {message}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        kind: io::ErrorKind,
        message: String,
    },
    #[error("{} is not a regular file", path.display())]
    NotRegularFile { path: PathBuf },
}

impl FileFactError {
    fn io(op: &'static str, path: impl AsRef<Path>, error: io::Error) -> Self {
        Self::Io {
            op,
            path: path.as_ref().to_path_buf(),
            kind: error.kind(),
            message: error.to_string(),
        }
    }

    fn not_regular_file(path: impl AsRef<Path>) -> Self {
        Self::NotRegularFile {
            path: path.as_ref().to_path_buf(),
        }
    }
}

fn sha256_file(path: &Path) -> Result<String, FileFactError> {
    let mut file = File::open(path).map_err(|error| FileFactError::io("open file", path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];

    loop {
        let bytes_read = file
            .read(&mut buffer)
            .map_err(|error| FileFactError::io("read file", path, error))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_file_facts_for_regular_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file = temp_dir.path().join("Statement.PDF");
        fs::write(&file, b"abc").unwrap();

        let facts = FileFacts::from_path(&file).unwrap();

        assert!(facts.path().is_absolute());
        assert_eq!(facts.name(), "Statement.PDF");
        assert_eq!(facts.stem(), "Statement");
        assert_eq!(facts.ext(), ".PDF");
        assert_eq!(facts.size(), 3);
        assert!(facts.modified_at().is_some());
        assert_eq!(
            facts.sha256().unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn exposes_namespaced_file_fact_values() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file = temp_dir.path().join("invoice.pdf");
        fs::write(&file, b"abc").unwrap();

        let facts = FileFacts::from_path(&file).unwrap();

        assert_eq!(
            facts.get("file.name").unwrap(),
            Some(FileFactValue::String("invoice.pdf".to_string()))
        );
        assert_eq!(
            facts.get("file.ext").unwrap(),
            Some(FileFactValue::String(".pdf".to_string()))
        );
        assert_eq!(
            facts.get("file.size").unwrap(),
            Some(FileFactValue::Unsigned(3))
        );
        assert_eq!(facts.get("pdf.text").unwrap(), None);
    }

    #[test]
    fn leaves_extension_empty_when_file_has_no_extension() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file = temp_dir.path().join("README");
        fs::write(&file, b"abc").unwrap();

        let facts = FileFacts::from_path(&file).unwrap();

        assert_eq!(facts.ext(), "");
    }

    #[test]
    fn rejects_directories() {
        let temp_dir = tempfile::tempdir().unwrap();

        let error = FileFacts::from_path(temp_dir.path()).unwrap_err();

        assert!(matches!(error, FileFactError::NotRegularFile { .. }));
    }

    #[test]
    fn hashes_lazily_and_caches_result() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file = temp_dir.path().join("lazy.pdf");
        fs::write(&file, b"abc").unwrap();

        let facts = FileFacts::from_path(&file).unwrap();
        fs::write(&file, b"changed").unwrap();

        let first_hash = facts.sha256().unwrap().to_string();
        fs::write(&file, b"changed again").unwrap();
        let second_hash = facts.sha256().unwrap().to_string();

        assert_eq!(first_hash, second_hash);
    }
}
