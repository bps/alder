use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::Serialize;
use thiserror::Error;

use crate::config::{Action, Config, ConflictPolicy, Rule};
use crate::expr::{self, Value};
use crate::render::{self, FactStrings};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Explanation {
    pub source: PathBuf,
    pub facts: FactStrings,
    pub rule_evaluations: Vec<RuleEvaluation>,
    pub plan: Option<ActionPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleEvaluation {
    pub rule_id: String,
    pub rule_name: Option<String>,
    pub matched: bool,
    pub shadowed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionPlan {
    pub source: PathBuf,
    pub rule_id: String,
    pub rule_name: Option<String>,
    pub variables: IndexMap<String, String>,
    pub actions: Vec<PlannedAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PlannedAction {
    Move {
        to: PathBuf,
        conflict: ConflictPolicy,
        terminal: bool,
    },
}

pub fn plan_for_file(
    config: &Config,
    source: impl AsRef<Path>,
    facts: &IndexMap<String, Value>,
) -> Result<Explanation, PlanError> {
    let source = source.as_ref().to_path_buf();
    let string_facts = string_facts(facts);
    let mut rule_evaluations = Vec::new();
    let mut plan = None;

    for rule in &config.rules {
        match expr::eval_bool(&rule.when, facts) {
            Ok(true) => {
                let shadowed = plan.is_some();
                rule_evaluations.push(RuleEvaluation {
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    matched: true,
                    shadowed,
                    error: None,
                });

                if plan.is_none() {
                    plan = Some(build_plan(config, rule, &source, &string_facts)?);
                }
            }
            Ok(false) => rule_evaluations.push(RuleEvaluation {
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                matched: false,
                shadowed: false,
                error: None,
            }),
            Err(error) => rule_evaluations.push(RuleEvaluation {
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                matched: false,
                shadowed: false,
                error: Some(error.to_string()),
            }),
        }
    }

    Ok(Explanation {
        source,
        facts: string_facts,
        rule_evaluations,
        plan,
    })
}

fn build_plan(
    config: &Config,
    rule: &Rule,
    source: &Path,
    string_facts: &FactStrings,
) -> Result<ActionPlan, PlanError> {
    let extracted = render::extract_variables(&rule.extract, string_facts)?;
    let template_values = template_values(string_facts, &extracted);
    let mut planned_actions = Vec::new();

    if let Some(action) = rule.actions.first() {
        match action {
            Action::Move(action) => {
                let to = render::render_destination_path(&action.to, &template_values)?;
                planned_actions.push(PlannedAction::Move {
                    to,
                    conflict: action
                        .conflict
                        .or_else(|| {
                            config
                                .defaults
                                .as_ref()
                                .and_then(|defaults| defaults.conflict)
                        })
                        .unwrap_or(ConflictPolicy::AppendCounter),
                    terminal: true,
                });
            }
            other => return Err(PlanError::UnsupportedAction(other.kind_name())),
        }
    }

    Ok(ActionPlan {
        source: source.to_path_buf(),
        rule_id: rule.id.clone(),
        rule_name: rule.name.clone(),
        variables: extracted,
        actions: planned_actions,
    })
}

fn string_facts(facts: &IndexMap<String, Value>) -> FactStrings {
    facts
        .iter()
        .filter_map(|(key, value)| match value {
            Value::String(value) => Some((key.clone(), value.clone())),
            Value::Bool(value) => Some((key.clone(), value.to_string())),
            Value::Null => None,
        })
        .collect()
}

fn template_values(
    string_facts: &FactStrings,
    extracted: &IndexMap<String, String>,
) -> IndexMap<String, String> {
    let mut values = IndexMap::new();

    for key in ["file.name", "file.stem", "file.ext"] {
        if let Some(value) = string_facts.get(key) {
            values.insert(key.to_string(), value.clone());
        }
    }

    values.extend(
        extracted
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    values
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error(transparent)]
    Render(#[from] render::RenderError),
    #[error("action {0:?} is not supported by planning yet")]
    UnsupportedAction(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_config_str;
    use crate::test_support::{facts, no_facts};

    #[test]
    fn plans_move_for_first_matching_rule() {
        let config = parse_config_str(
            r#"
version: 1
defaults:
  conflict: skip
rules:
  - id: pdfs
    when: file.ext == ".pdf"
    actions:
      - move:
          to: "~/Documents/PDFs/{{ file.name }}"
"#,
        )
        .unwrap();
        let facts = facts([("file.ext", ".pdf"), ("file.name", "statement.pdf")]);

        let explanation = plan_for_file(&config, "/tmp/statement.pdf", &facts).unwrap();

        assert_eq!(explanation.rule_evaluations.len(), 1);
        assert!(explanation.rule_evaluations[0].matched);
        let plan = explanation.plan.unwrap();
        assert_eq!(plan.rule_id, "pdfs");
        assert_eq!(
            plan.actions[0],
            PlannedAction::Move {
                to: PathBuf::from("~/Documents/PDFs/statement.pdf"),
                conflict: ConflictPolicy::Skip,
                terminal: true,
            }
        );
    }

    #[test]
    fn nonmatching_rule_yields_no_plan() {
        let config = parse_config_str(
            r#"
version: 1
rules:
  - id: pdfs
    when: file.ext == ".pdf"
    actions:
      - move:
          to: "PDFs/{{ file.name }}"
"#,
        )
        .unwrap();
        let facts = facts([("file.ext", ".txt"), ("file.name", "notes.txt")]);

        let explanation = plan_for_file(&config, "/tmp/notes.txt", &facts).unwrap();

        assert!(explanation.plan.is_none());
        assert!(!explanation.rule_evaluations[0].matched);
    }

    #[test]
    fn extraction_variable_renders_destination() {
        let config = parse_config_str(
            r#"
version: 1
rules:
  - id: amex
    when: contains(pdf.text, "American Express")
    extract:
      year:
        from: pdf.text
        regex: "Closing Date\\s+(\\d{4})-\\d{2}-\\d{2}"
    actions:
      - move:
          to: "Finance/Amex/{{ year }}/{{ file.name }}"
"#,
        )
        .unwrap();
        let facts = facts([
            ("file.name", "amex.pdf"),
            ("pdf.text", "American Express Closing Date 2026-04-15"),
        ]);

        let explanation = plan_for_file(&config, "/tmp/amex.pdf", &facts).unwrap();

        assert_eq!(
            explanation.plan.unwrap().actions[0],
            PlannedAction::Move {
                to: PathBuf::from("Finance/Amex/2026/amex.pdf"),
                conflict: ConflictPolicy::AppendCounter,
                terminal: true,
            }
        );
    }

    #[test]
    fn unsafe_extraction_value_errors() {
        let config = parse_config_str(
            r#"
version: 1
rules:
  - id: unsafe
    when: "true"
    extract:
      category:
        from: pdf.text
        regex: "Category: (.*)"
    actions:
      - move:
          to: "{{ category }}/{{ file.name }}"
"#,
        )
        .unwrap();
        let facts = facts([
            ("file.name", "statement.pdf"),
            ("pdf.text", "Category: ../../evil"),
        ]);

        let error = plan_for_file(&config, "/tmp/statement.pdf", &facts).unwrap_err();

        assert!(matches!(
            error,
            PlanError::Render(render::RenderError::UnsafeVariable { .. })
        ));
    }

    #[test]
    fn later_matching_rules_are_marked_shadowed() {
        let config = parse_config_str(
            r#"
version: 1
rules:
  - id: first
    when: "true"
    actions:
      - move:
          to: "First/{{ file.name }}"
  - id: second
    when: "true"
    actions:
      - move:
          to: "Second/{{ file.name }}"
"#,
        )
        .unwrap();
        let facts = facts([("file.name", "statement.pdf")]);

        let explanation = plan_for_file(&config, "/tmp/statement.pdf", &facts).unwrap();

        assert_eq!(explanation.plan.unwrap().rule_id, "first");
        assert!(explanation.rule_evaluations[1].matched);
        assert!(explanation.rule_evaluations[1].shadowed);
    }

    #[test]
    fn unsupported_action_errors() {
        let config = parse_config_str(
            r#"
version: 1
rules:
  - id: review
    when: "true"
    actions:
      - review:
"#,
        )
        .unwrap();
        let facts = no_facts();

        let error = plan_for_file(&config, "/tmp/file.pdf", &facts).unwrap_err();

        assert!(matches!(error, PlanError::UnsupportedAction("review")));
    }
}
