use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSupportScan {
    pub bundle_identifier: String,
    pub candidate_paths: Vec<PathBuf>,
}

pub fn scan_app_supporting_files(app: &Path) -> Result<AppSupportScan, AppSupportError> {
    #[cfg(target_os = "macos")]
    {
        let home = env::var_os("HOME").ok_or(AppSupportError::HomeUnavailable)?;
        let library = PathBuf::from(home).join("Library");
        scan_app_supporting_files_in_library(app, &library)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err(AppSupportError::UnsupportedPlatform)
    }
}

pub(crate) fn scan_app_supporting_files_in_library(
    app: &Path,
    library: &Path,
) -> Result<AppSupportScan, AppSupportError> {
    validate_app_bundle(app)?;
    let info = app.join("Contents/Info.plist");
    let plist = plist::Value::from_file(&info).map_err(|source| AppSupportError::Plist {
        path: info.clone(),
        source,
    })?;
    let bundle_identifier = plist_string(&plist, "CFBundleIdentifier")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppSupportError::MissingBundleIdentifier { path: info.clone() })?
        .to_string();
    validate_bundle_identifier(&bundle_identifier)?;
    let names = app_names(app, &plist);

    let mut candidates = BTreeSet::new();
    add_bundle_id_candidates(library, &bundle_identifier, &mut candidates);
    for name in names {
        add_name_candidates(library, &name, &mut candidates);
    }
    add_matching_children(
        &library.join("Preferences"),
        |name| preference_matches_bundle_id(name, &bundle_identifier),
        &mut candidates,
    );
    add_matching_children(
        &library.join("Group Containers"),
        |name| group_container_matches_bundle_id(name, &bundle_identifier),
        &mut candidates,
    );

    Ok(AppSupportScan {
        bundle_identifier,
        candidate_paths: candidates.into_iter().collect(),
    })
}

fn validate_app_bundle(app: &Path) -> Result<(), AppSupportError> {
    let metadata = fs::metadata(app).map_err(|source| AppSupportError::Io {
        op: "read app bundle metadata",
        path: app.to_path_buf(),
        source,
    })?;

    if metadata.is_dir() && is_app_bundle_path(app) {
        Ok(())
    } else {
        Err(AppSupportError::InvalidAppBundle(app.to_path_buf()))
    }
}

fn app_names(app: &Path, plist: &plist::Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for key in ["CFBundleName", "CFBundleDisplayName"] {
        if let Some(value) = plist_string(plist, key).filter(|value| safe_app_name(value)) {
            names.insert(value.to_string());
        }
    }
    if let Some(stem) = app.file_stem().and_then(|stem| stem.to_str())
        && safe_app_name(stem)
    {
        names.insert(stem.to_string());
    }
    names
}

fn plist_string<'a>(plist: &'a plist::Value, key: &str) -> Option<&'a str> {
    plist
        .as_dictionary()
        .and_then(|dictionary| dictionary.get(key))
        .and_then(|value| value.as_string())
}

fn validate_bundle_identifier(bundle_id: &str) -> Result<(), AppSupportError> {
    let valid_chars = bundle_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    let valid_segments = bundle_id
        .split('.')
        .all(|segment| !segment.is_empty() && segment != ".." && segment != ".");
    if valid_chars && valid_segments {
        Ok(())
    } else {
        Err(AppSupportError::InvalidBundleIdentifier(
            bundle_id.to_string(),
        ))
    }
}

fn safe_app_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.len() < 4 || trimmed.contains('/') || trimmed.contains(':') || trimmed.contains('\0')
    {
        return false;
    }
    !matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "app" | "helper" | "updater" | "update" | "electron" | "installer" | "uninstaller"
    )
}

fn add_bundle_id_candidates(library: &Path, bundle_id: &str, candidates: &mut BTreeSet<PathBuf>) {
    add_if_exists(
        library.join("Application Support").join(bundle_id),
        candidates,
    );
    add_if_exists(library.join("Caches").join(bundle_id), candidates);
    add_if_exists(library.join("Containers").join(bundle_id), candidates);
    add_if_exists(
        library
            .join("Preferences")
            .join(format!("{bundle_id}.plist")),
        candidates,
    );
    add_if_exists(
        library
            .join("Saved Application State")
            .join(format!("{bundle_id}.savedState")),
        candidates,
    );
    add_if_exists(library.join("Logs").join(bundle_id), candidates);
    add_if_exists(
        library.join("Application Scripts").join(bundle_id),
        candidates,
    );
    add_if_exists(
        library
            .join("LaunchAgents")
            .join(format!("{bundle_id}.plist")),
        candidates,
    );
}

fn add_name_candidates(library: &Path, name: &str, candidates: &mut BTreeSet<PathBuf>) {
    add_if_exists(library.join("Application Support").join(name), candidates);
    add_if_exists(library.join("Caches").join(name), candidates);
    add_if_exists(library.join("Logs").join(name), candidates);
}

fn add_if_exists(path: PathBuf, candidates: &mut BTreeSet<PathBuf>) {
    if path.exists() {
        candidates.insert(path);
    }
}

fn add_matching_children(
    directory: &Path,
    matches: impl Fn(&str) -> bool,
    candidates: &mut BTreeSet<PathBuf>,
) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches(&name) {
            candidates.insert(entry.path());
        }
    }
}

fn preference_matches_bundle_id(name: &str, bundle_id: &str) -> bool {
    name == format!("{bundle_id}.plist")
        || name
            .strip_prefix(bundle_id)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('-'))
}

fn group_container_matches_bundle_id(name: &str, bundle_id: &str) -> bool {
    name == bundle_id
        || name == format!("group.{bundle_id}")
        || name.ends_with(&format!(".{bundle_id}"))
}

fn is_app_bundle_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
}

#[derive(Debug, Error)]
pub enum AppSupportError {
    #[error("scan_app_supporting_files is only supported on macOS")]
    UnsupportedPlatform,
    #[error("HOME is not available for ~/Library app-support scanning")]
    HomeUnavailable,
    #[error("{} is not a macOS .app bundle directory", .0.display())]
    InvalidAppBundle(PathBuf),
    #[error("app bundle identifier {0:?} is not safe to use as a Library path component")]
    InvalidBundleIdentifier(String),
    #[error("app bundle Info.plist {} does not define CFBundleIdentifier", path.display())]
    MissingBundleIdentifier { path: PathBuf },
    #[error("failed to parse app bundle Info.plist {}: {source}", path.display())]
    Plist { path: PathBuf, source: plist::Error },
    #[error("failed to {op} for {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_existing_library_candidates_for_bundle_identifier_and_name() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Example.app");
        fs::create_dir_all(app.join("Contents")).unwrap();
        fs::write(
            app.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.example.Example</string>
  <key>CFBundleName</key><string>Example</string>
</dict></plist>
"#,
        )
        .unwrap();
        let library = temp.path().join("Library");
        fs::create_dir_all(library.join("Application Support/com.example.Example")).unwrap();
        fs::create_dir_all(library.join("Caches/Example")).unwrap();
        fs::create_dir_all(library.join("Group Containers/group.com.example.Example")).unwrap();
        fs::create_dir_all(library.join("Preferences")).unwrap();
        fs::write(
            library.join("Preferences/com.example.Example.helper.plist"),
            b"plist",
        )
        .unwrap();

        let scan = scan_app_supporting_files_in_library(&app, &library).unwrap();

        assert_eq!(scan.bundle_identifier, "com.example.Example");
        assert_eq!(
            scan.candidate_paths,
            vec![
                library.join("Application Support/com.example.Example"),
                library.join("Caches/Example"),
                library.join("Group Containers/group.com.example.Example"),
                library.join("Preferences/com.example.Example.helper.plist"),
            ]
        );
    }

    #[test]
    fn rejects_unsafe_bundle_identifiers() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Example.app");
        fs::create_dir_all(app.join("Contents")).unwrap();
        fs::write(
            app.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>../../Library/Escape</string>
</dict></plist>
"#,
        )
        .unwrap();

        let error =
            scan_app_supporting_files_in_library(&app, &temp.path().join("Library")).unwrap_err();

        assert!(matches!(error, AppSupportError::InvalidBundleIdentifier(_)));
    }

    #[test]
    fn bundle_id_child_matching_avoids_similar_prefixes() {
        assert!(preference_matches_bundle_id(
            "com.example.App.helper.plist",
            "com.example.App"
        ));
        assert!(!preference_matches_bundle_id(
            "com.example.Apple.plist",
            "com.example.App"
        ));
        assert!(group_container_matches_bundle_id(
            "TEAMID.com.example.App",
            "com.example.App"
        ));
        assert!(!group_container_matches_bundle_id(
            "TEAMID.com.example.App.helper",
            "com.example.App"
        ));
    }

    #[test]
    fn rejects_non_app_directories() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("Library");

        let error = scan_app_supporting_files_in_library(temp.path(), &library).unwrap_err();

        assert!(matches!(error, AppSupportError::InvalidAppBundle(_)));
    }
}
