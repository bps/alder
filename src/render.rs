use std::path::{Component, Path, PathBuf};

use chrono::NaiveDate;
use indexmap::IndexMap;
use minijinja::{Environment, Error as MiniJinjaError, ErrorKind, UndefinedBehavior};
use regex::Regex;
use serde_json::{Map as JsonMap, Value as JsonValue};
use thiserror::Error;

use crate::config::Extractor;

pub type FactStrings = IndexMap<String, String>;

pub fn extract_variables(
    extractors: &IndexMap<String, Extractor>,
    facts: &FactStrings,
) -> Result<IndexMap<String, String>, RenderError> {
    let mut variables = IndexMap::new();

    for (name, extractor) in extractors {
        let source = facts
            .get(&extractor.from)
            .ok_or_else(|| RenderError::MissingFact {
                variable: name.clone(),
                fact: extractor.from.clone(),
            })?;
        let regex = Regex::new(&extractor.regex).map_err(|error| RenderError::BadRegex {
            variable: name.clone(),
            regex: extractor.regex.clone(),
            message: error.to_string(),
        })?;
        let captures = regex.captures(source).ok_or_else(|| RenderError::NoMatch {
            variable: name.clone(),
            fact: extractor.from.clone(),
        })?;

        let captured = captures
            .name(name)
            .or_else(|| captures.get(1))
            .or_else(|| captures.get(0))
            .expect("captures always include full match")
            .as_str();
        let value = match &extractor.format {
            Some(format) => parse_date_variable(name, captured, format)?,
            None => captured.to_string(),
        };

        variables.insert(name.clone(), value);
    }

    Ok(variables)
}

fn parse_date_variable(variable: &str, value: &str, format: &str) -> Result<String, RenderError> {
    NaiveDate::parse_from_str(value, format)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .map_err(|error| RenderError::DateParse {
            variable: variable.to_string(),
            value: value.to_string(),
            format: format.to_string(),
            message: error.to_string(),
        })
}

pub fn render_template(
    template: &str,
    variables: &IndexMap<String, String>,
) -> Result<String, RenderError> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.add_filter("date", date_filter);
    let context = build_template_context(variables)?;
    let tmpl = env
        .template_from_str(template)
        .map_err(|error| RenderError::Template(error.to_string()))?;
    tmpl.render(context)
        .map_err(|error| RenderError::Template(error.to_string()))
}

fn date_filter(value: &str, format: &str) -> Result<String, MiniJinjaError> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|error| {
        MiniJinjaError::new(
            ErrorKind::InvalidOperation,
            format!("date filter expects YYYY-MM-DD input, got {value:?}: {error}"),
        )
    })?;
    Ok(date.format(format).to_string())
}

fn build_template_context(variables: &IndexMap<String, String>) -> Result<JsonValue, RenderError> {
    let mut root = JsonMap::new();
    for (key, value) in variables {
        let parts: Vec<&str> = key.split('.').collect();
        insert_dotted_value(&mut root, &parts, key, value.clone())?;
    }
    Ok(JsonValue::Object(root))
}

fn insert_dotted_value(
    map: &mut JsonMap<String, JsonValue>,
    parts: &[&str],
    full_key: &str,
    value: String,
) -> Result<(), RenderError> {
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err(RenderError::TemplateContext(format!(
            "invalid empty component in template key {full_key:?}"
        )));
    }

    if parts.len() == 1 {
        if map
            .insert(parts[0].to_string(), JsonValue::String(value))
            .is_some()
        {
            return Err(RenderError::TemplateContext(format!(
                "duplicate template key {full_key:?}"
            )));
        }
        return Ok(());
    }

    let entry = map
        .entry(parts[0].to_string())
        .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    let JsonValue::Object(child) = entry else {
        return Err(RenderError::TemplateContext(format!(
            "template key {full_key:?} conflicts with scalar key {:?}",
            parts[0]
        )));
    };
    insert_dotted_value(child, &parts[1..], full_key, value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMode {
    Relative,
    Destination,
}

pub fn render_safe_relative_path(
    template: &str,
    variables: &IndexMap<String, String>,
) -> Result<PathBuf, RenderError> {
    render_safe_path(template, variables, PathMode::Relative)
}

pub fn render_destination_path(
    template: &str,
    variables: &IndexMap<String, String>,
) -> Result<PathBuf, RenderError> {
    render_safe_path(template, variables, PathMode::Destination)
}

pub fn render_safe_path(
    template: &str,
    variables: &IndexMap<String, String>,
    mode: PathMode,
) -> Result<PathBuf, RenderError> {
    for (name, value) in variables {
        validate_path_segment_value(name, value)?;
    }

    let rendered = render_template(template, variables)?;
    validate_rendered_path(&rendered, mode)?;
    Ok(PathBuf::from(rendered))
}

pub fn sanitize_path_segment(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | '\0' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();

    if sanitized == "." || sanitized == ".." {
        sanitized.replace('.', "_")
    } else {
        sanitized
    }
}

fn validate_path_segment_value(name: &str, value: &str) -> Result<(), RenderError> {
    if value.is_empty() {
        return Err(RenderError::UnsafeVariable {
            variable: name.to_string(),
            reason: "value must not be empty".to_string(),
        });
    }

    if value == "." || value == ".." {
        return Err(RenderError::UnsafeVariable {
            variable: name.to_string(),
            reason: "value must not be . or ..".to_string(),
        });
    }

    if value
        .chars()
        .any(|ch| matches!(ch, '/' | '\\' | '\0') || ch.is_control())
    {
        return Err(RenderError::UnsafeVariable {
            variable: name.to_string(),
            reason: "value contains a path separator, NUL, or control character".to_string(),
        });
    }

    Ok(())
}

fn validate_rendered_path(rendered: &str, mode: PathMode) -> Result<(), RenderError> {
    if rendered.trim().is_empty() {
        return Err(RenderError::UnsafePath {
            path: rendered.to_string(),
            reason: "rendered path must not be empty".to_string(),
        });
    }

    if rendered.contains('\0') {
        return Err(RenderError::UnsafePath {
            path: rendered.to_string(),
            reason: "rendered path contains NUL".to_string(),
        });
    }

    if rendered.starts_with('~') && rendered != "~" && !rendered.starts_with("~/") {
        return Err(RenderError::UnsafePath {
            path: rendered.to_string(),
            reason: "~user destinations are not supported; use ~/ for the current user".to_string(),
        });
    }

    let path = Path::new(rendered);
    for component in path.components() {
        match component {
            Component::Normal(part) if part.is_empty() => {
                return Err(RenderError::UnsafePath {
                    path: rendered.to_string(),
                    reason: "rendered path contains an empty component".to_string(),
                });
            }
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(RenderError::UnsafePath {
                    path: rendered.to_string(),
                    reason: "rendered path must not contain ..".to_string(),
                });
            }
            Component::RootDir | Component::Prefix(_) if mode == PathMode::Relative => {
                return Err(RenderError::UnsafePath {
                    path: rendered.to_string(),
                    reason: "rendered path must be relative".to_string(),
                });
            }
            Component::RootDir | Component::Prefix(_) => {}
        }
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RenderError {
    #[error("extractor {variable:?} requires missing fact {fact:?}")]
    MissingFact { variable: String, fact: String },
    #[error("extractor {variable:?} has bad regex {regex:?}: {message}")]
    BadRegex {
        variable: String,
        regex: String,
        message: String,
    },
    #[error("extractor {variable:?} did not match fact {fact:?}")]
    NoMatch { variable: String, fact: String },
    #[error(
        "extractor {variable:?} could not parse date {value:?} with format {format:?}: {message}"
    )]
    DateParse {
        variable: String,
        value: String,
        format: String,
        message: String,
    },
    #[error("template error: {0}")]
    Template(String),
    #[error("template context error: {0}")]
    TemplateContext(String),
    #[error("unsafe template variable {variable:?}: {reason}")]
    UnsafeVariable { variable: String, reason: String },
    #[error("unsafe rendered path {path:?}: {reason}")]
    UnsafePath { path: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_first_capture_group() {
        let extractors = extractors([(
            "statement_date",
            Extractor {
                from: "pdf.text".to_string(),
                regex: r"Closing Date\s+(\d{2}/\d{2}/\d{4})".to_string(),
                format: None,
            },
        )]);
        let facts = facts([(
            "pdf.text",
            "American Express Closing Date 04/15/2026".to_string(),
        )]);

        let variables = extract_variables(&extractors, &facts).unwrap();

        assert_eq!(variables["statement_date"], "04/15/2026");
    }

    #[test]
    fn prefers_named_capture_matching_variable_name() {
        let extractors = extractors([(
            "statement_date",
            Extractor {
                from: "pdf.text".to_string(),
                regex: r"Closing Date\s+(?P<statement_date>\d{4}-\d{2}-\d{2})".to_string(),
                format: None,
            },
        )]);
        let facts = facts([("pdf.text", "Closing Date 2026-04-15".to_string())]);

        let variables = extract_variables(&extractors, &facts).unwrap();

        assert_eq!(variables["statement_date"], "2026-04-15");
    }

    #[test]
    fn missing_fact_errors() {
        let extractors = extractors([(
            "statement_date",
            Extractor {
                from: "pdf.text".to_string(),
                regex: "date".to_string(),
                format: None,
            },
        )]);

        let error = extract_variables(&extractors, &FactStrings::new()).unwrap_err();

        assert!(matches!(error, RenderError::MissingFact { .. }));
    }

    #[test]
    fn no_regex_match_errors() {
        let extractors = extractors([(
            "statement_date",
            Extractor {
                from: "pdf.text".to_string(),
                regex: "Closing Date".to_string(),
                format: None,
            },
        )]);
        let facts = facts([("pdf.text", "no date here".to_string())]);

        let error = extract_variables(&extractors, &facts).unwrap_err();

        assert!(matches!(error, RenderError::NoMatch { .. }));
    }

    #[test]
    fn extractor_format_parses_date_to_iso() {
        let extractors = extractors([(
            "statement_date",
            Extractor {
                from: "pdf.text".to_string(),
                regex: r"Closing Date\s+(\d{2}/\d{2}/\d{4})".to_string(),
                format: Some("%m/%d/%Y".to_string()),
            },
        )]);
        let facts = facts([("pdf.text", "Closing Date 04/15/2026".to_string())]);

        let variables = extract_variables(&extractors, &facts).unwrap();

        assert_eq!(variables["statement_date"], "2026-04-15");
    }

    #[test]
    fn invalid_extractor_date_errors() {
        let extractors = extractors([(
            "statement_date",
            Extractor {
                from: "pdf.text".to_string(),
                regex: r"Closing Date\s+(\S+)".to_string(),
                format: Some("%m/%d/%Y".to_string()),
            },
        )]);
        let facts = facts([("pdf.text", "Closing Date nope".to_string())]);

        let error = extract_variables(&extractors, &facts).unwrap_err();

        assert!(matches!(error, RenderError::DateParse { .. }));
    }

    #[test]
    fn renders_template_with_strict_variables() {
        let variables = vars([("statement_date", "2026-04-15")]);

        let rendered = render_template("{{ statement_date }} - Amex.pdf", &variables).unwrap();

        assert_eq!(rendered, "2026-04-15 - Amex.pdf");
    }

    #[test]
    fn date_filter_formats_iso_date() {
        let variables = vars([("statement_date", "2026-04-15")]);

        let rendered = render_template("{{ statement_date | date('%Y') }}", &variables).unwrap();

        assert_eq!(rendered, "2026");
    }

    #[test]
    fn date_filter_rejects_non_iso_input() {
        let variables = vars([("statement_date", "04/15/2026")]);

        let error = render_template("{{ statement_date | date('%Y') }}", &variables).unwrap_err();

        assert!(error.to_string().contains("YYYY-MM-DD"));
    }

    #[test]
    fn renders_design_style_date_destination() {
        let variables = vars([("statement_date", "2026-04-15")]);

        let path = render_safe_relative_path(
            "Amex/{{ statement_date | date('%Y') }}/{{ statement_date }} - Amex.pdf",
            &variables,
        )
        .unwrap();

        assert_eq!(path, PathBuf::from("Amex/2026/2026-04-15 - Amex.pdf"));
    }

    #[test]
    fn unknown_template_variable_errors() {
        let variables = vars([("statement_date", "2026-04-15")]);

        let error = render_template("{{ missing }}.pdf", &variables).unwrap_err();

        assert!(matches!(error, RenderError::Template(_)));
    }

    #[test]
    fn safe_relative_path_allows_subdirectories_from_template() {
        let variables = vars([("year", "2026"), ("filename", "statement.pdf")]);

        let path = render_safe_relative_path("{{ year }}/{{ filename }}", &variables).unwrap();

        assert_eq!(path, PathBuf::from("2026/statement.pdf"));
    }

    #[test]
    fn unsafe_variable_path_traversal_errors() {
        let variables = vars([("filename", "../../evil.pdf")]);

        let error = render_safe_relative_path("{{ filename }}", &variables).unwrap_err();

        assert!(matches!(error, RenderError::UnsafeVariable { .. }));
    }

    #[test]
    fn unsafe_variable_backslash_errors() {
        let variables = vars([("filename", r"..\evil.pdf")]);

        let error = render_safe_relative_path("{{ filename }}", &variables).unwrap_err();

        assert!(matches!(error, RenderError::UnsafeVariable { .. }));
    }

    #[test]
    fn literal_traversal_in_template_errors() {
        let variables = vars([("filename", "statement.pdf")]);

        let error = render_safe_relative_path("../{{ filename }}", &variables).unwrap_err();

        assert!(matches!(error, RenderError::UnsafePath { .. }));
    }

    #[test]
    fn absolute_template_path_errors() {
        let variables = vars([("filename", "statement.pdf")]);

        let error = render_safe_relative_path("/tmp/{{ filename }}", &variables).unwrap_err();

        assert!(matches!(error, RenderError::UnsafePath { .. }));
    }

    #[test]
    fn empty_rendered_path_errors() {
        let variables = vars([("filename", "statement.pdf")]);

        let error = render_safe_relative_path("   ", &variables).unwrap_err();

        assert!(matches!(error, RenderError::UnsafePath { .. }));
    }

    #[test]
    fn sanitizes_path_segment_when_callers_choose_sanitization() {
        assert_eq!(sanitize_path_segment("../../evil\0.pdf"), ".._.._evil_.pdf");
        assert_eq!(sanitize_path_segment(".."), "__");
    }

    fn extractors<const N: usize>(items: [(&str, Extractor); N]) -> IndexMap<String, Extractor> {
        items
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    fn facts<const N: usize>(items: [(&str, String); N]) -> FactStrings {
        items
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    fn vars<const N: usize>(items: [(&str, &str); N]) -> IndexMap<String, String> {
        items
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }
}
