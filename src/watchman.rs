use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::ser::SerializeTuple;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};
use thiserror::Error;

use crate::config::{Config, WatchConfig};
use crate::path_utils::{PathError, expand_user_path};

const DEFAULT_TRIGGER_NAME: &str = "alder";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchmanGenerateOptions {
    pub config_path: PathBuf,
    pub alder_exe: PathBuf,
    pub trigger_name: String,
}

impl WatchmanGenerateOptions {
    pub fn new(config_path: PathBuf, alder_exe: PathBuf) -> Self {
        Self {
            config_path,
            alder_exe,
            trigger_name: DEFAULT_TRIGGER_NAME.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TriggerCommand {
    command: String,
    root: PathBuf,
    definition: TriggerDefinition,
}

impl TriggerCommand {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn definition(&self) -> &TriggerDefinition {
        &self.definition
    }
}

impl Serialize for TriggerCommand {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut tuple = serializer.serialize_tuple(3)?;
        tuple.serialize_element(&self.command)?;
        tuple.serialize_element(&self.root)?;
        tuple.serialize_element(&self.definition)?;
        tuple.end()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TriggerDefinition {
    pub name: String,
    pub expression: Value,
    pub command: Vec<String>,
    pub append_files: bool,
    pub stdin: Vec<String>,
}

pub fn generate_trigger_commands(
    config: &Config,
    options: &WatchmanGenerateOptions,
) -> Result<Vec<TriggerCommand>, WatchmanError> {
    let watch = config
        .watch
        .as_ref()
        .ok_or(WatchmanError::MissingWatchConfig)?;
    if watch.paths.is_empty() {
        return Err(WatchmanError::MissingWatchPaths);
    }

    let mut roots = watch
        .paths
        .iter()
        .map(|path| expand_config_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    roots.sort();

    let patterns = WatchPatterns::new(watch)?;
    let mut commands = Vec::new();
    for root in roots {
        commands.push(TriggerCommand {
            command: "trigger".to_string(),
            root,
            definition: TriggerDefinition {
                name: options.trigger_name.clone(),
                expression: patterns.expression(),
                command: vec![
                    options.alder_exe.display().to_string(),
                    "ingest".to_string(),
                    "--from-watchman".to_string(),
                    "--config".to_string(),
                    options.config_path.display().to_string(),
                ],
                append_files: false,
                stdin: vec!["name".to_string(), "exists".to_string(), "type".to_string()],
            },
        });
    }

    Ok(commands)
}

pub fn parse_watchman_stdin(
    input: &str,
    watchman_root: impl AsRef<Path>,
    watch: &WatchConfig,
) -> Result<Vec<PathBuf>, WatchmanError> {
    let entries: Vec<WatchmanInputEntry> = serde_json::from_str(input)?;
    let patterns = WatchPatterns::new(watch)?;
    let root = watchman_root.as_ref();
    let mut candidates = Vec::new();

    for entry in entries {
        if !entry.exists().unwrap_or(true) || entry.kind() != Some("f") {
            continue;
        }

        let rel = safe_relative_watchman_name(entry.name())?;
        if !patterns.is_match(&rel) {
            continue;
        }
        candidates.push(root.join(rel));
    }

    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

#[derive(Debug, Clone)]
struct WatchPatterns {
    include: Vec<String>,
    ignore: Vec<String>,
    include_set: Option<GlobSet>,
    ignore_set: GlobSet,
}

impl WatchPatterns {
    fn new(watch: &WatchConfig) -> Result<Self, WatchmanError> {
        let include = sorted_patterns(&watch.include);
        let ignore = sorted_patterns(&watch.ignore);
        let include_set = if include.is_empty() {
            None
        } else {
            Some(globset(&include)?)
        };
        let ignore_set = globset(&ignore)?;

        Ok(Self {
            include,
            ignore,
            include_set,
            ignore_set,
        })
    }

    fn expression(&self) -> Value {
        let mut clauses = vec![json!(["type", "f"])];

        if !self.include.is_empty() {
            clauses.push(pattern_expression(&self.include));
        }
        if !self.ignore.is_empty() {
            clauses.push(json!(["not", pattern_expression(&self.ignore)]));
        }

        variadic_expression("allof", clauses)
    }

    fn is_match(&self, rel: &Path) -> bool {
        if self.ignore_set.is_match(rel) {
            return false;
        }
        match &self.include_set {
            Some(include_set) => include_set.is_match(rel),
            None => true,
        }
    }
}

fn pattern_expression(patterns: &[String]) -> Value {
    let expressions = patterns
        .iter()
        .map(|pattern| pattern_to_expression(pattern))
        .collect();
    variadic_expression("anyof", expressions)
}

fn pattern_to_expression(pattern: &str) -> Value {
    if let Some(suffix) = simple_suffix(pattern) {
        json!(["suffix", suffix])
    } else {
        json!(["match", pattern, "wholename"])
    }
}

fn variadic_expression(op: &str, expressions: Vec<Value>) -> Value {
    let mut values = Vec::with_capacity(expressions.len() + 1);
    values.push(Value::String(op.to_string()));
    values.extend(expressions);
    Value::Array(values)
}

fn simple_suffix(pattern: &str) -> Option<String> {
    let suffix = pattern.strip_prefix("*.")?;
    if suffix.is_empty()
        || suffix.contains('.')
        || suffix
            .chars()
            .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}' | '/'))
    {
        return None;
    }
    Some(suffix.to_string())
}

fn globset(patterns: &[String]) -> Result<GlobSet, WatchmanError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|error| WatchmanError::Pattern {
            pattern: pattern.clone(),
            message: error.to_string(),
        })?);
    }
    builder.build().map_err(|error| WatchmanError::Pattern {
        pattern: "<combined>".to_string(),
        message: error.to_string(),
    })
}

fn sorted_patterns(patterns: &[String]) -> Vec<String> {
    let mut patterns = patterns.to_vec();
    patterns.sort();
    patterns
}

fn safe_relative_watchman_name(name: &str) -> Result<PathBuf, WatchmanError> {
    if name.contains('\0') {
        return Err(WatchmanError::UnsafeWatchmanName(name.to_string()));
    }

    let path = Path::new(name);
    if path.is_absolute() {
        return Err(WatchmanError::UnsafeWatchmanName(name.to_string()));
    }

    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WatchmanError::UnsafeWatchmanName(name.to_string()));
            }
        }
    }
    Ok(safe)
}

fn expand_config_path(path: &str) -> Result<PathBuf, WatchmanError> {
    expand_user_path(path).map_err(watch_path_error)
}

fn watch_path_error(error: PathError) -> WatchmanError {
    match error {
        PathError::HomeUnavailable => WatchmanError::HomeUnavailable,
        PathError::UnsupportedTilde(path) => WatchmanError::UnsupportedTilde(path),
        PathError::ParentDir(path) => WatchmanError::UnsafeWatchPath(path),
        PathError::Io { path, source } => {
            WatchmanError::io("make watch path absolute", path, source)
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WatchmanInputEntry {
    Name(String),
    Object {
        name: String,
        #[serde(default)]
        exists: Option<bool>,
        #[serde(default, rename = "type")]
        kind: Option<String>,
    },
}

impl WatchmanInputEntry {
    fn name(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Object { name, .. } => name,
        }
    }

    fn exists(&self) -> Option<bool> {
        match self {
            Self::Name(_) => Some(true),
            Self::Object { exists, .. } => *exists,
        }
    }

    fn kind(&self) -> Option<&str> {
        match self {
            Self::Name(_) => Some("f"),
            Self::Object { kind, .. } => kind.as_deref(),
        }
    }
}

pub fn watchman_sync(
    config: &Config,
    options: &WatchmanGenerateOptions,
) -> Result<(), WatchmanError> {
    for command in generate_trigger_commands(config, options)? {
        run_watchman_json(&[
            Value::String("watch".to_string()),
            path_value(command.root()),
        ])?;
        run_watchman_json(&serde_json::to_value(&command).map_err(WatchmanError::JsonShape)?)?;
    }
    Ok(())
}

pub fn watchman_unsync(
    config: &Config,
    options: &WatchmanGenerateOptions,
) -> Result<(), WatchmanError> {
    for command in generate_trigger_commands(config, options)? {
        run_watchman_json(&[
            Value::String("trigger-del".to_string()),
            path_value(command.root()),
            Value::String(command.definition().name.clone()),
        ])?;
    }
    Ok(())
}

pub fn watchman_check(
    config: &Config,
    options: &WatchmanGenerateOptions,
) -> Result<(), WatchmanError> {
    for command in generate_trigger_commands(config, options)? {
        let output = run_watchman_json(&[
            Value::String("trigger-list".to_string()),
            path_value(command.root()),
        ])?;
        let expected =
            serde_json::to_value(command.definition()).map_err(WatchmanError::JsonShape)?;
        let triggers = output
            .get("triggers")
            .and_then(Value::as_array)
            .ok_or_else(|| WatchmanError::UnexpectedResponse(output.clone()))?;
        let actual = triggers
            .iter()
            .find(|trigger| {
                trigger.get("name") == Some(&Value::String(command.definition().name.clone()))
            })
            .ok_or_else(|| WatchmanError::TriggerDrift {
                root: command.root().to_path_buf(),
                message: "trigger is missing".to_string(),
            })?;
        if actual != &expected {
            return Err(WatchmanError::TriggerDrift {
                root: command.root().to_path_buf(),
                message: "trigger differs from generated definition".to_string(),
            });
        }
    }
    Ok(())
}

fn run_watchman_json(command: impl Serialize) -> Result<Value, WatchmanError> {
    let mut child = Command::new("watchman")
        .arg("-j")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| WatchmanError::io("spawn watchman", "watchman", error))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| WatchmanError::UnexpectedIo("watchman stdin unavailable".to_string()))?;
        serde_json::to_writer(&mut stdin, &command).map_err(WatchmanError::JsonShape)?;
        stdin
            .write_all(b"\n")
            .map_err(|error| WatchmanError::io("write watchman stdin", "watchman", error))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| WatchmanError::io("wait for watchman", "watchman", error))?;
    if !output.status.success() {
        return Err(WatchmanError::CommandFailed {
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(WatchmanError::Watchman(error.to_string()));
    }
    Ok(value)
}

fn path_value(path: &Path) -> Value {
    Value::String(path.display().to_string())
}

#[derive(Debug, Error)]
pub enum WatchmanError {
    #[error("config does not define watch settings")]
    MissingWatchConfig,
    #[error("config watch.paths must not be empty")]
    MissingWatchPaths,
    #[error("HOME is not available for ~/ expansion")]
    HomeUnavailable,
    #[error("unsupported ~user path {0:?}")]
    UnsupportedTilde(String),
    #[error("watch path must not contain ..: {}", .0.display())]
    UnsafeWatchPath(PathBuf),
    #[error("invalid watch pattern {pattern:?}: {message}")]
    Pattern { pattern: String, message: String },
    #[error("unsafe Watchman path name {0:?}")]
    UnsafeWatchmanName(String),
    #[error("watchman error: {0}")]
    Watchman(String),
    #[error("Watchman trigger drift for {}: {message}", root.display())]
    TriggerDrift { root: PathBuf, message: String },
    #[error("unexpected Watchman response: {0}")]
    UnexpectedResponse(Value),
    #[error("unexpected IO state: {0}")]
    UnexpectedIo(String),
    #[error("watchman exited with {status:?}: {}", stderr.trim())]
    CommandFailed { status: Option<i32>, stderr: String },
    #[error("failed to {op} for {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("JSON error: {0}")]
    JsonShape(#[source] serde_json::Error),
}

impl WatchmanError {
    fn io(op: &'static str, path: impl AsRef<OsStr>, source: io::Error) -> Self {
        Self::Io {
            op,
            path: PathBuf::from(path.as_ref()),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_config_str;

    #[test]
    fn generates_deterministic_trigger_json_from_config() {
        let config = parse_config_str(
            r#"
version: 1
watch:
  paths:
    - tmp/inbox
  include:
    - "*.pdf"
  ignore:
    - "*.tmp"
    - "*.download"
rules:
  - id: pdfs
    when: file.ext == ".pdf"
    actions:
      - move:
          to: "~/Documents/{{ file.name }}"
"#,
        )
        .unwrap();
        let options = WatchmanGenerateOptions::new(
            PathBuf::from("/tmp/alder.yaml"),
            PathBuf::from("/bin/alder"),
        );

        let commands = generate_trigger_commands(&config, &options).unwrap();
        let value = serde_json::to_value(&commands[0]).unwrap();

        assert_eq!(value[0], "trigger");
        assert_eq!(value[2]["name"], "alder");
        assert_eq!(value[2]["append_files"], false);
        assert_eq!(
            value[2]["stdin"],
            serde_json::json!(["name", "exists", "type"])
        );
        assert_eq!(
            value[2]["command"],
            serde_json::json!([
                "/bin/alder",
                "ingest",
                "--from-watchman",
                "--config",
                "/tmp/alder.yaml"
            ])
        );
        assert_eq!(
            value[2]["expression"],
            serde_json::json!([
                "allof",
                ["type", "f"],
                ["anyof", ["suffix", "pdf"]],
                ["not", ["anyof", ["suffix", "download"], ["suffix", "tmp"]]]
            ])
        );
    }

    #[test]
    fn generated_expression_uses_wholename_match_for_complex_globs() {
        let config = parse_config_str(
            r#"
version: 1
watch:
  paths:
    - tmp/inbox
  include:
    - "receipts/*.pdf"
rules:
  - id: pdfs
    when: file.ext == ".pdf"
    actions:
      - move:
          to: "~/Documents/{{ file.name }}"
"#,
        )
        .unwrap();
        let options = WatchmanGenerateOptions::new(
            PathBuf::from("/tmp/alder.yaml"),
            PathBuf::from("/bin/alder"),
        );

        let commands = generate_trigger_commands(&config, &options).unwrap();
        let value = serde_json::to_value(&commands[0]).unwrap();

        assert_eq!(
            value[2]["expression"],
            serde_json::json!([
                "allof",
                ["type", "f"],
                ["anyof", ["match", "receipts/*.pdf", "wholename"]]
            ])
        );
    }

    #[test]
    fn ignore_changes_change_generated_expression() {
        let first = config_with_ignore("*.tmp");
        let second = config_with_ignore("*.partial");
        let options = WatchmanGenerateOptions::new(
            PathBuf::from("/tmp/alder.yaml"),
            PathBuf::from("/bin/alder"),
        );

        let first =
            serde_json::to_value(generate_trigger_commands(&first, &options).unwrap()).unwrap();
        let second =
            serde_json::to_value(generate_trigger_commands(&second, &options).unwrap()).unwrap();

        assert_ne!(first, second);
        assert!(second.to_string().contains("partial"));
    }

    #[test]
    fn parses_watchman_stdin_and_reapplies_patterns() {
        let config = config_with_ignore("*.tmp");
        let watch = config.watch.as_ref().unwrap();
        let input = r#"
[
  {"name":"statement.pdf","exists":true,"type":"f"},
  {"name":"notes.txt","exists":true,"type":"f"},
  {"name":"partial.pdf.tmp","exists":true,"type":"f"},
  {"name":"old.pdf","exists":false,"type":"f"},
  {"name":"folder","exists":true,"type":"d"}
]
"#;

        let candidates = parse_watchman_stdin(input, "/watch/root", watch).unwrap();

        assert_eq!(candidates, vec![PathBuf::from("/watch/root/statement.pdf")]);
    }

    #[test]
    fn rejects_parent_components_in_watch_paths() {
        let config = parse_config_str(
            r#"
version: 1
watch:
  paths:
    - ../inbox
rules:
  - id: pdfs
    when: file.ext == ".pdf"
    actions:
      - move:
          to: "~/Documents/{{ file.name }}"
"#,
        )
        .unwrap();
        let options = WatchmanGenerateOptions::new(
            PathBuf::from("/tmp/alder.yaml"),
            PathBuf::from("/bin/alder"),
        );

        let error = generate_trigger_commands(&config, &options).unwrap_err();

        assert!(matches!(error, WatchmanError::UnsafeWatchPath(_)));
    }

    #[test]
    fn rejects_unsafe_watchman_names() {
        let config = config_with_ignore("*.tmp");
        let watch = config.watch.as_ref().unwrap();
        let input = r#"[{"name":"../escape.pdf","exists":true,"type":"f"}]"#;

        let error = parse_watchman_stdin(input, "/watch/root", watch).unwrap_err();

        assert!(matches!(error, WatchmanError::UnsafeWatchmanName(_)));
    }

    fn config_with_ignore(ignore: &str) -> Config {
        parse_config_str(&format!(
            r#"
version: 1
watch:
  paths:
    - tmp/inbox
  include:
    - "*.pdf"
  ignore:
    - "{ignore}"
rules:
  - id: pdfs
    when: file.ext == ".pdf"
    actions:
      - move:
          to: "~/Documents/{{{{ file.name }}}}"
"#
        ))
        .unwrap()
    }
}
