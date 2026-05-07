use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use chrono::{Datelike, Local, NaiveDate};
use indexmap::IndexMap;
use minijinja::{Environment, Error as MiniJinjaError, ErrorKind, UndefinedBehavior};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use thiserror::Error;

use crate::config::{DateExtractor, DateScope, DateWindow, Extractor, RegexExtractor};

pub type FactStrings = IndexMap<String, String>;

pub fn extract_variables(
    extractors: &IndexMap<String, Extractor>,
    facts: &FactStrings,
) -> Result<IndexMap<String, String>, RenderError> {
    Ok(extract_variables_with_diagnostics(extractors, facts)?.variables)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionResult {
    pub variables: IndexMap<String, String>,
    pub diagnostics: Vec<ExtractionDiagnostic>,
}

pub fn extract_variables_with_diagnostics(
    extractors: &IndexMap<String, Extractor>,
    facts: &FactStrings,
) -> Result<ExtractionResult, RenderError> {
    extract_variables_with_reference_year(extractors, facts, current_year())
}

pub fn extract_variables_with_reference_year(
    extractors: &IndexMap<String, Extractor>,
    facts: &FactStrings,
    reference_year: i32,
) -> Result<ExtractionResult, RenderError> {
    let mut variables = IndexMap::new();
    let mut diagnostics = Vec::new();

    for (name, extractor) in extractors {
        let source = facts
            .get(extractor.from())
            .ok_or_else(|| RenderError::MissingFact {
                variable: name.clone(),
                fact: extractor.from().to_string(),
            })?;
        let value = match extractor {
            Extractor::Regex(extractor) => extract_regex_variable(name, extractor, source)?,
            Extractor::Date(extractor) => {
                let extracted = extract_date_variable(name, extractor, source, reference_year)?;
                diagnostics.push(extracted.diagnostic);
                extracted.value
            }
        };

        variables.insert(name.clone(), value);
    }

    Ok(ExtractionResult {
        variables,
        diagnostics,
    })
}

fn current_year() -> i32 {
    Local::now().year()
}

fn extract_regex_variable(
    name: &str,
    extractor: &RegexExtractor,
    source: &str,
) -> Result<String, RenderError> {
    let regex = Regex::new(&extractor.regex).map_err(|error| RenderError::BadRegex {
        variable: name.to_string(),
        regex: extractor.regex.clone(),
        message: error.to_string(),
    })?;
    let captures = regex.captures(source).ok_or_else(|| RenderError::NoMatch {
        variable: name.to_string(),
        fact: extractor.from.clone(),
    })?;

    let captured = captures
        .name(name)
        .or_else(|| captures.get(1))
        .or_else(|| captures.get(0))
        .expect("captures always include full match")
        .as_str();
    match &extractor.format {
        Some(format) => parse_date_variable(name, captured, format),
        None => Ok(captured.to_string()),
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtractionDiagnostic {
    pub variable: String,
    pub kind: String,
    pub fact: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<DateCandidateDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<DateCandidateDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DateCandidateDiagnostic {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

struct DateExtraction {
    value: String,
    diagnostic: ExtractionDiagnostic,
}

#[derive(Debug, Clone)]
struct ParsedCandidate {
    start: usize,
    text: String,
    date: NaiveDate,
}

#[derive(Debug, Clone)]
struct RejectedCandidate {
    text: String,
    reason: String,
}

fn extract_date_variable(
    name: &str,
    extractor: &DateExtractor,
    source: &str,
    reference_year: i32,
) -> Result<DateExtraction, RenderError> {
    let window = effective_window(extractor);
    if extractor.scope == Some(DateScope::Document) {
        let (candidates, rejected) = date_candidates(source, extractor, reference_year);
        return select_unique_date(
            name,
            extractor,
            None,
            None,
            Some("document".to_string()),
            candidates,
            rejected,
        );
    }

    let label = extractor.after.as_ref().or(extractor.near.as_ref()).expect(
        "config validation requires date extractor to specify after, near, or scope: document",
    );
    let label_regex = label_regex(label).map_err(|error| RenderError::BadRegex {
        variable: name.to_string(),
        regex: label.clone(),
        message: error.to_string(),
    })?;

    let mut saw_label = false;
    let mut saw_rejected: Option<RejectedCandidate> = None;
    for captures in label_regex.captures_iter(source) {
        let matched = captures.name("label").expect("label group exists");
        saw_label = true;
        let range = match extractor.after {
            Some(_) => window_after(source, matched.end(), window),
            None => window_near(source, matched.start(), matched.end(), window),
        };
        let window_text = &source[range.clone()];
        let (mut candidates, rejected) = date_candidates(window_text, extractor, reference_year);
        for candidate in &mut candidates {
            candidate.start += range.start;
        }
        if saw_rejected.is_none() {
            saw_rejected = rejected.first().cloned();
        }
        if candidates.is_empty() {
            continue;
        }
        let window_name = window.name();
        return if extractor.after.is_some() {
            let selected = candidates
                .iter()
                .min_by_key(|candidate| candidate.start)
                .expect("candidates is not empty")
                .clone();
            Ok(DateExtraction {
                value: iso_date(selected.date),
                diagnostic: diagnostic(
                    name,
                    extractor,
                    Some(label.clone()),
                    Some(matched.as_str().to_string()),
                    Some(window_name),
                    Some(selected),
                    rejected,
                ),
            })
        } else {
            select_unique_date(
                name,
                extractor,
                Some(label.clone()),
                Some(matched.as_str().to_string()),
                Some(window_name),
                candidates,
                rejected,
            )
        };
    }

    if !saw_label {
        return Err(RenderError::NoMatch {
            variable: name.to_string(),
            fact: extractor.from.clone(),
        });
    }
    if let Some(rejected) = saw_rejected {
        return Err(rejected_candidate_error(name, extractor, &rejected));
    }
    Err(RenderError::NoMatch {
        variable: name.to_string(),
        fact: extractor.from.clone(),
    })
}

fn select_unique_date(
    name: &str,
    extractor: &DateExtractor,
    label: Option<String>,
    matched_label: Option<String>,
    window: Option<String>,
    candidates: Vec<ParsedCandidate>,
    rejected: Vec<RejectedCandidate>,
) -> Result<DateExtraction, RenderError> {
    if candidates.is_empty() {
        if let Some(rejected) = rejected.first() {
            return Err(rejected_candidate_error(name, extractor, rejected));
        }
        return Err(RenderError::NoMatch {
            variable: name.to_string(),
            fact: extractor.from.clone(),
        });
    }

    let mut dates: Vec<NaiveDate> = Vec::new();
    for candidate in &candidates {
        if !dates.contains(&candidate.date) {
            dates.push(candidate.date);
        }
    }
    if dates.len() > 1 {
        let details = candidates
            .iter()
            .map(|candidate| format!("{} -> {}", candidate.text, iso_date(candidate.date)))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(RenderError::DateAmbiguous {
            variable: name.to_string(),
            fact: extractor.from.clone(),
            details,
        });
    }

    let selected = candidates[0].clone();
    Ok(DateExtraction {
        value: iso_date(selected.date),
        diagnostic: diagnostic(
            name,
            extractor,
            label,
            matched_label,
            window,
            Some(selected),
            rejected,
        ),
    })
}

fn rejected_candidate_error(
    name: &str,
    extractor: &DateExtractor,
    rejected: &RejectedCandidate,
) -> RenderError {
    if rejected.reason.contains("ambiguous date text") {
        RenderError::DateAmbiguous {
            variable: name.to_string(),
            fact: extractor.from.clone(),
            details: format!("{}: {}", rejected.text, rejected.reason),
        }
    } else {
        RenderError::DateParse {
            variable: name.to_string(),
            value: rejected.text.clone(),
            format: extractor.formats.join(", "),
            message: rejected.reason.clone(),
        }
    }
}

fn diagnostic(
    name: &str,
    extractor: &DateExtractor,
    label: Option<String>,
    matched_label: Option<String>,
    window: Option<String>,
    selected: Option<ParsedCandidate>,
    rejected: Vec<RejectedCandidate>,
) -> ExtractionDiagnostic {
    ExtractionDiagnostic {
        variable: name.to_string(),
        kind: "date".to_string(),
        fact: extractor.from.clone(),
        label,
        matched_label,
        window,
        selected: selected.map(|candidate| DateCandidateDiagnostic {
            text: candidate.text,
            date: Some(iso_date(candidate.date)),
            reason: None,
        }),
        rejected: rejected
            .into_iter()
            .map(|candidate| DateCandidateDiagnostic {
                text: candidate.text,
                date: None,
                reason: Some(candidate.reason),
            })
            .collect(),
    }
}

fn label_regex(label: &str) -> Result<Regex, regex::Error> {
    let trimmed = label
        .trim()
        .trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, ':' | '.' | '-' | '#'));
    let mut pattern = String::from(r"(?i)(?:^|[^[:alnum:]])(?P<label>");
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            while chars.peek().is_some_and(|next| next.is_whitespace()) {
                chars.next();
            }
            pattern.push_str(r"\s+");
        } else if matches!(ch, ':' | '.' | '-' | '#') {
            pattern.push_str(r"[^\S\n]*");
            pattern.push_str(&regex::escape(&ch.to_string()));
            pattern.push_str(r"[^\S\n]*");
        } else {
            pattern.push_str(&regex::escape(&ch.to_string()));
        }
    }
    pattern.push_str(r"[^\S\n]*[:.\-#]*)");
    Regex::new(&pattern)
}

fn effective_window(extractor: &DateExtractor) -> DateWindow {
    extractor.window.unwrap_or_else(|| {
        if extractor.after.is_some() {
            DateWindow::NextLine
        } else {
            DateWindow::SameLine
        }
    })
}

impl DateWindow {
    fn name(self) -> String {
        match self {
            Self::SameLine => "same_line".to_string(),
            Self::NextLine => "next_line".to_string(),
            Self::Paragraph => "paragraph".to_string(),
            Self::Chars(count) => format!("chars:{count}"),
        }
    }
}

fn window_after(source: &str, label_end: usize, window: DateWindow) -> std::ops::Range<usize> {
    match window {
        DateWindow::SameLine => label_end..line_bounds(source, label_end).end,
        DateWindow::NextLine => label_end..next_line_window_end(source, label_end),
        DateWindow::Paragraph => label_end..paragraph_bounds(source, label_end).end,
        DateWindow::Chars(count) => label_end..char_window_end(source, label_end, count),
    }
}

fn window_near(
    source: &str,
    label_start: usize,
    label_end: usize,
    window: DateWindow,
) -> std::ops::Range<usize> {
    match window {
        DateWindow::SameLine => line_bounds(source, label_start),
        DateWindow::NextLine => {
            line_bounds(source, label_start).start..next_line_window_end(source, label_end)
        }
        DateWindow::Paragraph => paragraph_bounds(source, label_start),
        DateWindow::Chars(count) => label_end..char_window_end(source, label_end, count),
    }
}

fn line_bounds(source: &str, index: usize) -> std::ops::Range<usize> {
    let start = source[..index].rfind('\n').map_or(0, |pos| pos + 1);
    let end = source[index..]
        .find('\n')
        .map_or(source.len(), |pos| index + pos);
    start..end
}

fn next_line_window_end(source: &str, index: usize) -> usize {
    let current = line_bounds(source, index);
    let mut pos = if current.end < source.len() {
        current.end + 1
    } else {
        current.end
    };
    while pos < source.len() {
        let line = line_bounds(source, pos);
        if !source[line.clone()].trim().is_empty() {
            return line.end;
        }
        pos = if line.end < source.len() {
            line.end + 1
        } else {
            line.end
        };
    }
    current.end
}

fn paragraph_bounds(source: &str, index: usize) -> std::ops::Range<usize> {
    let start = source[..index]
        .rfind("\n\n")
        .map_or(0, |pos| pos + "\n\n".len());
    let newline_end = source[index..]
        .find("\n\n")
        .map_or(source.len(), |pos| index + pos);
    let form_end = source[index..]
        .find('\u{000c}')
        .map_or(source.len(), |pos| index + pos);
    start..newline_end.min(form_end)
}

fn char_window_end(source: &str, start: usize, count: usize) -> usize {
    source[start..]
        .char_indices()
        .nth(count)
        .map_or(source.len(), |(offset, _)| start + offset)
}

fn date_candidates(
    text: &str,
    extractor: &DateExtractor,
    reference_year: i32,
) -> (Vec<ParsedCandidate>, Vec<RejectedCandidate>) {
    let regex = candidate_regex(&extractor.formats);
    let min_year = extractor.min_year.unwrap_or(1990);
    let max_year = extractor.max_year.unwrap_or(reference_year + 1);
    let mut candidates = Vec::new();
    let mut rejected = Vec::new();

    for matched in regex.find_iter(text) {
        if !candidate_has_safe_boundaries(text, matched.start(), matched.end()) {
            continue;
        }
        let value = matched.as_str();
        match parse_candidate(value, &extractor.formats) {
            Ok(date) if date.year() < min_year || date.year() > max_year => {
                rejected.push(RejectedCandidate {
                    text: value.to_string(),
                    reason: format!(
                        "year {} outside allowed range {min_year}..={max_year}",
                        date.year()
                    ),
                });
            }
            Ok(date) => candidates.push(ParsedCandidate {
                start: matched.start(),
                text: value.to_string(),
                date,
            }),
            Err(reason) => rejected.push(RejectedCandidate {
                text: value.to_string(),
                reason,
            }),
        }
    }

    (dedupe_candidates(candidates), rejected)
}

fn candidate_regex(formats: &[String]) -> Regex {
    let mut pieces = Vec::new();
    if formats.iter().any(|format| {
        format.contains('/')
            && format.contains("%Y")
            && (format.contains("%m") || format.contains("%-m"))
            && (format.contains("%d") || format.contains("%-d"))
    }) {
        pieces.push(r"\d{1,2}/\d{1,2}/\d{4}".to_string());
    }
    if formats.iter().any(|format| {
        format.contains('-')
            && format.contains("%Y")
            && (format.contains("%m") || format.contains("%-m"))
            && (format.contains("%d") || format.contains("%-d"))
    }) {
        pieces.push(r"\d{4}-\d{1,2}-\d{1,2}".to_string());
    }
    if formats
        .iter()
        .any(|format| format.contains("%B") || format.contains("%b"))
    {
        pieces.push(r"[A-Za-z]{3,9}\s+\d{1,2},\s+\d{4}".to_string());
    }
    if formats.iter().any(|format| format == "%Y%m%d") {
        pieces.push(r"\d{8}".to_string());
    }
    if pieces.is_empty() {
        pieces.push(r"\b\B".to_string());
    }
    Regex::new(&format!(r"(?i)({})", pieces.join("|"))).expect("candidate regex is valid")
}

fn candidate_has_safe_boundaries(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    before.is_none_or(|ch| !ch.is_ascii_alphanumeric())
        && after.is_none_or(|ch| !ch.is_ascii_alphanumeric())
}

fn parse_candidate(value: &str, formats: &[String]) -> Result<NaiveDate, String> {
    let mut parsed = Vec::new();
    let mut errors = Vec::new();
    for format in formats {
        match NaiveDate::parse_from_str(value, format) {
            Ok(date) => {
                if !parsed.contains(&date) {
                    parsed.push(date);
                }
            }
            Err(error) => errors.push(format!("{format:?}: {error}")),
        }
    }
    match parsed.as_slice() {
        [date] => Ok(*date),
        [] => Err(errors.join("; ")),
        dates => Err(format!(
            "ambiguous date text parsed as {}",
            dates
                .iter()
                .map(|date| iso_date(*date))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn dedupe_candidates(candidates: Vec<ParsedCandidate>) -> Vec<ParsedCandidate> {
    let mut deduped = Vec::new();
    for candidate in candidates {
        if !deduped.iter().any(|existing: &ParsedCandidate| {
            existing.start == candidate.start && existing.text == candidate.text
        }) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn iso_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

static TEMPLATE_ENV: OnceLock<Environment<'static>> = OnceLock::new();

fn template_environment() -> &'static Environment<'static> {
    TEMPLATE_ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.add_filter("date", date_filter);
        env
    })
}

pub fn render_template(
    template: &str,
    variables: &IndexMap<String, String>,
) -> Result<String, RenderError> {
    let context = build_template_context(variables)?;
    let tmpl = template_environment()
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
    #[error("date extractor {variable:?} found ambiguous dates in fact {fact:?}: {details}")]
    DateAmbiguous {
        variable: String,
        fact: String,
        details: String,
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
    use crate::test_support::{extractors, fact_strings, vars};

    #[test]
    fn extracts_first_capture_group() {
        let extractors = extractors([(
            "statement_date",
            Extractor::Regex(RegexExtractor {
                from: "pdf.text".to_string(),
                regex: r"Closing Date\s+(\d{2}/\d{2}/\d{4})".to_string(),
                format: None,
            }),
        )]);
        let facts = fact_strings([("pdf.text", "American Express Closing Date 04/15/2026")]);

        let variables = extract_variables(&extractors, &facts).unwrap();

        assert_eq!(variables["statement_date"], "04/15/2026");
    }

    #[test]
    fn prefers_named_capture_matching_variable_name() {
        let extractors = extractors([(
            "statement_date",
            Extractor::Regex(RegexExtractor {
                from: "pdf.text".to_string(),
                regex: r"Closing Date\s+(?P<statement_date>\d{4}-\d{2}-\d{2})".to_string(),
                format: None,
            }),
        )]);
        let facts = fact_strings([("pdf.text", "Closing Date 2026-04-15")]);

        let variables = extract_variables(&extractors, &facts).unwrap();

        assert_eq!(variables["statement_date"], "2026-04-15");
    }

    #[test]
    fn missing_fact_errors() {
        let extractors = extractors([(
            "statement_date",
            Extractor::Regex(RegexExtractor {
                from: "pdf.text".to_string(),
                regex: "date".to_string(),
                format: None,
            }),
        )]);

        let error = extract_variables(&extractors, &FactStrings::new()).unwrap_err();

        assert!(matches!(error, RenderError::MissingFact { .. }));
    }

    #[test]
    fn no_regex_match_errors() {
        let extractors = extractors([(
            "statement_date",
            Extractor::Regex(RegexExtractor {
                from: "pdf.text".to_string(),
                regex: "Closing Date".to_string(),
                format: None,
            }),
        )]);
        let facts = fact_strings([("pdf.text", "no date here")]);

        let error = extract_variables(&extractors, &facts).unwrap_err();

        assert!(matches!(error, RenderError::NoMatch { .. }));
    }

    #[test]
    fn extractor_format_parses_date_to_iso() {
        let extractors = extractors([(
            "statement_date",
            Extractor::Regex(RegexExtractor {
                from: "pdf.text".to_string(),
                regex: r"Closing Date\s+(\d{2}/\d{2}/\d{4})".to_string(),
                format: Some("%m/%d/%Y".to_string()),
            }),
        )]);
        let facts = fact_strings([("pdf.text", "Closing Date 04/15/2026")]);

        let variables = extract_variables(&extractors, &facts).unwrap();

        assert_eq!(variables["statement_date"], "2026-04-15");
    }

    #[test]
    fn invalid_extractor_date_errors() {
        let extractors = extractors([(
            "statement_date",
            Extractor::Regex(RegexExtractor {
                from: "pdf.text".to_string(),
                regex: r"Closing Date\s+(\S+)".to_string(),
                format: Some("%m/%d/%Y".to_string()),
            }),
        )]);
        let facts = fact_strings([("pdf.text", "Closing Date nope")]);

        let error = extract_variables(&extractors, &facts).unwrap_err();

        assert!(matches!(error, RenderError::DateParse { .. }));
    }

    #[test]
    fn date_extractor_after_matches_normalized_label_on_same_line() {
        let extractors = extractors([date_extractor(
            "statement_date",
            DateExtractor {
                from: "pdf.text".to_string(),
                after: Some("Statement Date:".to_string()),
                near: None,
                scope: None,
                window: None,
                formats: vec!["%m/%d/%Y".to_string()],
                min_year: None,
                max_year: None,
            },
        )]);
        let facts = fact_strings([("pdf.text", "Statement   date : 04/15/2026")]);

        let result = extract_variables_with_reference_year(&extractors, &facts, 2026).unwrap();

        assert_eq!(result.variables["statement_date"], "2026-04-15");
        assert_eq!(
            result.diagnostics[0].matched_label.as_deref(),
            Some("Statement   date :")
        );
    }

    #[test]
    fn date_extractor_after_uses_next_non_empty_line_by_default() {
        let extractors = extractors([date_extractor(
            "stmt_date",
            DateExtractor {
                from: "pdf.text".to_string(),
                after: Some("Close date".to_string()),
                near: None,
                scope: None,
                window: None,
                formats: vec!["%m/%d/%Y".to_string()],
                min_year: None,
                max_year: None,
            },
        )]);
        let facts = fact_strings([("pdf.text", "Close date:\n\n4/5/2026\nDue date: 5/5/2026")]);

        let result = extract_variables_with_reference_year(&extractors, &facts, 2026).unwrap();

        assert_eq!(result.variables["stmt_date"], "2026-04-05");
    }

    #[test]
    fn date_extractor_near_allows_date_before_or_after_label_on_same_line() {
        let extractors = extractors([date_extractor(
            "bill_date",
            DateExtractor {
                from: "pdf.text".to_string(),
                after: None,
                near: Some("Bill Date".to_string()),
                scope: None,
                window: None,
                formats: vec!["%B %-d, %Y".to_string(), "%b %-d, %Y".to_string()],
                min_year: None,
                max_year: None,
            },
        )]);
        let facts = fact_strings([("pdf.text", "April 15, 2026    Bill Date")]);

        let result = extract_variables_with_reference_year(&extractors, &facts, 2026).unwrap();

        assert_eq!(result.variables["bill_date"], "2026-04-15");
    }

    #[test]
    fn date_extractor_document_scope_requires_one_distinct_date() {
        let extractors = extractors([date_extractor(
            "pay_date",
            DateExtractor {
                from: "pdf.text".to_string(),
                after: None,
                near: None,
                scope: Some(DateScope::Document),
                window: None,
                formats: vec!["%Y-%m-%d".to_string()],
                min_year: None,
                max_year: None,
            },
        )]);
        let facts = fact_strings([("pdf.text", "Pay period 2026-04-15 and due 2026-05-15")]);

        let error = extract_variables_with_reference_year(&extractors, &facts, 2026).unwrap_err();

        assert!(matches!(error, RenderError::DateAmbiguous { .. }));
    }

    #[test]
    fn date_extractor_ignores_shapes_not_declared_by_formats() {
        let extractors = extractors([date_extractor(
            "statement_date",
            DateExtractor {
                from: "pdf.text".to_string(),
                after: Some("Statement Date".to_string()),
                near: None,
                scope: None,
                window: Some(DateWindow::Paragraph),
                formats: vec!["%Y-%m-%d".to_string()],
                min_year: None,
                max_year: None,
            },
        )]);
        let facts = fact_strings([(
            "pdf.text",
            "Statement Date:\nAccount reference 04/15/2026\nPosted 2026-04-16",
        )]);

        let result = extract_variables_with_reference_year(&extractors, &facts, 2026).unwrap();

        assert_eq!(result.variables["statement_date"], "2026-04-16");
    }

    #[test]
    fn date_extractor_rejects_ambiguous_format_parse() {
        let extractors = extractors([date_extractor(
            "statement_date",
            DateExtractor {
                from: "pdf.text".to_string(),
                after: Some("Statement Date".to_string()),
                near: None,
                scope: None,
                window: None,
                formats: vec!["%m/%d/%Y".to_string(), "%d/%m/%Y".to_string()],
                min_year: None,
                max_year: None,
            },
        )]);
        let facts = fact_strings([("pdf.text", "Statement Date: 04/05/2026")]);

        let error = extract_variables_with_reference_year(&extractors, &facts, 2026).unwrap_err();

        assert!(matches!(error, RenderError::DateAmbiguous { .. }));
    }

    #[test]
    fn compact_date_requires_format_opt_in_and_safe_boundaries() {
        let extractors = extractors([date_extractor(
            "statement_date",
            DateExtractor {
                from: "pdf.text".to_string(),
                after: Some("Statement Date".to_string()),
                near: None,
                scope: None,
                window: Some(DateWindow::SameLine),
                formats: vec!["%Y%m%d".to_string()],
                min_year: None,
                max_year: None,
            },
        )]);
        let facts = fact_strings([("pdf.text", "Statement Date: x20260415 20260415")]);

        let result = extract_variables_with_reference_year(&extractors, &facts, 2026).unwrap();

        assert_eq!(result.variables["statement_date"], "2026-04-15");
    }

    #[test]
    fn invalid_calendar_date_and_year_range_error() {
        let extractors = extractors([date_extractor(
            "statement_date",
            DateExtractor {
                from: "pdf.text".to_string(),
                after: Some("Statement Date".to_string()),
                near: None,
                scope: None,
                window: None,
                formats: vec!["%m/%d/%Y".to_string()],
                min_year: Some(2020),
                max_year: Some(2025),
            },
        )]);
        let facts = fact_strings([("pdf.text", "Statement Date: 02/30/2026")]);
        let error = extract_variables_with_reference_year(&extractors, &facts, 2026).unwrap_err();
        assert!(matches!(error, RenderError::DateParse { .. }));

        let facts = fact_strings([("pdf.text", "Statement Date: 04/15/2026")]);
        let error = extract_variables_with_reference_year(&extractors, &facts, 2026).unwrap_err();
        assert!(error.to_string().contains("outside allowed range"));
    }

    #[test]
    fn fixture_hazel_rule_families_extract_label_dates() {
        let extractors = extractors([
            date_extractor(
                "statement_date",
                DateExtractor {
                    from: "pdf.text".to_string(),
                    after: Some("Statement Date:".to_string()),
                    near: None,
                    scope: None,
                    window: None,
                    formats: vec!["%m/%d/%Y".to_string()],
                    min_year: None,
                    max_year: None,
                },
            ),
            date_extractor(
                "stmt_date",
                DateExtractor {
                    from: "pdf.text".to_string(),
                    after: Some("Close date:".to_string()),
                    near: None,
                    scope: None,
                    window: None,
                    formats: vec!["%m/%d/%Y".to_string()],
                    min_year: None,
                    max_year: None,
                },
            ),
        ]);
        let facts = fact_strings([(
            "pdf.text",
            "Columbia Gas\nStatement Date: 04/15/2026\nDiscover Card\nClose date:\n04/30/2026",
        )]);

        let result = extract_variables_with_reference_year(&extractors, &facts, 2026).unwrap();

        assert_eq!(result.variables["statement_date"], "2026-04-15");
        assert_eq!(result.variables["stmt_date"], "2026-04-30");
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

    fn date_extractor(name: &str, extractor: DateExtractor) -> (&str, Extractor) {
        (name, Extractor::Date(extractor))
    }
}
