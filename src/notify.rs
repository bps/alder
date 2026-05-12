use std::path::Path;
use std::process::{Command, Stdio};

use crate::execute::{ActionKind, ExecutionRecord, ExecutionStatus};

const MAX_INDIVIDUAL_NOTIFICATIONS: usize = 5;

pub fn notify_execution_records(records: &[ExecutionRecord]) {
    if records.is_empty() {
        return;
    }

    let notifications = if records.len() > MAX_INDIVIDUAL_NOTIFICATIONS {
        vec![summary_notification(records)]
    } else {
        records.iter().map(record_notification).collect()
    };

    for notification in notifications {
        deliver(&notification);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Notification {
    title: String,
    subtitle: String,
    body: String,
}

fn record_notification(record: &ExecutionRecord) -> Notification {
    Notification {
        title: format!("Alder: {}", status_label(&record.status)),
        subtitle: format!("{} by {}", action_label(record.action), record.rule_id),
        body: record_body(record),
    }
}

fn summary_notification(records: &[ExecutionRecord]) -> Notification {
    let moved = count_status(records, ExecutionStatus::Moved);
    let trashed = count_status(records, ExecutionStatus::Trashed);
    let deduped = count_status(records, ExecutionStatus::Deduped);
    let skipped = count_status(records, ExecutionStatus::Skipped);
    let failed = count_status(records, ExecutionStatus::Failed);
    let scanned = count_status(records, ExecutionStatus::Scanned);
    let parts = [
        (moved, "moved"),
        (trashed, "trashed"),
        (deduped, "deduped"),
        (skipped, "skipped"),
        (failed, "failed"),
        (scanned, "scanned"),
    ]
    .into_iter()
    .filter_map(|(count, label)| (count > 0).then_some(format!("{count} {label}")))
    .collect::<Vec<_>>();

    Notification {
        title: "Alder: batch complete".to_string(),
        subtitle: format!("{} action(s)", records.len()),
        body: if parts.is_empty() {
            "No terminal actions recorded".to_string()
        } else {
            parts.join(", ")
        },
    }
}

fn count_status(records: &[ExecutionRecord], status: ExecutionStatus) -> usize {
    records
        .iter()
        .filter(|record| record.status == status)
        .count()
}

fn record_body(record: &ExecutionRecord) -> String {
    let source = file_label(&record.source);
    let detail = match (&record.destination, &record.reason) {
        (Some(destination), _) => format!("{source} → {}", destination.display()),
        (None, Some(reason)) => format!("{source}: {reason}"),
        (None, None) => source,
    };

    match record.status {
        ExecutionStatus::Moved | ExecutionStatus::Deduped => {
            format!("{detail}\nUndo: alder undo last")
        }
        ExecutionStatus::Trashed => format!("{detail}\nUndo: see action log for trash action_id"),
        ExecutionStatus::Scanned if !record.supporting_files.is_empty() => {
            format!(
                "{detail}\n{} candidate item(s)",
                record.supporting_files.len()
            )
        }
        ExecutionStatus::Scanned => detail,
        ExecutionStatus::Failed | ExecutionStatus::Skipped => {
            let reason = record.reason.as_deref().unwrap_or("no reason reported");
            format!("{detail}\nReason: {reason}")
        }
        ExecutionStatus::Planned | ExecutionStatus::InProgress | ExecutionStatus::Undone => detail,
    }
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn status_label(status: &ExecutionStatus) -> &'static str {
    match status {
        ExecutionStatus::Planned => "planned",
        ExecutionStatus::InProgress => "in progress",
        ExecutionStatus::Moved => "moved",
        ExecutionStatus::Skipped => "skipped",
        ExecutionStatus::Failed => "failed",
        ExecutionStatus::Deduped => "deduped",
        ExecutionStatus::Trashed => "trashed",
        ExecutionStatus::Scanned => "scanned",
        ExecutionStatus::Undone => "undone",
    }
}

fn action_label(action: ActionKind) -> &'static str {
    match action {
        ActionKind::Move => "move",
        ActionKind::Trash => "trash",
        ActionKind::ScanAppSupportingFiles => "scan app supporting files",
        ActionKind::UndoMove => "undo move",
        ActionKind::UndoTrash => "undo trash",
    }
}

fn deliver(notification: &Notification) {
    #[cfg(target_os = "macos")]
    {
        let script = r#"
on run argv
  display notification (item 3 of argv) with title (item 1 of argv) subtitle (item 2 of argv)
end run
"#;
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .arg(&notification.title)
            .arg(&notification.subtitle)
            .arg(&notification.body)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = notification;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn formats_move_notification_with_undo_hint() {
        let record = ExecutionRecord {
            action: ActionKind::Move,
            source: PathBuf::from("/tmp/statement.pdf"),
            destination: Some(PathBuf::from("/docs/statement.pdf")),
            status: ExecutionStatus::Moved,
            reason: None,
            rule_id: "pnc".to_string(),
            sha256: None,
            size: Some(10),
            supporting_files: Vec::new(),
        };

        let notification = record_notification(&record);

        assert_eq!(notification.title, "Alder: moved");
        assert_eq!(notification.subtitle, "move by pnc");
        assert!(
            notification
                .body
                .contains("statement.pdf → /docs/statement.pdf")
        );
        assert!(notification.body.contains("Undo: alder undo last"));
    }

    #[test]
    fn summarizes_large_batches() {
        let records = (0..6)
            .map(|index| ExecutionRecord {
                action: ActionKind::Move,
                source: PathBuf::from(format!("/tmp/{index}.pdf")),
                destination: Some(PathBuf::from(format!("/docs/{index}.pdf"))),
                status: if index == 5 {
                    ExecutionStatus::Trashed
                } else {
                    ExecutionStatus::Moved
                },
                reason: None,
                rule_id: "rule".to_string(),
                sha256: None,
                size: None,
                supporting_files: Vec::new(),
            })
            .collect::<Vec<_>>();

        let notification = summary_notification(&records);

        assert_eq!(notification.title, "Alder: batch complete");
        assert!(notification.body.contains("5 moved"));
        assert!(notification.body.contains("1 trashed"));
    }
}
