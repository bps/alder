use std::io;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

pub fn expand_user_path(path: &str) -> Result<PathBuf, PathError> {
    let expanded = if path == "~" {
        home_dir()?
    } else if let Some(rest) = path.strip_prefix("~/") {
        home_dir()?.join(rest)
    } else if path.starts_with('~') {
        return Err(PathError::UnsupportedTilde(path.to_string()));
    } else {
        PathBuf::from(path)
    };

    let normalized = normalize_no_parent(&expanded)?;
    if normalized.is_absolute() {
        Ok(normalized)
    } else {
        std::path::absolute(&normalized).map_err(|source| PathError::Io {
            path: normalized,
            source,
        })
    }
}

fn home_dir() -> Result<PathBuf, PathError> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or(PathError::HomeUnavailable)
}

fn normalize_no_parent(path: &Path) -> Result<PathBuf, PathError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
            Component::ParentDir => return Err(PathError::ParentDir(path.to_path_buf())),
        }
    }
    Ok(normalized)
}

#[derive(Debug, Error)]
pub enum PathError {
    #[error("HOME is not available for ~/ expansion")]
    HomeUnavailable,
    #[error("unsupported ~user path {0:?}")]
    UnsupportedTilde(String),
    #[error("path must not contain ..: {}", .0.display())]
    ParentDir(PathBuf),
    #[error("failed to resolve {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_relative_paths_absolute() {
        let expanded = expand_user_path("relative/path").unwrap();

        assert!(expanded.is_absolute());
        assert!(expanded.ends_with("relative/path"));
    }

    #[test]
    fn rejects_unsupported_tilde_paths() {
        let error = expand_user_path("~other/path").unwrap_err();

        assert!(matches!(error, PathError::UnsupportedTilde(path) if path == "~other/path"));
    }

    #[test]
    fn rejects_parent_components_after_expansion() {
        let error = expand_user_path("~/../escape").unwrap_err();

        assert!(matches!(error, PathError::ParentDir(_)));
    }

    #[test]
    fn removes_current_dir_components() {
        let expanded = expand_user_path("relative/./path").unwrap();

        assert!(expanded.ends_with("relative/path"));
    }
}
