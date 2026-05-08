use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use serde::Serialize;

use crate::config::{Config, Rule};
use crate::execute::{ExecuteOptions, ExecutionReport, execute_plan};
use crate::expr::{self, Value};
use crate::facts::file::FileFacts;
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

impl FactProvider {
    fn domain(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Pdf => "pdf",
            Self::Spotlight => "spotlight",
        }
    }
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
enum Applicability {
    Required,
    NotRequired,
    Skipped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredFacts {
    pub keys: BTreeSet<String>,
}

mod fact_providers {
    use indexmap::IndexMap;

    use crate::expr::Value;
    use crate::facts::file::FileFacts;
    use crate::facts::pdf::PdfTextProvider;
    use crate::facts::spotlight::SpotlightProvider;

    use super::{Applicability, RequiredFacts, spotlight_value_string, system_time_string};

    pub(super) trait FactProvider {
        fn name(&self) -> super::FactProvider;
        fn applies(&self, file: &FileFacts, required: &RequiredFacts) -> Applicability;
        fn collect(&self, file: &FileFacts) -> Result<IndexMap<String, Value>, String>;

        fn report_facts(&self, required: &RequiredFacts) -> Vec<String> {
            required.provider_keys(self.name().domain())
        }
    }

    pub(super) struct FileFactProvider;

    impl FactProvider for FileFactProvider {
        fn name(&self) -> super::FactProvider {
            super::FactProvider::File
        }

        fn applies(&self, _file: &FileFacts, _required: &RequiredFacts) -> Applicability {
            Applicability::Required
        }

        fn collect(&self, file: &FileFacts) -> Result<IndexMap<String, Value>, String> {
            let mut facts = IndexMap::new();
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

            Ok(facts)
        }
    }

    #[derive(Default)]
    pub(super) struct PdfFactProvider {
        provider: PdfTextProvider,
    }

    impl FactProvider for PdfFactProvider {
        fn name(&self) -> super::FactProvider {
            super::FactProvider::Pdf
        }

        fn applies(&self, file: &FileFacts, required: &RequiredFacts) -> Applicability {
            if !required.needs_provider("pdf") {
                Applicability::NotRequired
            } else if file.ext().eq_ignore_ascii_case(".pdf") {
                Applicability::Required
            } else {
                Applicability::Skipped("source is not a PDF".to_string())
            }
        }

        fn collect(&self, file: &FileFacts) -> Result<IndexMap<String, Value>, String> {
            let text = self
                .provider
                .text(file.path())
                .map_err(|error| format!("pdf.text: {error}"))?;

            let mut facts = IndexMap::new();
            facts.insert("pdf.text".to_string(), Value::String(text));
            Ok(facts)
        }

        fn report_facts(&self, _required: &RequiredFacts) -> Vec<String> {
            // The PDF provider currently claims exactly one fact, even if future-looking
            // configs reference other pdf.* keys that no provider can produce yet.
            vec!["pdf.text".to_string()]
        }
    }

    #[derive(Default)]
    pub(super) struct SpotlightFactProvider {
        provider: SpotlightProvider,
    }

    impl FactProvider for SpotlightFactProvider {
        fn name(&self) -> super::FactProvider {
            super::FactProvider::Spotlight
        }

        fn applies(&self, _file: &FileFacts, required: &RequiredFacts) -> Applicability {
            if required.needs_provider("spotlight") {
                Applicability::Required
            } else {
                Applicability::NotRequired
            }
        }

        fn collect(&self, file: &FileFacts) -> Result<IndexMap<String, Value>, String> {
            let spotlight_facts = self
                .provider
                .facts(file.path())
                .map_err(|error| format!("spotlight: {error}"))?;

            Ok(spotlight_facts
                .into_iter()
                .map(|(key, value)| (key, Value::String(spotlight_value_string(&value))))
                .collect())
        }
    }
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
    let facts_output = planning_facts_for_file(config, &source);
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
    let required = required_facts(config);
    facts_for_file_with_required(path, &required)
}

fn planning_facts_for_file(config: &Config, path: impl AsRef<Path>) -> FactsOutput {
    let source = path.as_ref().to_path_buf();
    let predicate_required = predicate_required_facts(config);
    let mut output = facts_for_file_with_required(&source, &predicate_required);

    if let Some(rule) = first_matching_rule(config, &output.facts) {
        let missing = missing_extractor_facts(rule, &output.facts);
        if !missing.keys.is_empty() {
            let extra = facts_for_file_with_required(&source, &missing);
            merge_facts_output(&mut output, extra);
        }
    }

    output
}

fn facts_for_file_with_required(path: impl AsRef<Path>, required: &RequiredFacts) -> FactsOutput {
    let source = path.as_ref().to_path_buf();
    let mut facts = IndexMap::new();
    let mut provider_errors = Vec::new();
    let mut provider_reports = Vec::new();

    match FileFacts::from_path(&source) {
        Ok(file) => {
            let file_provider = fact_providers::FileFactProvider;
            let pdf_provider = fact_providers::PdfFactProvider::default();
            let spotlight_provider = fact_providers::SpotlightFactProvider::default();
            let providers: [&dyn fact_providers::FactProvider; 3] =
                [&file_provider, &pdf_provider, &spotlight_provider];

            for provider in providers {
                let provider_name = provider.name();
                match provider.applies(&file, required) {
                    Applicability::Required => match provider.collect(&file) {
                        Ok(produced) => {
                            let produced_keys = produced.keys().cloned().collect();
                            facts.extend(produced);
                            provider_reports.push(ProviderReport {
                                provider: provider_name,
                                status: ProviderStatus::Invoked,
                                facts: produced_keys,
                                message: None,
                            });
                        }
                        Err(message) => {
                            provider_errors.push(message.clone());
                            provider_reports.push(ProviderReport {
                                provider: provider_name,
                                status: ProviderStatus::Error,
                                facts: provider.report_facts(required),
                                message: Some(message),
                            });
                        }
                    },
                    Applicability::NotRequired => provider_reports.push(ProviderReport {
                        provider: provider_name,
                        status: ProviderStatus::NotRequired,
                        facts: Vec::new(),
                        message: None,
                    }),
                    Applicability::Skipped(message) => provider_reports.push(ProviderReport {
                        provider: provider_name,
                        status: ProviderStatus::Skipped,
                        facts: provider.report_facts(required),
                        message: Some(message),
                    }),
                }
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

fn first_matching_rule<'a>(
    config: &'a Config,
    facts: &IndexMap<String, Value>,
) -> Option<&'a Rule> {
    config
        .rules
        .iter()
        .find(|rule| matches!(expr::eval_bool(&rule.when, facts), Ok(true)))
}

fn missing_extractor_facts(rule: &Rule, facts: &IndexMap<String, Value>) -> RequiredFacts {
    RequiredFacts {
        keys: rule
            .extract
            .values()
            .map(|extractor| extractor.from().to_string())
            .filter(|key| !facts.contains_key(key))
            .collect(),
    }
}

fn merge_facts_output(output: &mut FactsOutput, extra: FactsOutput) {
    output.facts.extend(extra.facts);
    output.provider_errors.extend(extra.provider_errors);
    for report in extra.provider_reports {
        if report.provider == FactProvider::File || report.status == ProviderStatus::NotRequired {
            continue;
        }
        upsert_provider_report(&mut output.provider_reports, report);
    }
}

fn upsert_provider_report(reports: &mut Vec<ProviderReport>, report: ProviderReport) {
    if let Some(existing) = reports
        .iter_mut()
        .find(|existing| existing.provider == report.provider)
    {
        *existing = report;
    } else {
        reports.push(report);
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
    let facts_output = planning_facts_for_file(config, &source);
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
    let mut required = predicate_required_facts(config);

    for rule in &config.rules {
        for extractor in rule.extract.values() {
            required.keys.insert(extractor.from().to_string());
        }
    }

    required
}

fn predicate_required_facts(config: &Config) -> RequiredFacts {
    let mut keys = BTreeSet::new();

    for rule in &config.rules {
        if let Ok(identifiers) = expr::identifiers(&rule.when) {
            keys.extend(identifiers);
        }
    }

    RequiredFacts { keys }
}

impl RequiredFacts {
    fn needs_provider(&self, provider: &str) -> bool {
        let prefix = format!("{provider}.");
        self.keys.iter().any(|key| key.starts_with(&prefix))
    }

    fn provider_keys(&self, provider: &str) -> Vec<String> {
        let prefix = format!("{provider}.");
        self.keys
            .iter()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect()
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

    #[test]
    fn planning_skips_pdf_extractor_for_nonmatching_rule() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("statement.pdf");
        fs::write(&file, b"fake").unwrap();
        let config = parse_config_str(&format!(
            r#"
version: 1
rules:
  - id: txt_only
    when: file.ext == ".txt"
    extract:
      title:
        from: pdf.text
        regex: "(.*)"
    actions:
      - move:
          to: "{}/{{{{ title }}}}.pdf"
"#,
            temp.path().display()
        ))
        .unwrap();

        let result = explain_file(&config, &file);
        let pdf_report = result
            .provider_reports
            .iter()
            .find(|report| report.provider == FactProvider::Pdf)
            .unwrap();

        assert_eq!(pdf_report.status, ProviderStatus::NotRequired);
        assert!(result.error.is_none(), "{:?}", result.error);
        assert!(
            result
                .explanation
                .as_ref()
                .is_some_and(|explanation| explanation.plan.is_none())
        );
    }

    #[test]
    fn planning_reports_pdf_extractor_after_rule_matches() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("notes.txt");
        fs::write(&file, b"not a pdf").unwrap();
        let config = parse_config_str(&format!(
            r#"
version: 1
rules:
  - id: txt_with_pdf_extractor
    when: file.ext == ".txt"
    extract:
      title:
        from: pdf.text
        regex: "(.*)"
    actions:
      - move:
          to: "{}/{{{{ title }}}}.pdf"
"#,
            temp.path().display()
        ))
        .unwrap();

        let result = explain_file(&config, &file);
        let pdf_report = result
            .provider_reports
            .iter()
            .find(|report| report.provider == FactProvider::Pdf)
            .unwrap();

        assert_eq!(pdf_report.status, ProviderStatus::Skipped);
        assert_eq!(pdf_report.message.as_deref(), Some("source is not a PDF"));
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("missing fact"))
        );
    }

    #[test]
    fn provider_reports_mark_required_pdf_skipped_for_non_pdf() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("notes.txt");
        fs::write(&file, b"not a pdf").unwrap();
        let config = parse_config_str(&format!(
            r#"
version: 1
defaults:
  destination_roots:
    - "{}"
rules:
  - id: pdf_text
    when: contains(pdf.text, "invoice")
    actions:
      - move:
          to: "{}/{{{{ file.name }}}}"
"#,
            temp.path().display(),
            temp.path().display()
        ))
        .unwrap();

        let facts = facts_for_file(&config, &file);
        let pdf_report = facts
            .provider_reports
            .iter()
            .find(|report| report.provider == FactProvider::Pdf)
            .unwrap();

        assert_eq!(pdf_report.status, ProviderStatus::Skipped);
        assert_eq!(pdf_report.facts, vec!["pdf.text"]);
        assert_eq!(pdf_report.message.as_deref(), Some("source is not a PDF"));
    }

    #[test]
    fn file_fact_errors_do_not_report_other_providers() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());

        let facts = facts_for_file(&config, temp.path());

        assert_eq!(facts.provider_reports.len(), 1);
        assert_eq!(facts.provider_reports[0].provider, FactProvider::File);
        assert_eq!(facts.provider_reports[0].status, ProviderStatus::Error);
        assert!(!facts.provider_errors.is_empty());
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
