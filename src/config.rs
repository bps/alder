use std::collections::HashSet;

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,

    #[serde(default)]
    pub watch: Option<WatchConfig>,

    #[serde(default)]
    pub stabilize: Option<StabilizeConfig>,

    #[serde(default)]
    pub defaults: Option<DefaultsConfig>,

    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchConfig {
    #[serde(default)]
    pub paths: Vec<String>,

    #[serde(default)]
    pub include: Vec<String>,

    #[serde(default)]
    pub ignore: Vec<String>,

    #[serde(default)]
    pub settle: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StabilizeConfig {
    #[serde(default)]
    pub unchanged_for: Option<String>,

    #[serde(default)]
    pub timeout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    #[serde(default)]
    pub conflict: Option<ConflictPolicy>,

    #[serde(default)]
    pub destination_roots: Vec<String>,

    #[serde(default)]
    pub unmatched: Option<UnmatchedAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Error,
    Skip,
    AppendCounter,
    ReplaceIfSameHash,
    Review,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnmatchedAction {
    #[serde(default)]
    pub move_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,

    #[serde(default)]
    pub name: Option<String>,

    pub when: String,

    #[serde(default)]
    pub extract: IndexMap<String, Extractor>,

    #[serde(default, with = "serde_yaml::with::singleton_map_recursive")]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extractor {
    Regex(RegexExtractor),
    Date(DateExtractor),
}

impl Extractor {
    pub fn from(&self) -> &str {
        match self {
            Self::Regex(extractor) => &extractor.from,
            Self::Date(extractor) => &extractor.from,
        }
    }
}

impl<'de> Deserialize<'de> for Extractor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        let kind = extractor_kind(&value).map_err(serde::de::Error::custom)?;
        match kind.as_deref() {
            None | Some("regex") => RawRegexExtractor::deserialize(value)
                .map(|raw| Self::Regex(raw.into()))
                .map_err(serde::de::Error::custom),
            Some("date") => RawDateExtractor::deserialize(value)
                .map(|raw| Self::Date(raw.into()))
                .map_err(serde::de::Error::custom),
            Some(other) => Err(serde::de::Error::custom(format!(
                "unknown extractor kind {other:?}; expected \"regex\" or \"date\""
            ))),
        }
    }
}

fn extractor_kind(value: &serde_yaml::Value) -> Result<Option<String>, String> {
    let serde_yaml::Value::Mapping(mapping) = value else {
        return Err("extractor must be a mapping".to_string());
    };
    let kind_key = serde_yaml::Value::String("kind".to_string());
    match mapping.get(&kind_key) {
        None => Ok(None),
        Some(serde_yaml::Value::String(kind)) => Ok(Some(kind.clone())),
        Some(_) => Err("extractor kind must be a string".to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexExtractor {
    pub from: String,
    pub regex: String,

    pub format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegexExtractor {
    #[serde(default)]
    kind: Option<RegexExtractorKind>,
    from: String,
    regex: String,

    #[serde(default)]
    format: Option<String>,
}

impl From<RawRegexExtractor> for RegexExtractor {
    fn from(raw: RawRegexExtractor) -> Self {
        let RawRegexExtractor {
            kind: _,
            from,
            regex,
            format,
        } = raw;
        Self {
            from,
            regex,
            format,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RegexExtractorKind {
    Regex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateExtractor {
    pub from: String,
    pub after: Option<String>,
    pub near: Option<String>,
    pub scope: Option<DateScope>,
    pub window: Option<DateWindow>,
    pub formats: Vec<String>,
    pub min_year: Option<i32>,
    pub max_year: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDateExtractor {
    kind: DateExtractorKind,
    from: String,

    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    near: Option<String>,
    #[serde(default)]
    scope: Option<DateScope>,
    #[serde(default)]
    window: Option<DateWindow>,
    formats: Vec<String>,
    #[serde(default)]
    min_year: Option<i32>,
    #[serde(default)]
    max_year: Option<i32>,
}

impl From<RawDateExtractor> for DateExtractor {
    fn from(raw: RawDateExtractor) -> Self {
        let RawDateExtractor {
            kind: _,
            from,
            after,
            near,
            scope,
            window,
            formats,
            min_year,
            max_year,
        } = raw;
        Self {
            from,
            after,
            near,
            scope,
            window,
            formats,
            min_year,
            max_year,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DateExtractorKind {
    Date,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateScope {
    Document,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateWindow {
    SameLine,
    NextLine,
    Paragraph,
    Chars(usize),
}

impl<'de> Deserialize<'de> for DateWindow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "same_line" => Ok(Self::SameLine),
            "next_line" => Ok(Self::NextLine),
            "paragraph" => Ok(Self::Paragraph),
            _ => {
                if let Some(count) = value.strip_prefix("chars:") {
                    let count = count.parse::<usize>().map_err(|_| {
                        serde::de::Error::custom(format!(
                            "invalid date window {value:?}; chars window must be chars:N"
                        ))
                    })?;
                    if count == 0 {
                        Err(serde::de::Error::custom(
                            "invalid date window \"chars:0\"; character count must be positive",
                        ))
                    } else {
                        Ok(Self::Chars(count))
                    }
                } else {
                    Err(serde::de::Error::custom(format!(
                        "invalid date window {value:?}; expected same_line, next_line, paragraph, or chars:N"
                    )))
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Move(DestinationAction),
    Copy(DestinationAction),
    Rename(DestinationAction),
    Tag(TagAction),
    Trash(#[serde(default)] TrashAction),
    Review(#[serde(default)] ReviewAction),
    MoveToReview(#[serde(default)] MoveToReviewAction),
}

impl Action {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Action::Move(_) => "move",
            Action::Copy(_) => "copy",
            Action::Rename(_) => "rename",
            Action::Tag(_) => "tag",
            Action::Trash(_) => "trash",
            Action::Review(_) => "review",
            Action::MoveToReview(_) => "move_to_review",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationAction {
    pub to: String,

    #[serde(default)]
    pub conflict: Option<ConflictPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagAction {
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrashAction {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAction {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveToReviewAction {
    #[serde(default)]
    pub to: Option<String>,

    #[serde(default)]
    pub conflict: Option<ConflictPolicy>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to parse config: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("invalid config: {}", .0.join("; "))]
    Invalid(Vec<String>),
}

pub fn parse_config_str(input: &str) -> Result<Config, ConfigError> {
    let config: Config = serde_yaml::from_str(input)?;
    validate_config(&config)?;
    Ok(config)
}

pub fn validate_config(config: &Config) -> Result<(), ConfigError> {
    let mut errors = Vec::new();

    if config.version != 1 {
        errors.push(format!(
            "unsupported version {}; expected version 1",
            config.version
        ));
    }

    let mut rule_ids = HashSet::new();
    for rule in &config.rules {
        if rule.id.trim().is_empty() {
            errors.push("rule id must not be empty".to_string());
        } else if !rule_ids.insert(rule.id.as_str()) {
            errors.push(format!("duplicate rule id {:?}", rule.id));
        }

        if rule.when.trim().is_empty() {
            errors.push(format!("rule {:?} must have a non-empty when", rule.id));
        }

        if rule.actions.is_empty() {
            errors.push(format!(
                "rule {:?} must declare at least one action",
                rule.id
            ));
        }

        for (name, extractor) in &rule.extract {
            validate_extractor(&rule.id, name, extractor, &mut errors);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::Invalid(errors))
    }
}

fn validate_extractor(rule_id: &str, name: &str, extractor: &Extractor, errors: &mut Vec<String>) {
    if extractor.from().trim().is_empty() {
        errors.push(format!(
            "rule {rule_id:?} extractor {name:?} must have a non-empty from"
        ));
    }

    let Extractor::Date(extractor) = extractor else {
        return;
    };

    let context_count = usize::from(extractor.after.is_some())
        + usize::from(extractor.near.is_some())
        + usize::from(extractor.scope == Some(DateScope::Document));
    if context_count != 1 {
        errors.push(format!(
            "rule {rule_id:?} date extractor {name:?} must specify exactly one of after, near, or scope: document"
        ));
    }

    if extractor.formats.is_empty() {
        errors.push(format!(
            "rule {rule_id:?} date extractor {name:?} must specify at least one format"
        ));
    }

    if extractor
        .after
        .as_ref()
        .is_some_and(|label| normalized_date_label_is_empty(label))
        || extractor
            .near
            .as_ref()
            .is_some_and(|label| normalized_date_label_is_empty(label))
    {
        errors.push(format!(
            "rule {rule_id:?} date extractor {name:?} labels must not be empty or punctuation-only"
        ));
    }

    if let (Some(min_year), Some(max_year)) = (extractor.min_year, extractor.max_year)
        && min_year > max_year
    {
        errors.push(format!(
            "rule {rule_id:?} date extractor {name:?} min_year must be <= max_year"
        ));
    }
}

fn normalized_date_label_is_empty(label: &str) -> bool {
    label
        .trim()
        .trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, ':' | '.' | '-' | '#'))
        .trim()
        .is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
version: 1

watch:
  paths:
    - ~/Downloads
  include:
    - "*.pdf"
  ignore:
    - "*.download"
    - "*.crdownload"
    - "*.part"
    - "*.tmp"
  settle: 5s

stabilize:
  unchanged_for: 3s
  timeout: 60s

defaults:
  conflict: append_counter
  unmatched:
    move_to: "~/Documents/_Inbox/PDF Review/{{ file.name }}"

rules:
  - id: amex-statements
    name: American Express statements
    when: |
      file.ext == ".pdf" &&
      contains(pdf.text, "American Express")
    extract:
      statement_date:
        from: pdf.text
        regex: "Closing Date\\s+(\\d{2}/\\d{2}/\\d{4})"
        format: "%m/%d/%Y"
    actions:
      - move:
          to: "~/Documents/Finance/{{ statement_date }}.pdf"
"#;

    #[test]
    fn parses_design_example() {
        let config = parse_config_str(EXAMPLE).unwrap();

        assert_eq!(config.version, 1);
        assert_eq!(config.rules.len(), 1);
        assert_eq!(config.rules[0].id, "amex-statements");
        assert_eq!(config.rules[0].extract.len(), 1);
        assert!(matches!(
            config.rules[0].actions.first(),
            Some(Action::Move(action)) if action.to.contains("statement_date")
        ));
    }

    #[test]
    fn parses_legacy_and_explicit_regex_extractors() {
        let config = parse_config_str(
            r#"
version: 1
rules:
  - id: regexes
    when: "true"
    extract:
      legacy:
        from: pdf.text
        regex: "Legacy (\\d+)"
      explicit:
        kind: regex
        from: pdf.text
        regex: "Explicit (\\d+)"
    actions:
      - review:
"#,
        )
        .unwrap();

        assert!(matches!(
            config.rules[0].extract["legacy"],
            Extractor::Regex(_)
        ));
        assert!(matches!(
            config.rules[0].extract["explicit"],
            Extractor::Regex(_)
        ));
    }

    #[test]
    fn parses_date_extractor() {
        let config = parse_config_str(
            r#"
version: 1
rules:
  - id: date
    when: "true"
    extract:
      statement_date:
        kind: date
        from: pdf.text
        after: "Statement Date:"
        formats: ["%m/%d/%Y"]
        min_year: 2020
        max_year: 2030
    actions:
      - review:
"#,
        )
        .unwrap();

        match &config.rules[0].extract["statement_date"] {
            Extractor::Date(extractor) => {
                assert_eq!(extractor.after.as_deref(), Some("Statement Date:"));
                assert_eq!(extractor.formats, vec!["%m/%d/%Y".to_string()]);
            }
            other => panic!("expected date extractor, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_date_extractor_key() {
        let error = parse_config_str(
            r#"
version: 1
rules:
  - id: date
    when: "true"
    extract:
      statement_date:
        kind: date
        from: pdf.text
        after: "Statement Date:"
        formats: ["%m/%d/%Y"]
        typo: nope
    actions:
      - review:
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn validates_date_extractor_context_and_formats() {
        let error = parse_config_str(
            r#"
version: 1
rules:
  - id: date
    when: "true"
    extract:
      statement_date:
        kind: date
        from: pdf.text
        after: "Statement Date:"
        near: Close Date
        formats: []
    actions:
      - review:
"#,
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("exactly one"));
        assert!(message.contains("at least one format"));
    }

    #[test]
    fn rejects_punctuation_only_date_label() {
        let error = parse_config_str(
            r#"
version: 1
rules:
  - id: date
    when: "true"
    extract:
      statement_date:
        kind: date
        from: pdf.text
        after: " : "
        formats: ["%m/%d/%Y"]
    actions:
      - review:
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("punctuation-only"));
    }

    #[test]
    fn rejects_unknown_root_key() {
        let error = parse_config_str(
            r#"
version: 1
surprise: true
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_unknown_nested_key() {
        let error = parse_config_str(
            r#"
version: 1
rules:
  - id: example
    when: file.ext == ".pdf"
    unexpected: true
    actions:
      - review:
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_unknown_action_key() {
        let error = parse_config_str(
            r#"
version: 1
rules:
  - id: example
    when: file.ext == ".pdf"
    actions:
      - move:
          to: review.pdf
          shell: rm -rf /
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_missing_version() {
        let error = parse_config_str(
            r#"
rules:
  - id: example
    when: "true"
    actions:
      - review:
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("missing field `version`"));
    }

    #[test]
    fn rejects_duplicate_rule_ids() {
        let error = parse_config_str(
            r#"
version: 1
rules:
  - id: duplicate
    when: "true"
    actions:
      - review:
  - id: duplicate
    when: "true"
    actions:
      - review:
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("duplicate rule id"));
    }

    #[test]
    fn rejects_rule_without_actions() {
        let error = parse_config_str(
            r#"
version: 1
rules:
  - id: no-actions
    when: "true"
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("at least one action"));
    }

    #[test]
    fn parses_multiple_action_shapes() {
        let config = parse_config_str(
            r#"
version: 1
rules:
  - id: actions
    when: "true"
    actions:
      - review:
      - review:
          reason: needs manual classification
      - move_to_review:
      - move_to_review:
          to: ~/Documents/Review/{{ file.name }}
      - trash: {}
      - trash:
      - tag:
          tags:
            - tax
            - todo
"#,
        )
        .unwrap();

        assert_eq!(config.rules[0].actions.len(), 7);
        match &config.rules[0].actions[0] {
            Action::Review(action) => assert_eq!(action, &ReviewAction::default()),
            other => panic!("expected review action, got {other:?}"),
        }
        match &config.rules[0].actions[1] {
            Action::Review(action) => {
                assert_eq!(
                    action.reason.as_deref(),
                    Some("needs manual classification")
                );
            }
            other => panic!("expected review action, got {other:?}"),
        }
        match &config.rules[0].actions[2] {
            Action::MoveToReview(action) => assert_eq!(action, &MoveToReviewAction::default()),
            other => panic!("expected move_to_review action, got {other:?}"),
        }
        match &config.rules[0].actions[3] {
            Action::MoveToReview(action) => {
                assert_eq!(
                    action.to.as_deref(),
                    Some("~/Documents/Review/{{ file.name }}")
                );
            }
            other => panic!("expected move_to_review action, got {other:?}"),
        }
        match &config.rules[0].actions[4] {
            Action::Trash(action) => assert_eq!(action, &TrashAction::default()),
            other => panic!("expected trash action, got {other:?}"),
        }
        match &config.rules[0].actions[5] {
            Action::Trash(action) => assert_eq!(action, &TrashAction::default()),
            other => panic!("expected trash action, got {other:?}"),
        }
        assert!(matches!(config.rules[0].actions[6], Action::Tag(_)));
    }
}
