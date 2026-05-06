use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::config::ConflictPolicy;
use crate::facts::file::{FileFactError, FileFacts};
use crate::planning::{ActionPlan, PlannedAction};

const MAX_APPEND_COUNTER_ATTEMPTS: u32 = 1000;

#[derive(Debug, Clone)]
pub struct ExecuteOptions {
    pub dry_run: bool,
    pub destination_roots: Vec<PathBuf>,
    pub action_log_path: PathBuf,
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionReport {
    pub records: Vec<ExecutionRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionRecord {
    pub action: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub status: ExecutionStatus,
    pub reason: Option<String>,
    pub rule_id: String,
    pub sha256: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Planned,
    InProgress,
    Moved,
    Skipped,
    Failed,
    Undone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ActionLogRecord {
    schema_version: u32,
    ts_unix_ms: u128,
    #[serde(default)]
    action_id: String,
    #[serde(default)]
    undoes_action_id: Option<String>,
    run_id: String,
    rule_id: String,
    action: String,
    status: ExecutionStatus,
    from: PathBuf,
    to: PathBuf,
    sha256: Option<String>,
    size: Option<u64>,
    reason: Option<String>,
}

pub fn execute_plan(
    plan: &ActionPlan,
    options: &ExecuteOptions,
) -> Result<ExecutionReport, ExecuteError> {
    let mut records = Vec::new();

    if options.dry_run {
        for action in &plan.actions {
            records.push(planned_record(plan, action)?);
        }
        return Ok(ExecutionReport { records });
    }

    if options.destination_roots.is_empty() {
        return Err(ExecuteError::NoDestinationRoots);
    }

    let mut log = ActionLog::open(&options.action_log_path)?;
    for action in &plan.actions {
        records.push(execute_action(plan, action, options, &mut log)?);
    }

    Ok(ExecutionReport { records })
}

fn planned_record(
    plan: &ActionPlan,
    action: &PlannedAction,
) -> Result<ExecutionRecord, ExecuteError> {
    match action {
        PlannedAction::Move { to, .. } => Ok(ExecutionRecord {
            action: "move".to_string(),
            source: plan.source.clone(),
            destination: resolve_destination(to)?,
            status: ExecutionStatus::Planned,
            reason: None,
            rule_id: plan.rule_id.clone(),
            sha256: None,
            size: None,
        }),
    }
}

fn execute_action(
    plan: &ActionPlan,
    action: &PlannedAction,
    options: &ExecuteOptions,
    log: &mut ActionLog,
) -> Result<ExecutionRecord, ExecuteError> {
    match action {
        PlannedAction::Move { to, conflict, .. } => execute_move(plan, to, *conflict, options, log),
    }
}

fn execute_move(
    plan: &ActionPlan,
    to: &Path,
    conflict: ConflictPolicy,
    options: &ExecuteOptions,
    log: &mut ActionLog,
) -> Result<ExecutionRecord, ExecuteError> {
    reject_unsafe_source(&plan.source)?;
    let source_facts = FileFacts::from_path(&plan.source)?;
    let source = source_facts.path().to_path_buf();
    let requested_destination = resolve_destination(to)?;
    ensure_destination_in_roots(&requested_destination, &options.destination_roots)?;

    let destination = resolve_conflict(&requested_destination, conflict)?;
    if destination != requested_destination {
        ensure_destination_in_roots(&destination, &options.destination_roots)?;
    }

    match conflict {
        ConflictPolicy::Skip if requested_destination.exists() => {
            return Ok(ExecutionRecord {
                action: "move".to_string(),
                source,
                destination: requested_destination,
                status: ExecutionStatus::Skipped,
                reason: Some("destination exists".to_string()),
                rule_id: plan.rule_id.clone(),
                sha256: None,
                size: Some(source_facts.size()),
            });
        }
        ConflictPolicy::Review if requested_destination.exists() => {
            return Ok(ExecutionRecord {
                action: "move".to_string(),
                source,
                destination: requested_destination,
                status: ExecutionStatus::Skipped,
                reason: Some("destination exists; review required".to_string()),
                rule_id: plan.rule_id.clone(),
                sha256: None,
                size: Some(source_facts.size()),
            });
        }
        ConflictPolicy::ReplaceIfSameHash => {
            return Err(ExecuteError::UnsupportedConflict(conflict));
        }
        _ => {}
    }

    let sha256 = source_facts.sha256()?.to_string();
    let size = source_facts.size();
    let action_id = new_action_id(&options.run_id, &source, &destination);
    let intent = ActionLogRecord::new(
        action_id.clone(),
        None,
        &options.run_id,
        &plan.rule_id,
        "move",
        ExecutionStatus::InProgress,
        &source,
        &destination,
        Some(sha256.clone()),
        Some(size),
        None,
    );
    log.append(&intent)?;

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| ExecuteError::io("create destination parent", parent, error))?;
        ensure_destination_in_roots(&destination, &options.destination_roots)?;
    }

    move_without_overwrite(&source, &destination)?;

    let committed = ActionLogRecord::new(
        action_id,
        None,
        &options.run_id,
        &plan.rule_id,
        "move",
        ExecutionStatus::Moved,
        &source,
        &destination,
        Some(sha256.clone()),
        Some(size),
        None,
    );
    log.append(&committed)?;

    Ok(ExecutionRecord {
        action: "move".to_string(),
        source,
        destination,
        status: ExecutionStatus::Moved,
        reason: None,
        rule_id: plan.rule_id.clone(),
        sha256: Some(sha256),
        size: Some(size),
    })
}

fn reject_unsafe_source(source: &Path) -> Result<(), ExecuteError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| ExecuteError::io("read source metadata", source, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ExecuteError::UnsafeSource(source.to_path_buf()));
    }
    Ok(())
}

fn resolve_conflict(
    requested_destination: &Path,
    conflict: ConflictPolicy,
) -> Result<PathBuf, ExecuteError> {
    if !requested_destination.exists() {
        return Ok(requested_destination.to_path_buf());
    }

    match conflict {
        ConflictPolicy::Error => Err(ExecuteError::DestinationExists(
            requested_destination.to_path_buf(),
        )),
        ConflictPolicy::Skip | ConflictPolicy::Review => Ok(requested_destination.to_path_buf()),
        ConflictPolicy::AppendCounter => append_counter_destination(requested_destination),
        ConflictPolicy::ReplaceIfSameHash => Err(ExecuteError::UnsupportedConflict(conflict)),
    }
}

fn append_counter_destination(path: &Path) -> Result<PathBuf, ExecuteError> {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let extension = path.extension().and_then(|value| value.to_str());

    for counter in 2..=MAX_APPEND_COUNTER_ATTEMPTS {
        let filename = match extension {
            Some(extension) => format!("{stem} {counter}.{extension}"),
            None => format!("{stem} {counter}"),
        };
        let candidate = parent.join(filename);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(ExecuteError::ConflictExhausted(path.to_path_buf()))
}

fn move_without_overwrite(source: &Path, destination: &Path) -> Result<(), ExecuteError> {
    fs::hard_link(source, destination).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            ExecuteError::DestinationExists(destination.to_path_buf())
        } else {
            ExecuteError::io("create destination hard link", destination, error)
        }
    })?;

    fs::remove_file(source).map_err(|error| {
        let _ = fs::remove_file(destination);
        ExecuteError::io("remove source after move", source, error)
    })?;

    Ok(())
}

fn resolve_destination(path: &Path) -> Result<PathBuf, ExecuteError> {
    let text = path.as_os_str().to_string_lossy();
    let expanded = if text == "~" || text.starts_with("~/") {
        let home = env::var_os("HOME").ok_or(ExecuteError::HomeUnavailable)?;
        let home = PathBuf::from(home);
        if text == "~" {
            home
        } else {
            home.join(&text[2..])
        }
    } else if text.starts_with('~') {
        return Err(ExecuteError::UnsupportedTilde(path.to_path_buf()));
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        return Err(ExecuteError::RelativeDestination(path.to_path_buf()));
    };

    normalize_no_parent(&expanded)
}

fn normalize_no_parent(path: &Path) -> Result<PathBuf, ExecuteError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
            Component::ParentDir => {
                return Err(ExecuteError::UnsafeDestination(path.to_path_buf()));
            }
        }
    }
    Ok(normalized)
}

fn ensure_destination_in_roots(destination: &Path, roots: &[PathBuf]) -> Result<(), ExecuteError> {
    let canonical_roots = roots
        .iter()
        .map(|root| {
            root.canonicalize()
                .map_err(|error| ExecuteError::io("canonicalize destination root", root, error))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let parent = destination
        .parent()
        .ok_or_else(|| ExecuteError::UnsafeDestination(destination.to_path_buf()))?;
    let existing_parent = deepest_existing_parent(parent)
        .ok_or_else(|| ExecuteError::UnsafeDestination(destination.to_path_buf()))?;
    let canonical_existing = existing_parent.canonicalize().map_err(|error| {
        ExecuteError::io("canonicalize destination parent", &existing_parent, error)
    })?;

    if canonical_roots
        .iter()
        .any(|root| canonical_existing.starts_with(root))
    {
        Ok(())
    } else {
        Err(ExecuteError::DestinationOutsideRoots {
            destination: destination.to_path_buf(),
            roots: canonical_roots,
        })
    }
}

fn deepest_existing_parent(path: &Path) -> Option<PathBuf> {
    let mut current = path.to_path_buf();
    loop {
        if current.exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

struct ActionLog {
    file: File,
}

impl ActionLog {
    fn open(path: &Path) -> Result<Self, ExecuteError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ExecuteError::io("create action log parent", parent, error))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .map_err(|error| ExecuteError::io("open action log", path, error))?;
        file.lock_exclusive()
            .map_err(|error| ExecuteError::io("lock action log", path, error))?;
        Ok(Self { file })
    }

    fn read_records(&mut self) -> Result<Vec<ActionLogRecord>, ExecuteError> {
        use std::io::{Read, Seek, SeekFrom};

        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|error| ExecuteError::io("seek action log", "action log", error))?;
        let mut input = String::new();
        self.file
            .read_to_string(&mut input)
            .map_err(|error| ExecuteError::io("read action log", "action log", error))?;
        input
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(ExecuteError::SerializeLog))
            .collect()
    }

    fn append(&mut self, record: &ActionLogRecord) -> Result<(), ExecuteError> {
        use std::io::{Seek, SeekFrom};

        self.file
            .seek(SeekFrom::End(0))
            .map_err(|error| ExecuteError::io("seek action log end", "action log", error))?;
        serde_json::to_writer(&mut self.file, record).map_err(ExecuteError::SerializeLog)?;
        self.file
            .write_all(b"\n")
            .map_err(|error| ExecuteError::io("write action log newline", "action log", error))?;
        self.file
            .sync_data()
            .map_err(|error| ExecuteError::io("sync action log", "action log", error))?;
        Ok(())
    }
}

impl Drop for ActionLog {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UndoReport {
    pub undone_action_id: String,
    pub restored_from: PathBuf,
    pub restored_to: PathBuf,
    pub status: ExecutionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReconcileFinding {
    pub action_id: String,
    pub action: String,
    pub from: PathBuf,
    pub to: PathBuf,
    pub status: ExecutionStatus,
    pub message: String,
}

pub fn undo_last_move(action_log_path: &Path) -> Result<UndoReport, ExecuteError> {
    let mut log = ActionLog::open(action_log_path)?;
    let records = log.read_records()?;
    let record = latest_unundone_move(&records).ok_or(ExecuteError::NothingToUndo)?;

    if record.sha256.is_none() || record.size.is_none() {
        return Err(ExecuteError::UndoRefused(
            "move record lacks hash or size; refusing undo".to_string(),
        ));
    }
    if record.from.exists() {
        return Err(ExecuteError::UndoRefused(format!(
            "source path already exists: {}",
            record.from.display()
        )));
    }
    reject_unsafe_source(&record.to)?;
    let destination_facts = FileFacts::from_path(&record.to)?;
    if Some(destination_facts.size()) != record.size {
        return Err(ExecuteError::UndoRefused(
            "destination size no longer matches action log".to_string(),
        ));
    }
    if Some(destination_facts.sha256()?.to_string()) != record.sha256 {
        return Err(ExecuteError::UndoRefused(
            "destination hash no longer matches action log".to_string(),
        ));
    }

    if let Some(parent) = record.from.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| ExecuteError::io("create undo destination parent", parent, error))?;
    }

    let undo_action_id = new_action_id(&record.run_id, &record.to, &record.from);
    let intent = ActionLogRecord::new(
        undo_action_id.clone(),
        Some(record.action_id.clone()),
        &record.run_id,
        &record.rule_id,
        "undo_move",
        ExecutionStatus::InProgress,
        &record.to,
        &record.from,
        record.sha256.clone(),
        record.size,
        Some("undo move".to_string()),
    );
    log.append(&intent)?;

    move_without_overwrite(&record.to, &record.from)?;

    let undone = ActionLogRecord::new(
        undo_action_id,
        Some(record.action_id.clone()),
        &record.run_id,
        &record.rule_id,
        "undo_move",
        ExecutionStatus::Undone,
        &record.to,
        &record.from,
        record.sha256.clone(),
        record.size,
        Some("undo move".to_string()),
    );
    log.append(&undone)?;

    Ok(UndoReport {
        undone_action_id: record.action_id,
        restored_from: record.to,
        restored_to: record.from,
        status: ExecutionStatus::Undone,
    })
}

pub fn reconcile_action_log(action_log_path: &Path) -> Result<Vec<ReconcileFinding>, ExecuteError> {
    let mut log = ActionLog::open(action_log_path)?;
    let records = log.read_records()?;
    Ok(reconcile_records(&records))
}

fn latest_unundone_move(records: &[ActionLogRecord]) -> Option<ActionLogRecord> {
    let undone: std::collections::HashSet<&str> = records
        .iter()
        .filter(|record| record.action == "undo_move" && record.status == ExecutionStatus::Undone)
        .filter_map(|record| record.undoes_action_id.as_deref())
        .collect();

    records
        .iter()
        .rev()
        .find(|record| {
            record.action == "move"
                && record.status == ExecutionStatus::Moved
                && !undone.contains(record.action_id.as_str())
        })
        .cloned()
}

fn reconcile_records(records: &[ActionLogRecord]) -> Vec<ReconcileFinding> {
    records
        .iter()
        .filter(|record| record.status == ExecutionStatus::InProgress)
        .filter(|record| {
            !records.iter().any(|later| {
                later.action_id == record.action_id
                    && matches!(
                        later.status,
                        ExecutionStatus::Moved | ExecutionStatus::Failed | ExecutionStatus::Undone
                    )
            })
        })
        .map(|record| ReconcileFinding {
            action_id: record.action_id.clone(),
            action: record.action.clone(),
            from: record.from.clone(),
            to: record.to.clone(),
            status: record.status.clone(),
            message: "in-progress action has no terminal record".to_string(),
        })
        .collect()
}

impl ActionLogRecord {
    fn new(
        action_id: String,
        undoes_action_id: Option<String>,
        run_id: &str,
        rule_id: &str,
        action: &str,
        status: ExecutionStatus,
        from: &Path,
        to: &Path,
        sha256: Option<String>,
        size: Option<u64>,
        reason: Option<String>,
    ) -> Self {
        Self {
            schema_version: 1,
            ts_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            action_id,
            undoes_action_id,
            run_id: run_id.to_string(),
            rule_id: rule_id.to_string(),
            action: action.to_string(),
            status,
            from: from.to_path_buf(),
            to: to.to_path_buf(),
            sha256,
            size,
            reason,
        }
    }
}

fn new_action_id(run_id: &str, from: &Path, to: &Path) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{run_id}:{ts}:{}:{}", from.display(), to.display())
}

#[derive(Debug)]
pub enum ExecuteError {
    NoDestinationRoots,
    RelativeDestination(PathBuf),
    UnsupportedTilde(PathBuf),
    HomeUnavailable,
    UnsafeSource(PathBuf),
    UnsafeDestination(PathBuf),
    DestinationOutsideRoots {
        destination: PathBuf,
        roots: Vec<PathBuf>,
    },
    DestinationExists(PathBuf),
    ConflictExhausted(PathBuf),
    UnsupportedConflict(ConflictPolicy),
    NothingToUndo,
    UndoRefused(String),
    FileFact(FileFactError),
    Io {
        op: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    SerializeLog(serde_json::Error),
}

impl ExecuteError {
    fn io(op: &'static str, path: impl AsRef<Path>, source: io::Error) -> Self {
        Self::Io {
            op,
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}

impl From<FileFactError> for ExecuteError {
    fn from(error: FileFactError) -> Self {
        Self::FileFact(error)
    }
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDestinationRoots => {
                write!(f, "execution requires at least one destination root")
            }
            Self::RelativeDestination(path) => write!(
                f,
                "destination {} is relative; execution requires absolute or ~/ destinations",
                path.display()
            ),
            Self::UnsupportedTilde(path) => write!(
                f,
                "destination {} uses unsupported ~user syntax",
                path.display()
            ),
            Self::HomeUnavailable => {
                write!(f, "HOME is not available for ~/ destination expansion")
            }
            Self::UnsafeSource(path) => write!(
                f,
                "unsafe source {}; expected a regular non-symlink file",
                path.display()
            ),
            Self::UnsafeDestination(path) => write!(f, "unsafe destination {}", path.display()),
            Self::DestinationOutsideRoots { destination, roots } => write!(
                f,
                "destination {} is outside configured roots {:?}",
                destination.display(),
                roots
            ),
            Self::DestinationExists(path) => {
                write!(f, "destination {} already exists", path.display())
            }
            Self::ConflictExhausted(path) => write!(
                f,
                "could not find append-counter destination for {}",
                path.display()
            ),
            Self::UnsupportedConflict(policy) => write!(
                f,
                "conflict policy {policy:?} is not supported by execution yet"
            ),
            Self::NothingToUndo => write!(f, "nothing to undo"),
            Self::UndoRefused(message) => write!(f, "undo refused: {message}"),
            Self::FileFact(error) => write!(f, "{error}"),
            Self::Io { op, path, source } => {
                write!(f, "failed to {op} for {}: {source}", path.display())
            }
            Self::SerializeLog(error) => {
                write!(f, "failed to serialize action log record: {error}")
            }
        }
    }
}

impl std::error::Error for ExecuteError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::{ActionPlan, PlannedAction};
    use indexmap::IndexMap;

    #[test]
    fn dry_run_does_not_move_or_log() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        fs::write(&source, b"abc").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(
            &source,
            &dest_root.join("source.pdf"),
            ConflictPolicy::Error,
        );

        let report = execute_plan(&plan, &options(true, &dest_root, &log)).unwrap();

        assert_eq!(report.records[0].status, ExecutionStatus::Planned);
        assert!(source.exists());
        assert!(!dest_root.join("source.pdf").exists());
        assert!(!log.exists());
    }

    #[test]
    fn moves_file_and_appends_action_log() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        fs::write(&source, b"abc").unwrap();
        let dest = dest_root.join("source.pdf");
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(&source, &dest, ConflictPolicy::Error);

        let report = execute_plan(&plan, &options(false, &dest_root, &log)).unwrap();

        assert_eq!(report.records[0].status, ExecutionStatus::Moved);
        assert!(!source.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"abc");
        let log_text = fs::read_to_string(log).unwrap();
        assert!(log_text.contains(r#""status":"in_progress""#));
        assert!(log_text.contains(r#""status":"moved""#));
    }

    #[test]
    fn append_counter_avoids_existing_destination() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        fs::write(&source, b"abc").unwrap();
        fs::write(dest_root.join("source.pdf"), b"existing").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(
            &source,
            &dest_root.join("source.pdf"),
            ConflictPolicy::AppendCounter,
        );

        let report = execute_plan(&plan, &options(false, &dest_root, &log)).unwrap();

        assert_eq!(
            report.records[0].destination,
            dest_root.join("source 2.pdf")
        );
        assert_eq!(fs::read(dest_root.join("source 2.pdf")).unwrap(), b"abc");
    }

    #[test]
    fn error_conflict_leaves_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        let dest = dest_root.join("source.pdf");
        fs::write(&source, b"abc").unwrap();
        fs::write(&dest, b"existing").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(&source, &dest, ConflictPolicy::Error);

        let error = execute_plan(&plan, &options(false, &dest_root, &log)).unwrap_err();

        assert!(matches!(error, ExecuteError::DestinationExists(_)));
        assert!(source.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"existing");
    }

    #[test]
    fn skip_conflict_leaves_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        let dest = dest_root.join("source.pdf");
        fs::write(&source, b"abc").unwrap();
        fs::write(&dest, b"existing").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(&source, &dest, ConflictPolicy::Skip);

        let report = execute_plan(&plan, &options(false, &dest_root, &log)).unwrap();

        assert_eq!(report.records[0].status, ExecutionStatus::Skipped);
        assert!(source.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"existing");
    }

    #[test]
    fn destination_outside_root_errors() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        let other_root = temp_dir.path().join("other");
        fs::create_dir(&dest_root).unwrap();
        fs::create_dir(&other_root).unwrap();
        fs::write(&source, b"abc").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(
            &source,
            &other_root.join("source.pdf"),
            ConflictPolicy::Error,
        );

        let error = execute_plan(&plan, &options(false, &dest_root, &log)).unwrap_err();

        assert!(matches!(
            error,
            ExecuteError::DestinationOutsideRoots { .. }
        ));
        assert!(source.exists());
    }

    #[cfg(unix)]
    #[test]
    fn source_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().unwrap();
        let real_source = temp_dir.path().join("real.pdf");
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        fs::write(&real_source, b"abc").unwrap();
        symlink(&real_source, &source).unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(
            &source,
            &dest_root.join("source.pdf"),
            ConflictPolicy::Error,
        );

        let error = execute_plan(&plan, &options(false, &dest_root, &log)).unwrap_err();

        assert!(matches!(error, ExecuteError::UnsafeSource(_)));
        assert!(source.exists());
    }

    #[test]
    fn undo_last_move_restores_source_and_logs_undo() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        let dest = dest_root.join("source.pdf");
        fs::write(&source, b"abc").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(&source, &dest, ConflictPolicy::Error);
        execute_plan(&plan, &options(false, &dest_root, &log)).unwrap();

        let report = undo_last_move(&log).unwrap();

        assert_eq!(report.status, ExecutionStatus::Undone);
        assert!(source.exists());
        assert!(!dest.exists());
        assert_eq!(fs::read(&source).unwrap(), b"abc");
        let log_text = fs::read_to_string(log).unwrap();
        assert!(log_text.contains(r#""action":"undo_move""#));
        assert!(log_text.contains(r#""status":"undone""#));
    }

    #[test]
    fn undo_refuses_modified_destination() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        let dest = dest_root.join("source.pdf");
        fs::write(&source, b"abc").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(&source, &dest, ConflictPolicy::Error);
        execute_plan(&plan, &options(false, &dest_root, &log)).unwrap();
        fs::write(&dest, b"changed").unwrap();

        let error = undo_last_move(&log).unwrap_err();

        assert!(matches!(error, ExecuteError::UndoRefused(_)));
        assert!(dest.exists());
        assert!(!source.exists());
    }

    #[test]
    fn reconcile_reports_orphan_in_progress_records() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let record = ActionLogRecord::new(
            "action-1".to_string(),
            None,
            "run",
            "rule",
            "move",
            ExecutionStatus::InProgress,
            Path::new("/from"),
            Path::new("/to"),
            None,
            None,
            None,
        );
        {
            let mut action_log = ActionLog::open(&log).unwrap();
            action_log.append(&record).unwrap();
        }

        let findings = reconcile_action_log(&log).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].action_id, "action-1");
    }

    fn plan(source: &Path, destination: &Path, conflict: ConflictPolicy) -> ActionPlan {
        ActionPlan {
            source: source.to_path_buf(),
            rule_id: "rule".to_string(),
            rule_name: None,
            variables: IndexMap::new(),
            actions: vec![PlannedAction::Move {
                to: destination.to_path_buf(),
                conflict,
                terminal: true,
            }],
        }
    }

    fn options(dry_run: bool, root: &Path, log: &Path) -> ExecuteOptions {
        ExecuteOptions {
            dry_run,
            destination_roots: vec![root.to_path_buf()],
            action_log_path: log.to_path_buf(),
            run_id: "test-run".to_string(),
        }
    }
}
