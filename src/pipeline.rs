use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use serde::Serialize;

use crate::config::Config;
use crate::execute::{ExecuteOptions, ExecutionReport, execute_plan};
use crate::expr::Value;
use crate::facts::file::FileFacts;
use crate::facts::pdf::PdfTextProvider;
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
    pub explanation: Option<Explanation>,
    pub execution: Option<ExecutionReport>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FactsOutput {
    pub source: PathBuf,
    pub facts: IndexMap<String, Value>,
    pub provider_errors: Vec<String>,
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
    match plan_for_file(config, &source, &facts_output.facts) {
        Ok(explanation) => PipelineResult {
            source,
            provider_errors: facts_output.provider_errors,
            explanation: Some(explanation),
            execution: None,
            error: None,
        },
        Err(error) => PipelineResult {
            source,
            provider_errors: facts_output.provider_errors,
            explanation: None,
            execution: None,
            error: Some(error.to_string()),
        },
    }
}

pub fn facts_for_file(config: &Config, path: impl AsRef<Path>) -> FactsOutput {
    let source = path.as_ref().to_path_buf();
    let mut facts = IndexMap::new();
    let mut provider_errors = Vec::new();

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

            if needs_pdf_text(config) && file.ext().eq_ignore_ascii_case(".pdf") {
                match PdfTextProvider::default().text(file.path()) {
                    Ok(text) => {
                        facts.insert("pdf.text".to_string(), Value::String(text));
                    }
                    Err(error) => provider_errors.push(format!("pdf.text: {error}")),
                }
            }
        }
        Err(error) => provider_errors.push(error.to_string()),
    }

    FactsOutput {
        source,
        facts,
        provider_errors,
    }
}

pub fn destination_roots(config: &Config) -> Result<Vec<PathBuf>, String> {
    let roots = config
        .defaults
        .as_ref()
        .map(|defaults| defaults.destination_roots.as_slice())
        .unwrap_or(&[]);

    roots.iter().map(|root| expand_config_path(root)).collect()
}

fn process_file(config: &Config, source: PathBuf, options: &ProcessOptions) -> PipelineResult {
    let facts_output = facts_for_file(config, &source);
    let explanation = match plan_for_file(config, &source, &facts_output.facts) {
        Ok(explanation) => explanation,
        Err(error) => {
            return PipelineResult {
                source,
                provider_errors: facts_output.provider_errors,
                explanation: None,
                execution: None,
                error: Some(error.to_string()),
            };
        }
    };

    let execution = if let Some(plan) = explanation.plan.as_ref() {
        let execute_options = ExecuteOptions {
            dry_run: options.dry_run,
            destination_roots: options.destination_roots.clone(),
            action_log_path: options.action_log_path.clone(),
            run_id: options.run_id.clone(),
        };
        match execute_plan(plan, &execute_options) {
            Ok(report) => Some(report),
            Err(error) => {
                return PipelineResult {
                    source,
                    provider_errors: facts_output.provider_errors,
                    explanation: Some(explanation),
                    execution: None,
                    error: Some(error.to_string()),
                };
            }
        }
    } else {
        None
    };

    PipelineResult {
        source,
        provider_errors: facts_output.provider_errors,
        explanation: Some(explanation),
        execution,
        error: None,
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

fn needs_pdf_text(config: &Config) -> bool {
    config.rules.iter().any(|rule| {
        rule.when.contains("pdf.text")
            || rule
                .extract
                .values()
                .any(|extractor| extractor.from == "pdf.text")
    })
}

fn system_time_string(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().to_string(),
        Err(_) => "0".to_string(),
    }
}

fn expand_config_path(path: &str) -> Result<PathBuf, String> {
    let expanded = if path == "~" || path.starts_with("~/") {
        let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not available".to_string())?;
        if path == "~" {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(&path[2..])
        }
    } else if path.starts_with('~') {
        return Err(format!("unsupported ~user path {path:?}"));
    } else {
        PathBuf::from(path)
    };

    if expanded
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!(
            "destination root must not contain ..: {}",
            expanded.display()
        ));
    }

    Ok(if expanded.is_absolute() {
        expanded
    } else {
        std::path::absolute(&expanded)
            .map_err(|error| format!("failed to resolve {}: {error}", expanded.display()))?
    })
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
