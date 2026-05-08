use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;
use std::{iter::Peekable, str::CharIndices};
use thiserror::Error;

/// Provisional CEL-like evaluator for the MVP.
///
/// This is intentionally a tiny, swappable subset. Dotted identifiers such as
/// `file.ext` are treated as single fact keys, not as general member access.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Null,
    Bool(bool),
    String(String),
}

pub fn eval_bool(expr: &str, facts: &IndexMap<String, Value>) -> Result<bool, ExprError> {
    let ast = parse(expr)?;
    match ast.eval(facts)? {
        Value::Bool(value) => Ok(value),
        other => Err(ExprError::TypeError(format!(
            "expression produced {}, expected bool",
            other.type_name()
        ))),
    }
}

pub fn identifiers(expr: &str) -> Result<Vec<String>, ExprError> {
    let ast = parse(expr)?;
    let mut identifiers = Vec::new();
    ast.collect_identifiers(&mut identifiers);
    identifiers.sort();
    identifiers.dedup();
    Ok(identifiers)
}

fn parse(input: &str) -> Result<Expr, ExprError> {
    let tokens = Lexer::new(input).tokenize()?;
    let mut parser = Parser { tokens, pos: 0 };
    let expr = parser.parse_or()?;
    parser.expect(TokenTag::Eof)?;
    Ok(expr)
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Literal(Value),
    Identifier(String),
    Call {
        name: String,
        args: Vec<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
}

impl Expr {
    fn collect_identifiers(&self, out: &mut Vec<String>) {
        match self {
            Self::Literal(_) => {}
            Self::Identifier(name) => out.push(name.clone()),
            Self::Call { args, .. } => {
                for arg in args {
                    arg.collect_identifiers(out);
                }
            }
            Self::Binary { left, right, .. } => {
                left.collect_identifiers(out);
                right.collect_identifiers(out);
            }
        }
    }

    fn eval(&self, facts: &IndexMap<String, Value>) -> Result<Value, ExprError> {
        match self {
            Self::Literal(value) => Ok(value.clone()),
            Self::Identifier(name) => facts
                .get(name)
                .cloned()
                .ok_or_else(|| ExprError::UnknownIdentifier(name.clone())),
            Self::Call { name, args } => eval_call(name, args, facts),
            Self::Binary { op, left, right } => match op {
                BinaryOp::And => {
                    if !expect_bool(left.eval(facts)?)? {
                        return Ok(Value::Bool(false));
                    }
                    Ok(Value::Bool(expect_bool(right.eval(facts)?)?))
                }
                BinaryOp::Or => {
                    if expect_bool(left.eval(facts)?)? {
                        return Ok(Value::Bool(true));
                    }
                    Ok(Value::Bool(expect_bool(right.eval(facts)?)?))
                }
                BinaryOp::Eq => Ok(Value::Bool(left.eval(facts)? == right.eval(facts)?)),
                BinaryOp::Ne => Ok(Value::Bool(left.eval(facts)? != right.eval(facts)?)),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinaryOp {
    And,
    Or,
    Eq,
    Ne,
}

fn eval_call(
    name: &str,
    args: &[Expr],
    facts: &IndexMap<String, Value>,
) -> Result<Value, ExprError> {
    match name {
        "contains" => {
            expect_arity(name, args, 2)?;
            let haystack = args[0].eval(facts)?;
            let needle = expect_string(args[1].eval(facts)?)?;
            eval_nullable_string_predicate(name, haystack, |haystack| haystack.contains(&needle))
        }
        "matches" => {
            expect_arity(name, args, 2)?;
            let haystack = args[0].eval(facts)?;
            let pattern = expect_string(args[1].eval(facts)?)?;
            let regex = Regex::new(&pattern).map_err(|error| ExprError::BadRegex {
                pattern,
                message: error.to_string(),
            })?;
            eval_nullable_string_predicate(name, haystack, |haystack| regex.is_match(haystack))
        }
        "lower" => {
            expect_arity(name, args, 1)?;
            Ok(match args[0].eval(facts)? {
                Value::String(value) => Value::String(value.to_lowercase()),
                Value::Null => Value::Null,
                other => {
                    return Err(ExprError::TypeError(format!(
                        "lower argument must be string or null, got {}",
                        other.type_name()
                    )));
                }
            })
        }
        _ => Err(ExprError::UnknownFunction(name.to_string())),
    }
}

fn eval_nullable_string_predicate(
    function: &str,
    haystack: Value,
    predicate: impl FnOnce(&str) -> bool,
) -> Result<Value, ExprError> {
    Ok(Value::Bool(match haystack {
        Value::String(haystack) => predicate(&haystack),
        Value::Null => false,
        other => {
            return Err(ExprError::TypeError(format!(
                "{function} haystack must be string or null, got {}",
                other.type_name()
            )));
        }
    }))
}

fn expect_arity(name: &str, args: &[Expr], expected: usize) -> Result<(), ExprError> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(ExprError::ArityMismatch {
            function: name.to_string(),
            expected,
            actual: args.len(),
        })
    }
}

fn expect_bool(value: Value) -> Result<bool, ExprError> {
    match value {
        Value::Bool(value) => Ok(value),
        other => Err(ExprError::TypeError(format!(
            "expected bool, got {}",
            other.type_name()
        ))),
    }
}

fn expect_string(value: Value) -> Result<String, ExprError> {
    match value {
        Value::String(value) => Ok(value),
        other => Err(ExprError::TypeError(format!(
            "expected string, got {}",
            other.type_name()
        ))),
    }
}

impl Value {
    fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::String(_) => "string",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExprError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("unknown identifier {0:?}")]
    UnknownIdentifier(String),
    #[error("unknown function {0:?}")]
    UnknownFunction(String),
    #[error("function {function:?} expected {expected} argument(s), got {actual}")]
    ArityMismatch {
        function: String,
        expected: usize,
        actual: usize,
    },
    #[error("type error: {0}")]
    TypeError(String),
    #[error("bad regex {pattern:?}: {message}")]
    BadRegex { pattern: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Ident(String),
    String(String),
    Bool(bool),
    LParen,
    RParen,
    Comma,
    And,
    Or,
    Eq,
    Ne,
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenTag {
    Ident,
    String,
    Bool,
    LParen,
    RParen,
    Comma,
    And,
    Or,
    Eq,
    Ne,
    Eof,
}

impl TokenKind {
    fn tag(&self) -> TokenTag {
        match self {
            Self::Ident(_) => TokenTag::Ident,
            Self::String(_) => TokenTag::String,
            Self::Bool(_) => TokenTag::Bool,
            Self::LParen => TokenTag::LParen,
            Self::RParen => TokenTag::RParen,
            Self::Comma => TokenTag::Comma,
            Self::And => TokenTag::And,
            Self::Or => TokenTag::Or,
            Self::Eq => TokenTag::Eq,
            Self::Ne => TokenTag::Ne,
            Self::Eof => TokenTag::Eof,
        }
    }
}

struct Lexer<'a> {
    input: &'a str,
    chars: Peekable<CharIndices<'a>>,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.char_indices().peekable(),
        }
    }

    fn tokenize(mut self) -> Result<Vec<TokenKind>, ExprError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
            let Some(ch) = self.peek_char() else {
                break;
            };

            let token = match ch {
                '(' => {
                    self.next_char();
                    TokenKind::LParen
                }
                ')' => {
                    self.next_char();
                    TokenKind::RParen
                }
                ',' => {
                    self.next_char();
                    TokenKind::Comma
                }
                '&' if self.peek_next_char() == Some('&') => {
                    self.next_char();
                    self.next_char();
                    TokenKind::And
                }
                '|' if self.peek_next_char() == Some('|') => {
                    self.next_char();
                    self.next_char();
                    TokenKind::Or
                }
                '=' if self.peek_next_char() == Some('=') => {
                    self.next_char();
                    self.next_char();
                    TokenKind::Eq
                }
                '!' if self.peek_next_char() == Some('=') => {
                    self.next_char();
                    self.next_char();
                    TokenKind::Ne
                }
                '"' => TokenKind::String(self.string_literal()?),
                ch if is_ident_start(ch) => self.identifier(),
                _ => {
                    return Err(ExprError::Parse(format!(
                        "unexpected character {:?} at byte {} in {:?}",
                        ch,
                        self.byte_pos(),
                        self.input
                    )));
                }
            };
            tokens.push(token);
        }
        tokens.push(TokenKind::Eof);
        Ok(tokens)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek_char(), Some(ch) if ch.is_whitespace()) {
            self.next_char();
        }
    }

    fn string_literal(&mut self) -> Result<String, ExprError> {
        self.next_char();
        let mut value = String::new();

        while let Some(ch) = self.peek_char() {
            self.next_char();
            match ch {
                '"' => return Ok(value),
                '\\' => {
                    let escaped = self.peek_char().ok_or_else(|| {
                        ExprError::Parse("unterminated escape sequence in string".to_string())
                    })?;
                    self.next_char();
                    value.push(match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '"' => '"',
                        '\\' => '\\',
                        other => {
                            return Err(ExprError::Parse(format!(
                                "unsupported escape sequence \\{other}"
                            )));
                        }
                    });
                }
                other => value.push(other),
            }
        }

        Err(ExprError::Parse("unterminated string literal".to_string()))
    }

    fn identifier(&mut self) -> TokenKind {
        let start = self.byte_pos();
        self.next_char();
        while matches!(self.peek_char(), Some(ch) if is_ident_continue(ch)) {
            self.next_char();
        }
        let ident = self.input[start..self.byte_pos()].to_string();
        match ident.as_str() {
            "true" => TokenKind::Bool(true),
            "false" => TokenKind::Bool(false),
            _ => TokenKind::Ident(ident),
        }
    }

    fn peek_char(&mut self) -> Option<char> {
        self.chars.peek().map(|(_, ch)| *ch)
    }

    fn peek_next_char(&self) -> Option<char> {
        // The cloned iterator still includes the currently peeked char, so nth(1) is next.
        self.chars.clone().nth(1).map(|(_, ch)| ch)
    }

    fn next_char(&mut self) -> Option<(usize, char)> {
        self.chars.next()
    }

    fn byte_pos(&mut self) -> usize {
        self.chars
            .peek()
            .map(|(byte_pos, _)| *byte_pos)
            .unwrap_or(self.input.len())
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch == '.' || ch.is_ascii_alphanumeric()
}

struct Parser {
    tokens: Vec<TokenKind>,
    pos: usize,
}

impl Parser {
    fn parse_or(&mut self) -> Result<Expr, ExprError> {
        let mut expr = self.parse_and()?;
        while self.consume(TokenTag::Or) {
            expr = Expr::Binary {
                op: BinaryOp::Or,
                left: Box::new(expr),
                right: Box::new(self.parse_and()?),
            };
        }
        Ok(expr)
    }

    fn parse_and(&mut self) -> Result<Expr, ExprError> {
        let mut expr = self.parse_equality()?;
        while self.consume(TokenTag::And) {
            expr = Expr::Binary {
                op: BinaryOp::And,
                left: Box::new(expr),
                right: Box::new(self.parse_equality()?),
            };
        }
        Ok(expr)
    }

    fn parse_equality(&mut self) -> Result<Expr, ExprError> {
        let mut expr = self.parse_primary()?;
        loop {
            let op = if self.consume(TokenTag::Eq) {
                BinaryOp::Eq
            } else if self.consume(TokenTag::Ne) {
                BinaryOp::Ne
            } else {
                break;
            };
            expr = Expr::Binary {
                op,
                left: Box::new(expr),
                right: Box::new(self.parse_primary()?),
            };
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, ExprError> {
        match self.advance() {
            TokenKind::String(value) => Ok(Expr::Literal(Value::String(value))),
            TokenKind::Bool(value) => Ok(Expr::Literal(Value::Bool(value))),
            TokenKind::Ident(name) if self.consume(TokenTag::LParen) => {
                let mut args = Vec::new();
                if !self.consume(TokenTag::RParen) {
                    loop {
                        args.push(self.parse_or()?);
                        if self.consume(TokenTag::RParen) {
                            break;
                        }
                        self.expect(TokenTag::Comma)?;
                    }
                }
                Ok(Expr::Call { name, args })
            }
            TokenKind::Ident(name) => Ok(Expr::Identifier(name)),
            TokenKind::LParen => {
                let expr = self.parse_or()?;
                self.expect(TokenTag::RParen)?;
                Ok(expr)
            }
            other => Err(ExprError::Parse(format!(
                "expected expression, got {}",
                token_name(&other)
            ))),
        }
    }

    fn consume(&mut self, expected: TokenTag) -> bool {
        if self.peek().tag() == expected {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, expected: TokenTag) -> Result<(), ExprError> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(ExprError::Parse(format!(
                "expected {}, got {}",
                token_tag_name(expected),
                token_name(self.peek())
            )))
        }
    }

    fn advance(&mut self) -> TokenKind {
        let token = self.peek().clone();
        self.pos += 1;
        token
    }

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos]
    }
}

fn token_name(token: &TokenKind) -> &'static str {
    token_tag_name(token.tag())
}

fn token_tag_name(token: TokenTag) -> &'static str {
    match token {
        TokenTag::Ident => "identifier",
        TokenTag::String => "string",
        TokenTag::Bool => "bool",
        TokenTag::LParen => "(",
        TokenTag::RParen => ")",
        TokenTag::Comma => ",",
        TokenTag::And => "&&",
        TokenTag::Or => "||",
        TokenTag::Eq => "==",
        TokenTag::Ne => "!=",
        TokenTag::Eof => "end of input",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::facts;

    #[test]
    fn evaluates_design_style_expression() {
        let facts = facts([
            ("file.ext", Value::String(".pdf".to_string())),
            (
                "pdf.text",
                Value::String("American Express Statement Closing Date".to_string()),
            ),
        ]);

        assert!(
            eval_bool(
                r#"file.ext == ".pdf" && contains(pdf.text, "American Express")"#,
                &facts
            )
            .unwrap()
        );
    }

    #[test]
    fn respects_boolean_precedence() {
        let facts = facts([
            ("a", Value::Bool(true)),
            ("b", Value::Bool(false)),
            ("c", Value::Bool(false)),
        ]);

        assert!(eval_bool("a || b && c", &facts).unwrap());
        assert!(!eval_bool("(a || b) && c", &facts).unwrap());
    }

    #[test]
    fn short_circuits_boolean_operators() {
        let facts = facts([("file.ext", Value::String(".txt".to_string()))]);

        assert!(
            !eval_bool(
                r#"file.ext == ".pdf" && unknown_function(pdf.text)"#,
                &facts
            )
            .unwrap()
        );
        assert!(
            eval_bool(
                r#"file.ext == ".txt" || unknown_function(pdf.text)"#,
                &facts
            )
            .unwrap()
        );
    }

    #[test]
    fn null_facts_make_contains_and_matches_false() {
        let facts = facts([("pdf.text", Value::Null)]);

        assert!(!eval_bool(r#"contains(pdf.text, "American")"#, &facts).unwrap());
        assert!(!eval_bool(r#"matches(pdf.text, "American")"#, &facts).unwrap());
    }

    #[test]
    fn lower_preserves_null() {
        let facts = facts([("pdf.title", Value::Null)]);

        assert!(!eval_bool(r#"lower(pdf.title) == """#, &facts).unwrap());
    }

    #[test]
    fn matches_reports_invalid_regex() {
        let facts = facts([("pdf.text", Value::String("text".to_string()))]);

        let error = eval_bool(r#"matches(pdf.text, "[")"#, &facts).unwrap_err();

        assert!(matches!(error, ExprError::BadRegex { .. }));
    }

    #[test]
    fn unknown_identifier_is_an_error() {
        let facts = IndexMap::new();

        let error = eval_bool(r#"contains(pdf.txt, "x")"#, &facts).unwrap_err();

        assert_eq!(error, ExprError::UnknownIdentifier("pdf.txt".to_string()));
    }

    #[test]
    fn unknown_function_is_an_error() {
        let facts = facts([("pdf.text", Value::String("text".to_string()))]);

        let error = eval_bool("starts_with(pdf.text, \"t\")", &facts).unwrap_err();

        assert_eq!(error, ExprError::UnknownFunction("starts_with".to_string()));
    }

    #[test]
    fn parse_errors_report_utf8_byte_offsets() {
        let error = eval_bool("\u{2003}@", &IndexMap::new()).unwrap_err();

        assert!(matches!(
            error,
            ExprError::Parse(message) if message.contains("at byte 3")
        ));
    }

    #[test]
    fn extracts_identifiers_from_expression() {
        let ids =
            identifiers(r#"file.ext == ".pdf" && contains(pdf.text, "American Express")"#).unwrap();

        assert_eq!(ids, vec!["file.ext".to_string(), "pdf.text".to_string()]);
    }
}
