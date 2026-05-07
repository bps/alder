use std::collections::HashSet;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extractor {
    pub from: String,
    pub regex: String,

    #[serde(default)]
    pub format: Option<String>,
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
