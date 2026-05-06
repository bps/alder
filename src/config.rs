use std::collections::HashSet;
use std::fmt;

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_yaml::Value;

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

    #[serde(default)]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extractor {
    pub from: String,
    pub regex: String,

    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Move(MoveAction),
    Copy(CopyAction),
    Rename(RenameAction),
    Tag(TagAction),
    Review(ReviewAction),
    MoveToReview(MoveToReviewAction),
}

impl<'de> Deserialize<'de> for Action {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let mapping = value
            .as_mapping()
            .ok_or_else(|| de::Error::custom("action must be a single-key map"))?;

        if mapping.len() != 1 {
            return Err(de::Error::custom(
                "action must contain exactly one action key",
            ));
        }

        let (key, body) = mapping.iter().next().expect("mapping has one entry");
        let key = key
            .as_str()
            .ok_or_else(|| de::Error::custom("action key must be a string"))?;

        match key {
            "move" => deserialize_action_body(body, key).map(Self::Move),
            "copy" => deserialize_action_body(body, key).map(Self::Copy),
            "rename" => deserialize_action_body(body, key).map(Self::Rename),
            "tag" => deserialize_action_body(body, key).map(Self::Tag),
            "review" => deserialize_optional_action_body(body, key).map(Self::Review),
            "move_to_review" => deserialize_optional_action_body(body, key).map(Self::MoveToReview),
            other => Err(de::Error::custom(format!("unknown action {other:?}"))),
        }
    }
}

fn deserialize_action_body<T, E>(body: &Value, key: &str) -> Result<T, E>
where
    T: for<'de> Deserialize<'de>,
    E: de::Error,
{
    serde_yaml::from_value(body.clone())
        .map_err(|error| E::custom(format!("invalid {key} action: {error}")))
}

fn deserialize_optional_action_body<T, E>(body: &Value, key: &str) -> Result<T, E>
where
    T: for<'de> Deserialize<'de> + Default,
    E: de::Error,
{
    if matches!(body, Value::Null) {
        Ok(T::default())
    } else {
        deserialize_action_body(body, key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveAction {
    pub to: String,

    #[serde(default)]
    pub conflict: Option<ConflictPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyAction {
    pub to: String,

    #[serde(default)]
    pub conflict: Option<ConflictPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameAction {
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

#[derive(Debug)]
pub enum ConfigError {
    Parse(serde_yaml::Error),
    Invalid(Vec<String>),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "failed to parse config: {error}"),
            Self::Invalid(errors) => write!(f, "invalid config: {}", errors.join("; ")),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<serde_yaml::Error> for ConfigError {
    fn from(error: serde_yaml::Error) -> Self {
        Self::Parse(error)
    }
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
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::Invalid(errors))
    }
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
          to: ~/Documents/Review/{{ file.name }}
      - tag:
          tags:
            - tax
            - todo
"#,
        )
        .unwrap();

        assert_eq!(config.rules[0].actions.len(), 4);
        assert!(matches!(config.rules[0].actions[0], Action::Review(_)));
        assert!(matches!(config.rules[0].actions[1], Action::Review(_)));
        assert!(matches!(
            config.rules[0].actions[2],
            Action::MoveToReview(_)
        ));
        assert!(matches!(config.rules[0].actions[3], Action::Tag(_)));
    }
}
