use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::config::ConflictPolicy;
use crate::facts::file::{FileFactError, FileFacts};
use crate::path_utils::{PathError, expand_user_path};
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
    pub action: ActionKind,
    pub source: PathBuf,
    pub destination: Option<PathBuf>,
    pub status: ExecutionStatus,
    pub reason: Option<String>,
    pub rule_id: String,
    pub sha256: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Move,
    Trash,
    UndoMove,
}

impl ActionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Trash => "trash",
            Self::UndoMove => "undo_move",
        }
    }
}

impl fmt::Display for ActionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Planned,
    InProgress,
    Moved,
    Skipped,
    Failed,
    Deduped,
    Trashed,
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
    action: ActionKind,
    status: ExecutionStatus,
    from: PathBuf,
    to: Option<PathBuf>,
    sha256: Option<String>,
    size: Option<u64>,
    reason: Option<String>,
}

struct ActionLogRecordInput<'a> {
    action_id: String,
    undoes_action_id: Option<String>,
    run_id: &'a str,
    rule_id: &'a str,
    action: ActionKind,
    status: ExecutionStatus,
    from: &'a Path,
    to: Option<&'a Path>,
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

    if options.destination_roots.is_empty()
        && plan
            .actions
            .iter()
            .any(|action| matches!(action, PlannedAction::Move { .. }))
    {
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
            action: ActionKind::Move,
            source: plan.source.clone(),
            destination: Some(resolve_destination(to)?),
            status: ExecutionStatus::Planned,
            reason: None,
            rule_id: plan.rule_id.clone(),
            sha256: None,
            size: None,
        }),
        PlannedAction::Trash { .. } => Ok(ExecutionRecord {
            action: ActionKind::Trash,
            source: plan.source.clone(),
            destination: None,
            status: ExecutionStatus::Planned,
            reason: Some("would move source to operating system trash/recycle bin".to_string()),
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
        PlannedAction::Trash { .. } => execute_trash(plan, options, log),
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
    if conflict == ConflictPolicy::ReplaceIfSameHash && requested_destination.exists() {
        return dedupe_if_same_hash(plan, &source_facts, &requested_destination, options, log);
    }

    let destination = resolve_conflict(&requested_destination, conflict)?;
    if destination != requested_destination {
        ensure_destination_in_roots(&destination, &options.destination_roots)?;
    }

    match conflict {
        ConflictPolicy::Skip if requested_destination.exists() => {
            return Ok(ExecutionRecord {
                action: ActionKind::Move,
                source,
                destination: Some(requested_destination),
                status: ExecutionStatus::Skipped,
                reason: Some("destination exists".to_string()),
                rule_id: plan.rule_id.clone(),
                sha256: None,
                size: Some(source_facts.size()),
            });
        }
        ConflictPolicy::Review if requested_destination.exists() => {
            return Ok(ExecutionRecord {
                action: ActionKind::Move,
                source,
                destination: Some(requested_destination),
                status: ExecutionStatus::Skipped,
                reason: Some("destination exists; review required".to_string()),
                rule_id: plan.rule_id.clone(),
                sha256: None,
                size: Some(source_facts.size()),
            });
        }
        _ => {}
    }

    let sha256 = source_facts.sha256()?.to_string();
    let size = source_facts.size();
    let action_id = new_action_id();
    let intent = ActionLogRecord::new(ActionLogRecordInput {
        action_id: action_id.clone(),
        undoes_action_id: None,
        run_id: &options.run_id,
        rule_id: &plan.rule_id,
        action: ActionKind::Move,
        status: ExecutionStatus::InProgress,
        from: &source,
        to: Some(&destination),
        sha256: Some(sha256.clone()),
        size: Some(size),
        reason: None,
    });
    log.append(&intent)?;

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| ExecuteError::io("create destination parent", parent, error))?;
        ensure_destination_in_roots(&destination, &options.destination_roots)?;
    }

    move_without_overwrite(&source, &destination)?;

    let committed = ActionLogRecord::new(ActionLogRecordInput {
        action_id,
        undoes_action_id: None,
        run_id: &options.run_id,
        rule_id: &plan.rule_id,
        action: ActionKind::Move,
        status: ExecutionStatus::Moved,
        from: &source,
        to: Some(&destination),
        sha256: Some(sha256.clone()),
        size: Some(size),
        reason: None,
    });
    log.append(&committed)?;

    Ok(ExecutionRecord {
        action: ActionKind::Move,
        source,
        destination: Some(destination),
        status: ExecutionStatus::Moved,
        reason: None,
        rule_id: plan.rule_id.clone(),
        sha256: Some(sha256),
        size: Some(size),
    })
}

fn execute_trash(
    plan: &ActionPlan,
    options: &ExecuteOptions,
    log: &mut ActionLog,
) -> Result<ExecutionRecord, ExecuteError> {
    reject_unsafe_source(&plan.source)?;
    let source_facts = FileFacts::from_path(&plan.source)?;
    let source = source_facts.path().to_path_buf();
    let size = source_facts.size();
    let action_id = new_action_id();
    let reason = Some("move to operating system trash/recycle bin".to_string());
    let intent = ActionLogRecord::new(ActionLogRecordInput {
        action_id: action_id.clone(),
        undoes_action_id: None,
        run_id: &options.run_id,
        rule_id: &plan.rule_id,
        action: ActionKind::Trash,
        status: ExecutionStatus::InProgress,
        from: &source,
        to: None,
        sha256: None,
        size: Some(size),
        reason: reason.clone(),
    });
    log.append(&intent)?;

    if let Err(error) = trash::delete(&source) {
        let failed = ActionLogRecord::new(ActionLogRecordInput {
            action_id,
            undoes_action_id: None,
            run_id: &options.run_id,
            rule_id: &plan.rule_id,
            action: ActionKind::Trash,
            status: ExecutionStatus::Failed,
            from: &source,
            to: None,
            sha256: None,
            size: Some(size),
            reason: Some(format!("trash operation failed: {error}")),
        });
        log.append(&failed)?;
        return Err(ExecuteError::Trash(error));
    }

    let committed = ActionLogRecord::new(ActionLogRecordInput {
        action_id,
        undoes_action_id: None,
        run_id: &options.run_id,
        rule_id: &plan.rule_id,
        action: ActionKind::Trash,
        status: ExecutionStatus::Trashed,
        from: &source,
        to: None,
        sha256: None,
        size: Some(size),
        reason: reason.clone(),
    });
    log.append(&committed)?;

    Ok(ExecutionRecord {
        action: ActionKind::Trash,
        source,
        destination: None,
        status: ExecutionStatus::Trashed,
        reason,
        rule_id: plan.rule_id.clone(),
        sha256: None,
        size: Some(size),
    })
}

fn dedupe_if_same_hash(
    plan: &ActionPlan,
    source_facts: &FileFacts,
    destination: &Path,
    options: &ExecuteOptions,
    log: &mut ActionLog,
) -> Result<ExecutionRecord, ExecuteError> {
    let source = source_facts.path().to_path_buf();
    if source == destination {
        return Err(ExecuteError::SameSourceDestination(source));
    }
    reject_unsafe_source(destination)?;
    let destination_facts = FileFacts::from_path(destination)?;
    let source_hash = source_facts.sha256()?.to_string();
    let destination_hash = destination_facts.sha256()?.to_string();
    if source_facts.size() != destination_facts.size() || source_hash != destination_hash {
        return Err(ExecuteError::HashMismatch {
            source_path: source,
            destination: destination.to_path_buf(),
            source_hash,
            destination_hash,
        });
    }

    let fresh_source = FileFacts::from_path(&source)?;
    if fresh_source.size() != source_facts.size() || fresh_source.sha256()? != source_hash {
        return Err(ExecuteError::UndoRefused(
            "source changed while verifying replace_if_same_hash".to_string(),
        ));
    }

    fs::remove_file(&source)
        .map_err(|error| ExecuteError::io("remove duplicate source", &source, error))?;

    let action_id = new_action_id();
    let record = ActionLogRecord::new(ActionLogRecordInput {
        action_id,
        undoes_action_id: None,
        run_id: &options.run_id,
        rule_id: &plan.rule_id,
        action: ActionKind::Move,
        status: ExecutionStatus::Deduped,
        from: &source,
        to: Some(destination),
        sha256: Some(source_hash.clone()),
        size: Some(source_facts.size()),
        reason: Some("destination already has same hash; removed source duplicate".to_string()),
    });
    log.append(&record)?;

    Ok(ExecutionRecord {
        action: ActionKind::Move,
        source,
        destination: Some(destination.to_path_buf()),
        status: ExecutionStatus::Deduped,
        reason: Some("destination already has same hash; removed source duplicate".to_string()),
        rule_id: plan.rule_id.clone(),
        sha256: Some(source_hash),
        size: Some(source_facts.size()),
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
        ConflictPolicy::ReplaceIfSameHash => Ok(requested_destination.to_path_buf()),
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
    if !text.starts_with('~') && !path.is_absolute() {
        return Err(ExecuteError::RelativeDestination(path.to_path_buf()));
    }

    expand_user_path(&text).map_err(|error| destination_path_error(error, path))
}

fn destination_path_error(error: PathError, original: &Path) -> ExecuteError {
    match error {
        PathError::HomeUnavailable => ExecuteError::HomeUnavailable,
        PathError::UnsupportedTilde(_) => ExecuteError::UnsupportedTilde(original.to_path_buf()),
        PathError::ParentDir(path) => ExecuteError::UnsafeDestination(path),
        PathError::Io { path, source } => ExecuteError::io("resolve destination", path, source),
    }
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
    pub action: ActionKind,
    pub from: PathBuf,
    pub to: Option<PathBuf>,
    pub status: ExecutionStatus,
    pub message: String,
}

pub fn undo_last_move(action_log_path: &Path) -> Result<UndoReport, ExecuteError> {
    let mut log = ActionLog::open(action_log_path)?;
    let records = log.read_records()?;
    if let Some(record) = latest_unundone_terminal_record(&records)
        && record.action == ActionKind::Trash
    {
        return Err(ExecuteError::UndoRefused(format!(
            "last action is trash for {}; restore it from the operating system Trash/Recycle Bin",
            record.from.display()
        )));
    }
    let record = latest_unundone_move(&records).ok_or(ExecuteError::NothingToUndo)?;
    let destination = record.to.as_ref().ok_or_else(|| {
        ExecuteError::UndoRefused("move record lacks destination; refusing undo".to_string())
    })?;

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
    reject_unsafe_source(destination)?;
    let destination_facts = FileFacts::from_path(destination)?;
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

    let undo_action_id = new_action_id();
    let intent = ActionLogRecord::new(ActionLogRecordInput {
        action_id: undo_action_id.clone(),
        undoes_action_id: Some(record.action_id.clone()),
        run_id: &record.run_id,
        rule_id: &record.rule_id,
        action: ActionKind::UndoMove,
        status: ExecutionStatus::InProgress,
        from: destination,
        to: Some(&record.from),
        sha256: record.sha256.clone(),
        size: record.size,
        reason: Some("undo move".to_string()),
    });
    log.append(&intent)?;

    move_without_overwrite(destination, &record.from)?;

    let undone = ActionLogRecord::new(ActionLogRecordInput {
        action_id: undo_action_id,
        undoes_action_id: Some(record.action_id.clone()),
        run_id: &record.run_id,
        rule_id: &record.rule_id,
        action: ActionKind::UndoMove,
        status: ExecutionStatus::Undone,
        from: destination,
        to: Some(&record.from),
        sha256: record.sha256.clone(),
        size: record.size,
        reason: Some("undo move".to_string()),
    });
    log.append(&undone)?;

    Ok(UndoReport {
        undone_action_id: record.action_id,
        restored_from: destination.to_path_buf(),
        restored_to: record.from,
        status: ExecutionStatus::Undone,
    })
}

fn latest_unundone_terminal_record(records: &[ActionLogRecord]) -> Option<&ActionLogRecord> {
    let undone: std::collections::HashSet<&str> = records
        .iter()
        .filter(|record| {
            record.action == ActionKind::UndoMove && record.status == ExecutionStatus::Undone
        })
        .filter_map(|record| record.undoes_action_id.as_deref())
        .collect();

    records.iter().rev().find(|record| {
        !undone.contains(record.action_id.as_str())
            && matches!(
                (record.action, &record.status),
                (ActionKind::Move, ExecutionStatus::Moved)
                    | (ActionKind::Trash, ExecutionStatus::Trashed)
            )
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
        .filter(|record| {
            record.action == ActionKind::UndoMove && record.status == ExecutionStatus::Undone
        })
        .filter_map(|record| record.undoes_action_id.as_deref())
        .collect();

    records
        .iter()
        .rev()
        .find(|record| {
            record.action == ActionKind::Move
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
                        ExecutionStatus::Moved
                            | ExecutionStatus::Failed
                            | ExecutionStatus::Trashed
                            | ExecutionStatus::Undone
                    )
            })
        })
        .map(|record| ReconcileFinding {
            action_id: record.action_id.clone(),
            action: record.action,
            from: record.from.clone(),
            to: record.to.clone(),
            status: record.status.clone(),
            message: "in-progress action has no terminal record".to_string(),
        })
        .collect()
}

impl ActionLogRecord {
    fn new(input: ActionLogRecordInput<'_>) -> Self {
        let ActionLogRecordInput {
            action_id,
            undoes_action_id,
            run_id,
            rule_id,
            action,
            status,
            from,
            to,
            sha256,
            size,
            reason,
        } = input;

        Self {
            schema_version: 2,
            ts_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            action_id,
            undoes_action_id,
            run_id: run_id.to_string(),
            rule_id: rule_id.to_string(),
            action,
            status,
            from: from.to_path_buf(),
            to: to.map(Path::to_path_buf),
            sha256,
            size,
            reason,
        }
    }
}

fn new_action_id() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Debug, Error)]
pub enum ExecuteError {
    #[error("execution requires at least one destination root")]
    NoDestinationRoots,
    #[error("destination {} is relative; execution requires absolute or ~/ destinations", .0.display())]
    RelativeDestination(PathBuf),
    #[error("destination {} uses unsupported ~user syntax", .0.display())]
    UnsupportedTilde(PathBuf),
    #[error("HOME is not available for ~/ destination expansion")]
    HomeUnavailable,
    #[error("unsafe source {}; expected a regular non-symlink file", .0.display())]
    UnsafeSource(PathBuf),
    #[error("unsafe destination {}", .0.display())]
    UnsafeDestination(PathBuf),
    #[error("destination {} is outside configured roots {roots:?}", destination.display())]
    DestinationOutsideRoots {
        destination: PathBuf,
        roots: Vec<PathBuf>,
    },
    #[error("destination {} already exists", .0.display())]
    DestinationExists(PathBuf),
    #[error("could not find append-counter destination for {}", .0.display())]
    ConflictExhausted(PathBuf),
    #[error("replace_if_same_hash refused: {} ({source_hash}) differs from {} ({destination_hash})", source_path.display(), destination.display())]
    HashMismatch {
        source_path: PathBuf,
        destination: PathBuf,
        source_hash: String,
        destination_hash: String,
    },
    #[error("source and destination are the same path: {}", .0.display())]
    SameSourceDestination(PathBuf),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("undo refused: {0}")]
    UndoRefused(String),
    #[error("failed to move source to operating system trash/recycle bin: {0}")]
    Trash(#[from] trash::Error),
    #[error(transparent)]
    FileFact(#[from] FileFactError),
    #[error("failed to {op} for {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("failed to serialize action log record: {0}")]
    SerializeLog(#[source] serde_json::Error),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::{ActionPlan, PlannedAction};
    use indexmap::IndexMap;

    #[test]
    fn action_kind_uses_existing_json_names() {
        assert_eq!(
            serde_json::to_string(&ActionKind::Move).unwrap(),
            r#""move""#
        );
        assert_eq!(
            serde_json::to_string(&ActionKind::UndoMove).unwrap(),
            r#""undo_move""#
        );
        assert_eq!(
            serde_json::to_string(&ActionKind::Trash).unwrap(),
            r#""trash""#
        );
        assert_eq!(
            serde_json::from_str::<ActionKind>(r#""move""#).unwrap(),
            ActionKind::Move
        );
        assert_eq!(
            serde_json::from_str::<ActionKind>(r#""undo_move""#).unwrap(),
            ActionKind::UndoMove
        );
        assert_eq!(
            serde_json::from_str::<ActionKind>(r#""trash""#).unwrap(),
            ActionKind::Trash
        );
    }

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
    fn dry_run_trash_does_not_trash_or_log() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        fs::write(&source, b"abc").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = trash_plan(&source);

        let report = execute_plan(&plan, &options(true, &dest_root, &log)).unwrap();

        assert_eq!(report.records[0].action, ActionKind::Trash);
        assert_eq!(report.records[0].status, ExecutionStatus::Planned);
        assert_eq!(report.records[0].destination, None);
        assert!(
            report.records[0]
                .reason
                .as_deref()
                .unwrap()
                .contains("operating system trash")
        );
        assert!(source.exists());
        assert!(!log.exists());
    }

    #[test]
    fn move_execution_requires_destination_roots_but_trash_plan_does_not() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        fs::write(&source, b"abc").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let dest = temp_dir.path().join("dest/source.pdf");

        let error = execute_plan(
            &plan(&source, &dest, ConflictPolicy::Error),
            &no_root_options(&log),
        )
        .unwrap_err();

        assert!(matches!(error, ExecuteError::NoDestinationRoots));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let real_source = temp_dir.path().join("real.pdf");
            let symlink_source = temp_dir.path().join("symlink.pdf");
            fs::write(&real_source, b"abc").unwrap();
            symlink(&real_source, &symlink_source).unwrap();

            let error =
                execute_plan(&trash_plan(&symlink_source), &no_root_options(&log)).unwrap_err();

            assert!(matches!(error, ExecuteError::UnsafeSource(_)));
        }
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
    fn action_ids_are_unique_uuids() {
        let mut ids = std::collections::HashSet::new();

        for _ in 0..1000 {
            let action_id = new_action_id();
            uuid::Uuid::parse_str(&action_id).unwrap();
            assert!(ids.insert(action_id));
        }
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
            Some(dest_root.join("source 2.pdf"))
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

    #[test]
    fn relative_destination_errors_before_expansion() {
        let error = resolve_destination(Path::new("relative.pdf")).unwrap_err();

        assert!(matches!(error, ExecuteError::RelativeDestination(_)));
    }

    #[test]
    fn destination_parent_components_are_unsafe() {
        let error = resolve_destination(Path::new("~/../escape.pdf")).unwrap_err();

        assert!(matches!(error, ExecuteError::UnsafeDestination(_)));
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
    fn undo_refuses_to_reach_past_latest_trash_action() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        {
            let mut action_log = ActionLog::open(&log).unwrap();
            action_log
                .append(&ActionLogRecord::new(ActionLogRecordInput {
                    action_id: "move-1".to_string(),
                    undoes_action_id: None,
                    run_id: "run",
                    rule_id: "rule",
                    action: ActionKind::Move,
                    status: ExecutionStatus::Moved,
                    from: Path::new("/from"),
                    to: Some(Path::new("/to")),
                    sha256: Some("abc".to_string()),
                    size: Some(3),
                    reason: None,
                }))
                .unwrap();
            action_log
                .append(&ActionLogRecord::new(ActionLogRecordInput {
                    action_id: "trash-1".to_string(),
                    undoes_action_id: None,
                    run_id: "run",
                    rule_id: "rule",
                    action: ActionKind::Trash,
                    status: ExecutionStatus::Trashed,
                    from: Path::new("/trashed"),
                    to: None,
                    sha256: None,
                    size: Some(4),
                    reason: Some("move to operating system trash/recycle bin".to_string()),
                }))
                .unwrap();
        }

        let error = undo_last_move(&log).unwrap_err();

        assert!(
            matches!(error, ExecuteError::UndoRefused(message) if message.contains("Trash/Recycle Bin"))
        );
    }

    #[test]
    fn reconcile_reports_orphan_in_progress_records() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let record = ActionLogRecord::new(ActionLogRecordInput {
            action_id: "action-1".to_string(),
            undoes_action_id: None,
            run_id: "run",
            rule_id: "rule",
            action: ActionKind::Move,
            status: ExecutionStatus::InProgress,
            from: Path::new("/from"),
            to: Some(Path::new("/to")),
            sha256: None,
            size: None,
            reason: None,
        });
        {
            let mut action_log = ActionLog::open(&log).unwrap();
            action_log.append(&record).unwrap();
        }

        let findings = reconcile_action_log(&log).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].action_id, "action-1");
    }

    #[test]
    fn replace_if_same_hash_removes_duplicate_source_only() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        let dest = dest_root.join("source.pdf");
        fs::write(&source, b"abc").unwrap();
        fs::write(&dest, b"abc").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(&source, &dest, ConflictPolicy::ReplaceIfSameHash);

        let report = execute_plan(&plan, &options(false, &dest_root, &log)).unwrap();

        assert_eq!(report.records[0].status, ExecutionStatus::Deduped);
        assert!(!source.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"abc");
        let log_text = fs::read_to_string(log).unwrap();
        assert!(log_text.contains(r#""status":"deduped""#));
    }

    #[test]
    fn replace_if_same_hash_refuses_different_destination() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source = temp_dir.path().join("source.pdf");
        let dest_root = temp_dir.path().join("dest");
        fs::create_dir(&dest_root).unwrap();
        let dest = dest_root.join("source.pdf");
        fs::write(&source, b"abc").unwrap();
        fs::write(&dest, b"different").unwrap();
        let log = temp_dir.path().join("actions.jsonl");
        let plan = plan(&source, &dest, ConflictPolicy::ReplaceIfSameHash);

        let error = execute_plan(&plan, &options(false, &dest_root, &log)).unwrap_err();

        assert!(matches!(error, ExecuteError::HashMismatch { .. }));
        assert_eq!(fs::read(&source).unwrap(), b"abc");
        assert_eq!(fs::read(&dest).unwrap(), b"different");
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

    fn trash_plan(source: &Path) -> ActionPlan {
        ActionPlan {
            source: source.to_path_buf(),
            rule_id: "rule".to_string(),
            rule_name: None,
            variables: IndexMap::new(),
            actions: vec![PlannedAction::Trash { terminal: true }],
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

    fn no_root_options(log: &Path) -> ExecuteOptions {
        ExecuteOptions {
            dry_run: false,
            destination_roots: Vec::new(),
            action_log_path: log.to_path_buf(),
            run_id: "test-run".to_string(),
        }
    }
}
