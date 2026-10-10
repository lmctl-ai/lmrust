//! GraphQL-style input form (lmts `graphql-form.ts`). A small documented subset:
//!
//! ```text
//! mutation { opName(paramA: "x", paramB: [1,2], paramC: true) { __typename } }
//! query { opName(paramA: "x") { __typename } }
//! { opName(paramA: "x") { __typename } }
//! ```
//!
//! The selection set is ignored; only the arguments are validated against the op schema.
//! Variables and named operations are rejected. Offsets count Unicode scalar values.

use crate::js;
use crate::synopsis::usage_line;
use crate::types::{IssueCode, OperationSchema, Outcome, Record, ValidationIssue};
use crate::validate::{validate_params, SchemaValidationError};
use serde_json::Value;

/// A parsed GraphQL input document.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GraphqlInputDocument {
    pub op: Option<String>,
    pub params: Record,
}

struct ParseError {
    message: String,
    offset: usize,
}

const NO_VARIABLES: &str = "variables and operation names are not supported; inline literal values";

fn fail<T>(message: impl Into<String>, offset: usize) -> Result<T, ParseError> {
    Err(ParseError {
        message: message.into(),
        offset,
    })
}

struct Parser {
    text: Vec<char>,
    pos: usize,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.text.get(self.pos).copied()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn starts_with(&self, word: &str) -> bool {
        let w: Vec<char> = word.chars().collect();
        self.text.len() >= self.pos + w.len() && self.text[self.pos..self.pos + w.len()] == w[..]
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if matches!(c, ' ' | '\t' | '\n' | '\r') {
                self.pos += 1;
            } else if c == '#' {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// `^[A-Za-z_][A-Za-z0-9_]*` at the cursor.
    fn name(&self) -> Option<String> {
        if !self.peek().is_some_and(is_name_start) {
            return None;
        }
        let mut end = self.pos;
        while end < self.text.len() && is_name_char(self.text[end]) {
            end += 1;
        }
        Some(self.text[self.pos..end].iter().collect())
    }

    /// Hex digits at the start of the next four characters, parsed like `parseInt(hex, 16)`.
    fn hex4(&self, start: usize) -> Option<u32> {
        let mut value: Option<u32> = None;
        for k in 0..4 {
            match self.text.get(start + k).and_then(|c| c.to_digit(16)) {
                Some(d) => value = Some(value.unwrap_or(0) * 16 + d),
                None => break,
            }
        }
        value
    }

    fn parse_string(&mut self) -> Result<String, ParseError> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '"' {
                self.pos += 1;
                return Ok(out);
            }
            if c == '\\' {
                let next = self.text.get(self.pos + 1).copied();
                let simple = match next {
                    Some('"') => Some('"'),
                    Some('\\') => Some('\\'),
                    Some('n') => Some('\n'),
                    Some('t') => Some('\t'),
                    Some('r') => Some('\r'),
                    Some('/') => Some('/'),
                    Some('b') => Some('\u{8}'),
                    Some('f') => Some('\u{c}'),
                    _ => None,
                };
                if let Some(ch) = simple {
                    out.push(ch);
                    self.pos += 2;
                    continue;
                }
                if next == Some('u') {
                    let Some(code) = self.hex4(self.pos + 2) else {
                        return fail("invalid \\u escape", self.pos);
                    };
                    self.pos += 6;
                    if (0xD800..0xDC00).contains(&code)
                        && self.peek() == Some('\\')
                        && self.text.get(self.pos + 1) == Some(&'u')
                    {
                        if let Some(low) = self.hex4(self.pos + 2).filter(|l| (0xDC00..0xE000).contains(l)) {
                            let combined = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            out.push(char::from_u32(combined).unwrap_or('\u{FFFD}'));
                            self.pos += 6;
                            continue;
                        }
                    }
                    // A lone surrogate has no UTF-8 form.
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    continue;
                }
                let shown = next.map(String::from).unwrap_or_else(|| "undefined".into());
                return fail(format!("unknown escape \\{shown}"), self.pos);
            }
            out.push(c);
            self.pos += 1;
        }
        fail("unterminated string literal", self.pos)
    }

    fn parse_number(&mut self) -> Result<Value, ParseError> {
        let rest: String = self.text[self.pos..].iter().collect();
        let b = rest.as_bytes();
        let mut i = usize::from(b.first() == Some(&b'-'));
        let digits = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == digits {
            return fail("invalid number", self.pos);
        }
        if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            let mut j = i + 1;
            if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
            }
        }
        let lexeme = &rest[..i];
        self.pos += i;
        match lexeme.parse::<f64>().ok().and_then(js::number_value) {
            Some(v) => Ok(v),
            // JavaScript would produce Infinity, which JSON cannot carry.
            None => fail("invalid number", self.pos - i),
        }
    }

    fn parse_array(&mut self) -> Result<Value, ParseError> {
        self.pos += 1;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Value::Array(out));
        }
        while !self.at_end() {
            out.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    self.pos += 1;
                    self.skip_ws();
                }
                Some(']') => {
                    self.pos += 1;
                    return Ok(Value::Array(out));
                }
                _ => return fail("expected , or ] in array", self.pos),
            }
        }
        fail("unterminated array", self.pos)
    }

    fn parse_object(&mut self) -> Result<Value, ParseError> {
        self.pos += 1;
        let mut out = Record::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Value::Object(out));
        }
        while !self.at_end() {
            self.skip_ws();
            if self.peek() == Some('$') {
                return fail(NO_VARIABLES, self.pos);
            }
            let Some(key) = self.name() else {
                return fail("expected object key", self.pos);
            };
            self.pos += key.chars().count();
            self.skip_ws();
            if self.peek() != Some(':') {
                return fail("expected : after object key", self.pos);
            }
            self.pos += 1;
            let v = self.parse_value()?;
            out.insert(key, v);
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {
                    self.pos += 1;
                    return Ok(Value::Object(out));
                }
                _ => return fail("expected , or } in object", self.pos),
            }
        }
        fail("unterminated object", self.pos)
    }

    fn parse_value(&mut self) -> Result<Value, ParseError> {
        self.skip_ws();
        match self.peek() {
            Some('$') => return fail(NO_VARIABLES, self.pos),
            Some('"') => return self.parse_string().map(Value::String),
            Some('[') => return self.parse_array(),
            Some('{') => return self.parse_object(),
            Some(c) if c == '-' || c.is_ascii_digit() => return self.parse_number(),
            _ => {}
        }
        for (word, value) in [
            ("true", Value::Bool(true)),
            ("false", Value::Bool(false)),
            ("null", Value::Null),
        ] {
            if self.starts_with(word) {
                self.pos += word.len();
                return Ok(value);
            }
        }
        if let Some(ident) = self.name() {
            self.pos += ident.chars().count();
            return Ok(Value::String(ident));
        }
        fail(format!("unexpected value at offset {}", self.pos), self.pos)
    }

    fn document(&mut self, parsed_op: &mut Option<String>) -> Result<GraphqlInputDocument, ParseError> {
        self.skip_ws();
        // Optional operation keyword: query | mutation | subscription (followed by a word boundary).
        for kw in ["query", "mutation", "subscription"] {
            if self.starts_with(kw) && !self.text.get(self.pos + kw.len()).copied().is_some_and(is_name_char) {
                self.pos += kw.len();
                self.skip_ws();
                if self.peek() != Some('{') {
                    return fail(NO_VARIABLES, self.pos);
                }
                break;
            }
        }
        if self.peek() != Some('{') {
            if self.peek() == Some('$') || self.peek().is_some_and(is_name_start) {
                return fail(NO_VARIABLES, self.pos);
            }
            return fail(format!("expected {{ at offset {}", self.pos), self.pos);
        }
        self.pos += 1;
        self.skip_ws();
        if self.peek() == Some('$') {
            return fail(NO_VARIABLES, self.pos);
        }
        let Some(field) = self.name() else {
            return fail("expected operation field name", self.pos);
        };
        *parsed_op = Some(field.clone());
        self.pos += field.chars().count();
        self.skip_ws();

        let mut params = Record::new();
        if self.peek() == Some('(') {
            self.pos += 1;
            self.skip_ws();
            while !self.at_end() && self.peek() != Some(')') {
                if self.peek() == Some('$') {
                    return fail(NO_VARIABLES, self.pos);
                }
                let Some(key) = self.name() else {
                    return fail("expected argument name", self.pos);
                };
                self.pos += key.chars().count();
                self.skip_ws();
                if self.peek() != Some(':') {
                    return fail(format!("expected : after argument name \"{key}\""), self.pos);
                }
                self.pos += 1;
                self.skip_ws();
                let val = self.parse_value()?;
                // A null argument is omitted so parameter defaults apply.
                if !val.is_null() {
                    params.insert(key, val);
                }
                self.skip_ws();
                if self.peek() == Some(',') {
                    self.pos += 1;
                    self.skip_ws();
                }
            }
            if self.peek() != Some(')') {
                return fail("expected ) after arguments", self.pos);
            }
            self.pos += 1;
        }

        // Skip the selection set { ... } if present.
        self.skip_ws();
        if self.peek() == Some('{') {
            let start = self.pos;
            let mut depth: i64 = 0;
            let mut in_string = false;
            let mut escaped = false;
            while self.pos < self.text.len() {
                let c = self.text[self.pos];
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = !in_string;
                } else if in_string {
                } else if c == '#' {
                    while self.pos < self.text.len() && self.text[self.pos] != '\n' {
                        self.pos += 1;
                    }
                    continue;
                } else if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += 1;
                        break;
                    }
                }
                self.pos += 1;
            }
            if depth != 0 {
                return fail("unbalanced braces in selection set", start);
            }
        }

        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
        } else {
            if self.at_end() {
                return fail("unclosed operation: expected }", self.pos);
            }
            return fail("more than one top-level field is not supported", self.pos);
        }
        self.skip_ws();
        if !self.at_end() {
            return fail("unexpected trailing content", self.pos);
        }
        Ok(GraphqlInputDocument {
            op: Some(field),
            params,
        })
    }
}

/// Parse a minimal GraphQL document. Variables, named operations and multiple
/// top-level fields are rejected; null arguments are omitted so defaults apply.
pub fn parse_graphql_document(text: &str) -> Outcome<GraphqlInputDocument> {
    let mut parser = Parser {
        text: text.chars().collect(),
        pos: 0,
    };
    let mut parsed_op = None;
    parser.document(&mut parsed_op).map_err(|e| {
        let usage = usage_line(
            &OperationSchema::new(parsed_op.clone().unwrap_or_else(|| "<graphql>".into())),
            None,
        );
        let mut i = ValidationIssue::new(
            IssueCode::DocumentParse,
            format!("{}. Expected: {}", e.message, usage),
            usage.clone(),
            usage,
        );
        i.op = parsed_op;
        i.offset = Some(e.offset);
        vec![i]
    })
}

/// Throwing-style wrapper around [`parse_graphql_document`] (lmts `parseGraphqlInput`).
pub fn parse_graphql_input(text: &str) -> Result<GraphqlInputDocument, SchemaValidationError> {
    parse_graphql_document(text).map_err(|issues| {
        let first = &issues[0];
        let mut details = Record::new();
        details.insert("code".into(), Value::String(first.code.as_str().into()));
        details.insert("synopsis".into(), Value::String(first.synopsis.clone()));
        details.insert("issues".into(), serde_json::to_value(&issues).unwrap_or(Value::Null));
        details.insert("offset".into(), first.offset.map(Value::from).unwrap_or(Value::Null));
        SchemaValidationError::new(
            first.op.clone().unwrap_or_else(|| "<graphql>".into()),
            first.message.clone(),
            details,
        )
    })
}

/// Validate a document's params against the op schema.
pub fn validate_graphql_params(
    schema: &OperationSchema,
    doc: &GraphqlInputDocument,
) -> Result<Record, SchemaValidationError> {
    validate_params(schema, &doc.params)
}
