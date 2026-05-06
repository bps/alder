use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::config::{Config, WatchConfig};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriggerCommand(String, PathBuf, TriggerDefinition);

impl TriggerCommand {
    pub fn root(&self) -> &Path {
        &self.1
    }

    pub fn definition(&self) -> &TriggerDefinition {
        &self.2
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriggerDefinition {
    pub name: String,
    pub expression: WatchmanExpression,
    pub command: Vec<String>,
    pub append_files: bool,
    pub stdin: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchmanExpression {
    Type(String),
    Suffix(String),
    Match { pattern: String, scope: String },
    AllOf(Vec<WatchmanExpression>),
    AnyOf(Vec<WatchmanExpression>),
    Not(Box<WatchmanExpression>),
}

impl Serialize for WatchmanExpression {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Type(value) => vec![
                Value::String("type".to_string()),
                Value::String(value.clone()),
            ]
            .serialize(serializer),
            Self::Suffix(value) => vec![
                Value::String("suffix".to_string()),
                Value::String(value.clone()),
            ]
            .serialize(serializer),
            Self::Match { pattern, scope } => vec![
                Value::String("match".to_string()),
                Value::String(pattern.clone()),
                Value::String(scope.clone()),
            ]
            .serialize(serializer),
            Self::AllOf(items) => serialize_variadic("allof", items, serializer),
            Self::AnyOf(items) => serialize_variadic("anyof", items, serializer),
            Self::Not(item) => vec![
                Value::String("not".to_string()),
                serde_json::to_value(item).map_err(serde::ser::Error::custom)?,
            ]
            .serialize(serializer),
        }
    }
}

fn serialize_variadic<S>(
    op: &str,
    items: &[WatchmanExpression],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut values = Vec::with_capacity(items.len() + 1);
    values.push(Value::String(op.to_string()));
    for item in items {
        values.push(serde_json::to_value(item).map_err(serde::ser::Error::custom)?);
    }
    values.serialize(serializer)
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
        commands.push(TriggerCommand(
            "trigger".to_string(),
            root,
            TriggerDefinition {
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
        ));
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

    fn expression(&self) -> WatchmanExpression {
        let mut clauses = vec![WatchmanExpression::Type("f".to_string())];

        if !self.include.is_empty() {
            clauses.push(pattern_expression(&self.include));
        }
        if !self.ignore.is_empty() {
            clauses.push(WatchmanExpression::Not(Box::new(pattern_expression(
                &self.ignore,
            ))));
        }

        WatchmanExpression::AllOf(clauses)
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

fn pattern_expression(patterns: &[String]) -> WatchmanExpression {
    let expressions = patterns
        .iter()
        .map(|pattern| pattern_to_expression(pattern))
        .collect();
    WatchmanExpression::AnyOf(expressions)
}

fn pattern_to_expression(pattern: &str) -> WatchmanExpression {
    if let Some(suffix) = simple_suffix(pattern) {
        WatchmanExpression::Suffix(suffix)
    } else {
        WatchmanExpression::Match {
            pattern: pattern.to_string(),
            scope: "wholename".to_string(),
        }
    }
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
    let expanded = if path == "~" || path.starts_with("~/") {
        let home = env::var_os("HOME").ok_or(WatchmanError::HomeUnavailable)?;
        if path == "~" {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(&path[2..])
        }
    } else if path.starts_with('~') {
        return Err(WatchmanError::UnsupportedTilde(path.to_string()));
    } else {
        PathBuf::from(path)
    };

    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Ok(std::path::absolute(&expanded)
            .map_err(|error| WatchmanError::io("make watch path absolute", &expanded, error))?)
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
            Value::String("watch-project".to_string()),
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

#[derive(Debug)]
pub enum WatchmanError {
    MissingWatchConfig,
    MissingWatchPaths,
    HomeUnavailable,
    UnsupportedTilde(String),
    Pattern {
        pattern: String,
        message: String,
    },
    UnsafeWatchmanName(String),
    Watchman(String),
    TriggerDrift {
        root: PathBuf,
        message: String,
    },
    UnexpectedResponse(Value),
    UnexpectedIo(String),
    CommandFailed {
        status: Option<i32>,
        stderr: String,
    },
    Io {
        op: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Json(serde_json::Error),
    JsonShape(serde_json::Error),
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

impl From<serde_json::Error> for WatchmanError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl fmt::Display for WatchmanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingWatchConfig => write!(f, "config does not define watch settings"),
            Self::MissingWatchPaths => write!(f, "config watch.paths must not be empty"),
            Self::HomeUnavailable => write!(f, "HOME is not available for ~/ expansion"),
            Self::UnsupportedTilde(path) => write!(f, "unsupported ~user path {path:?}"),
            Self::Pattern { pattern, message } => {
                write!(f, "invalid watch pattern {pattern:?}: {message}")
            }
            Self::UnsafeWatchmanName(name) => write!(f, "unsafe Watchman path name {name:?}"),
            Self::Watchman(message) => write!(f, "watchman error: {message}"),
            Self::TriggerDrift { root, message } => {
                write!(
                    f,
                    "Watchman trigger drift for {}: {message}",
                    root.display()
                )
            }
            Self::UnexpectedResponse(value) => write!(f, "unexpected Watchman response: {value}"),
            Self::UnexpectedIo(message) => write!(f, "unexpected IO state: {message}"),
            Self::CommandFailed { status, stderr } => {
                write!(f, "watchman exited with {status:?}: {}", stderr.trim())
            }
            Self::Io { op, path, source } => {
                write!(f, "failed to {op} for {}: {source}", path.display())
            }
            Self::Json(error) | Self::JsonShape(error) => write!(f, "JSON error: {error}"),
        }
    }
}

impl std::error::Error for WatchmanError {}

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
        assert!(value[2]["expression"].to_string().contains("suffix"));
        assert!(value[2]["expression"].to_string().contains("download"));
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
