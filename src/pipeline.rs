use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use serde::Serialize;

use crate::config::Config;
use crate::execute::{ExecuteOptions, ExecutionReport, execute_plan};
use crate::expr::{self, Value};
use crate::facts::file::FileFacts;
use crate::facts::pdf::PdfTextProvider;
use crate::facts::spotlight::SpotlightProvider;
use crate::path_utils::{PathError, expand_user_path};
use crate::planning::{Explanation, plan_for_file};

#[derive(Debug, Clone, Serialize)]
pub struct ProcessOptions {
    pub dry_run: bool,
    pub destination_roots: Vec<PathBuf>,
    pub action_log_path: PathBuf,
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineResult {
    pub source: PathBuf,
    pub provider_errors: Vec<String>,
    pub provider_reports: Vec<ProviderReport>,
    pub explanation: Option<Explanation>,
    pub execution: Option<ExecutionReport>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FactsOutput {
    pub source: PathBuf,
    pub facts: IndexMap<String, Value>,
    pub provider_errors: Vec<String>,
    pub provider_reports: Vec<ProviderReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderReport {
    pub provider: FactProvider,
    pub status: ProviderStatus,
    pub facts: Vec<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactProvider {
    File,
    Pdf,
    Spotlight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    NotRequired,
    Skipped,
    Invoked,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredFacts {
    pub keys: BTreeSet<String>,
}

pub fn process_paths(
    config: &Config,
    paths: &[PathBuf],
    options: &ProcessOptions,
) -> Vec<PipelineResult> {
    collect_input_paths(paths)
        .into_iter()
        .map(|path| process_file(config, path, options))
        .collect()
}

pub fn explain_file(config: &Config, path: impl AsRef<Path>) -> PipelineResult {
    let source = path.as_ref().to_path_buf();
    let facts_output = facts_for_file(config, &source);
    let (explanation, error) = match plan_for_file(config, &source, &facts_output.facts) {
        Ok(explanation) => (Some(explanation), None),
        Err(error) => (None, Some(error.to_string())),
    };

    PipelineResult {
        source,
        provider_errors: facts_output.provider_errors,
        provider_reports: facts_output.provider_reports,
        explanation,
        execution: None,
        error,
    }
}

pub fn facts_for_file(config: &Config, path: impl AsRef<Path>) -> FactsOutput {
    let source = path.as_ref().to_path_buf();
    let mut facts = IndexMap::new();
    let mut provider_errors = Vec::new();
    let mut provider_reports = Vec::new();
    let required = required_facts(config);

    match FileFacts::from_path(&source) {
        Ok(file) => {
            facts.insert(
                "file.path".to_string(),
                Value::String(file.path().display().to_string()),
            );
            facts.insert(
                "file.name".to_string(),
                Value::String(file.name().to_string()),
            );
            facts.insert(
                "file.stem".to_string(),
                Value::String(file.stem().to_string()),
            );
            facts.insert(
                "file.ext".to_string(),
                Value::String(file.ext().to_string()),
            );
            facts.insert(
                "file.size".to_string(),
                Value::String(file.size().to_string()),
            );
            if let Some(modified) = file.modified_at() {
                facts.insert(
                    "file.modified_at".to_string(),
                    Value::String(system_time_string(modified)),
                );
            }
            if let Some(created) = file.created_at() {
                facts.insert(
                    "file.created_at".to_string(),
                    Value::String(system_time_string(created)),
                );
            }
            provider_reports.push(ProviderReport {
                provider: FactProvider::File,
                status: ProviderStatus::Invoked,
                facts: facts
                    .keys()
                    .filter(|key| key.starts_with("file."))
                    .cloned()
                    .collect(),
                message: None,
            });

            if required.needs_provider("pdf") && file.ext().eq_ignore_ascii_case(".pdf") {
                match PdfTextProvider::default().text(file.path()) {
                    Ok(text) => {
                        facts.insert("pdf.text".to_string(), Value::String(text));
                        provider_reports.push(ProviderReport {
                            provider: FactProvider::Pdf,
                            status: ProviderStatus::Invoked,
                            facts: vec!["pdf.text".to_string()],
                            message: None,
                        });
                    }
                    Err(error) => {
                        let message = format!("pdf.text: {error}");
                        provider_errors.push(message.clone());
                        provider_reports.push(ProviderReport {
                            provider: FactProvider::Pdf,
                            status: ProviderStatus::Error,
                            facts: vec!["pdf.text".to_string()],
                            message: Some(message),
                        });
                    }
                }
            } else if required.needs_provider("pdf") {
                provider_reports.push(ProviderReport {
                    provider: FactProvider::Pdf,
                    status: ProviderStatus::Skipped,
                    facts: vec!["pdf.text".to_string()],
                    message: Some("source is not a PDF".to_string()),
                });
            } else {
                provider_reports.push(ProviderReport {
                    provider: FactProvider::Pdf,
                    status: ProviderStatus::NotRequired,
                    facts: Vec::new(),
                    message: None,
                });
            }

            if required.needs_provider("spotlight") {
                match SpotlightProvider::default().facts(file.path()) {
                    Ok(spotlight_facts) => {
                        let mut produced = Vec::new();
                        for (key, value) in spotlight_facts {
                            produced.push(key.clone());
                            facts.insert(key, Value::String(spotlight_value_string(&value)));
                        }
                        provider_reports.push(ProviderReport {
                            provider: FactProvider::Spotlight,
                            status: ProviderStatus::Invoked,
                            facts: produced,
                            message: None,
                        });
                    }
                    Err(error) => {
                        let message = format!("spotlight: {error}");
                        provider_errors.push(message.clone());
                        provider_reports.push(ProviderReport {
                            provider: FactProvider::Spotlight,
                            status: ProviderStatus::Error,
                            facts: required
                                .keys
                                .iter()
                                .filter(|key| key.starts_with("spotlight."))
                                .cloned()
                                .collect(),
                            message: Some(message),
                        });
                    }
                }
            } else {
                provider_reports.push(ProviderReport {
                    provider: FactProvider::Spotlight,
                    status: ProviderStatus::NotRequired,
                    facts: Vec::new(),
                    message: None,
                });
            }
        }
        Err(error) => {
            let message = error.to_string();
            provider_errors.push(message.clone());
            provider_reports.push(ProviderReport {
                provider: FactProvider::File,
                status: ProviderStatus::Error,
                facts: Vec::new(),
                message: Some(message),
            });
        }
    }

    FactsOutput {
        source,
        facts,
        provider_errors,
        provider_reports,
    }
}

pub fn destination_roots(config: &Config) -> Result<Vec<PathBuf>, String> {
    let roots = config
        .defaults
        .as_ref()
        .map(|defaults| defaults.destination_roots.as_slice())
        .unwrap_or(&[]);

    roots
        .iter()
        .map(|root| expand_user_path(root).map_err(destination_root_error))
        .collect()
}

fn process_file(config: &Config, source: PathBuf, options: &ProcessOptions) -> PipelineResult {
    let facts_output = facts_for_file(config, &source);
    let mut error = None;
    let explanation = match plan_for_file(config, &source, &facts_output.facts) {
        Ok(explanation) => Some(explanation),
        Err(plan_error) => {
            error = Some(plan_error.to_string());
            None
        }
    };

    let execution = if let Some(plan) = explanation
        .as_ref()
        .and_then(|explanation| explanation.plan.as_ref())
    {
        let execute_options = ExecuteOptions {
            dry_run: options.dry_run,
            destination_roots: options.destination_roots.clone(),
            action_log_path: options.action_log_path.clone(),
            run_id: options.run_id.clone(),
        };
        match execute_plan(plan, &execute_options) {
            Ok(report) => Some(report),
            Err(execute_error) => {
                error = Some(execute_error.to_string());
                None
            }
        }
    } else {
        None
    };

    PipelineResult {
        source,
        provider_errors: facts_output.provider_errors,
        provider_reports: facts_output.provider_reports,
        explanation,
        execution,
        error,
    }
}

fn collect_input_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        collect_path(path, &mut files);
    }
    files.sort();
    files.dedup();
    files
}

fn collect_path(path: &Path, files: &mut Vec<PathBuf>) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_file() {
        files.push(path.to_path_buf());
    } else if metadata.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            collect_path(&entry.path(), files);
        }
    }
}

pub fn required_facts(config: &Config) -> RequiredFacts {
    let mut keys = BTreeSet::new();

    for rule in &config.rules {
        if let Ok(identifiers) = expr::identifiers(&rule.when) {
            keys.extend(identifiers);
        }
        for extractor in rule.extract.values() {
            keys.insert(extractor.from.clone());
        }
    }

    RequiredFacts { keys }
}

impl RequiredFacts {
    fn needs_provider(&self, provider: &str) -> bool {
        let prefix = format!("{provider}.");
        self.keys.iter().any(|key| key.starts_with(&prefix))
    }
}

fn spotlight_value_string(value: &plist::Value) -> String {
    match value {
        plist::Value::String(value) => value.clone(),
        plist::Value::Boolean(value) => value.to_string(),
        plist::Value::Integer(value) => value.to_string(),
        plist::Value::Real(value) => value.to_string(),
        plist::Value::Array(values) => values
            .iter()
            .map(spotlight_value_string)
            .collect::<Vec<_>>()
            .join("\n"),
        other => format!("{other:?}"),
    }
}

fn system_time_string(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().to_string(),
        Err(_) => "0".to_string(),
    }
}

fn destination_root_error(error: PathError) -> String {
    match error {
        PathError::HomeUnavailable => "HOME is not available".to_string(),
        PathError::UnsupportedTilde(path) => format!("unsupported ~user path {path:?}"),
        PathError::ParentDir(path) => {
            format!("destination root must not contain ..: {}", path.display())
        }
        PathError::Io { path, source } => format!("failed to resolve {}: {source}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_config_str;

    #[test]
    fn dry_run_plans_without_moving() {
        let temp = tempfile::tempdir().unwrap();
        let inbox = temp.path().join("inbox");
        let sorted = temp.path().join("sorted");
        fs::create_dir_all(&inbox).unwrap();
        fs::create_dir_all(&sorted).unwrap();
        let source = inbox.join("statement.pdf");
        fs::write(&source, b"fake").unwrap();
        let config = config(&sorted);

        let results = process_paths(
            &config,
            std::slice::from_ref(&inbox),
            &options(true, &sorted),
        );

        assert_eq!(results.len(), 1);
        assert!(results[0].explanation.as_ref().unwrap().plan.is_some());
        assert!(source.exists());
        assert!(!sorted.join("statement.pdf").exists());
    }

    #[test]
    fn execute_moves_and_logs() {
        let temp = tempfile::tempdir().unwrap();
        let inbox = temp.path().join("inbox");
        let sorted = temp.path().join("sorted");
        fs::create_dir_all(&inbox).unwrap();
        fs::create_dir_all(&sorted).unwrap();
        let source = inbox.join("statement.pdf");
        fs::write(&source, b"fake").unwrap();
        let config = config(&sorted);

        let results = process_paths(
            &config,
            std::slice::from_ref(&inbox),
            &options(false, &sorted),
        );

        assert_eq!(results.len(), 1);
        assert!(results[0].error.is_none(), "{:?}", results[0].error);
        assert!(!source.exists());
        assert_eq!(fs::read(sorted.join("statement.pdf")).unwrap(), b"fake");
        assert!(
            fs::read_to_string(temp.path().join("actions.jsonl"))
                .unwrap()
                .contains("moved")
        );
    }

    #[test]
    fn required_facts_include_expression_and_extractors() {
        let config = parse_config_str(
            r#"
version: 1
rules:
  - id: amex
    when: file.ext == ".pdf" && contains(pdf.text, "American Express")
    extract:
      author:
        from: spotlight.kMDItemAuthors
        regex: "(.*)"
    actions:
      - move:
          to: "~/Documents/{{ file.name }}"
"#,
        )
        .unwrap();

        let required = required_facts(&config);

        assert!(required.keys.contains("file.ext"));
        assert!(required.keys.contains("pdf.text"));
        assert!(required.keys.contains("spotlight.kMDItemAuthors"));
    }

    #[test]
    fn provider_reports_mark_pdf_not_required() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("statement.pdf");
        fs::write(&file, b"fake").unwrap();
        let config = config(temp.path());

        let facts = facts_for_file(&config, &file);

        assert!(facts.provider_reports.iter().any(|report| {
            report.provider == FactProvider::Pdf && report.status == ProviderStatus::NotRequired
        }));
    }

    fn config(sorted: &Path) -> Config {
        parse_config_str(&format!(
            r#"
version: 1
defaults:
  conflict: append_counter
  destination_roots:
    - "{}"
rules:
  - id: pdfs
    when: file.ext == ".pdf"
    actions:
      - move:
          to: "{}/{{{{ file.name }}}}"
"#,
            sorted.display(),
            sorted.display()
        ))
        .unwrap()
    }

    fn options(dry_run: bool, root: &Path) -> ProcessOptions {
        ProcessOptions {
            dry_run,
            destination_roots: vec![root.to_path_buf()],
            action_log_path: root.parent().unwrap().join("actions.jsonl"),
            run_id: "test".to_string(),
        }
    }
}
