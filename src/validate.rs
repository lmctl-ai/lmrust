//! The single validator (lmts `validate.ts`): validates a raw parameter record against an
//! [`OperationSchema`], collecting every issue, applying defaults and coercing types.

use crate::js;
use crate::synopsis::{build_synopsis, is_required, to_camel, to_kebab, usage_line};
use crate::types::{
    is_reserved_name, InputForm, IssueCode, Occurs, OperationSchema, Outcome, ParamSpec, ParamType, Record,
    ValidationIssue, ValidationOptions,
};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;

/// The throwing-style error of lmts (`SchemaValidationError`): the operation, the first
/// message, and a details record (which carries `issues` when there are any).
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaValidationError {
    pub operation: String,
    pub message: String,
    pub details: Record,
}

impl SchemaValidationError {
    pub fn new(operation: impl Into<String>, message: impl Into<String>, details: Record) -> Self {
        SchemaValidationError {
            operation: operation.into(),
            message: message.into(),
            details,
        }
    }

    /// The issues carried in `details.issues` (empty when there are none).
    pub fn issues(&self) -> Vec<ValidationIssue> {
        self.details
            .get("issues")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Build the error lmts throws for a list of issues: first message, `{...first, issues}`.
    pub(crate) fn from_issues(operation: &str, issues: &[ValidationIssue], mut details: Record) -> Self {
        let first = &issues[0];
        if let Ok(Value::Object(fields)) = serde_json::to_value(first) {
            for (k, v) in fields {
                details.insert(k, v);
            }
        }
        details.insert("issues".into(), serde_json::to_value(issues).unwrap_or(Value::Null));
        SchemaValidationError::new(operation, first.message.clone(), details)
    }
}

impl fmt::Display for SchemaValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SchemaValidationError {}

fn find_suggestion(key: &str, params: &[ParamSpec]) -> Option<String> {
    let clean = to_camel(key.strip_prefix("--").unwrap_or(key));
    let mut best = None;
    let mut min = 3; // suggest only when edit distance <= 2
    for p in params {
        let d_camel = js::levenshtein(&clean, &p.name);
        let d_kebab = js::levenshtein(&to_kebab(&clean), &to_kebab(&p.name));
        let d = d_camel.min(d_kebab);
        if d < min {
            min = d;
            best = Some(p.name.clone());
        }
    }
    best
}

/// Normalize a value to a list: arrays as is, a string holding a JSON array is parsed,
/// any other string / number / boolean becomes a one-element list; objects and null are not lists.
pub fn to_array_value(value: &Value) -> Option<Vec<Value>> {
    match value {
        Value::Array(items) => Some(items.clone()),
        Value::String(s) => {
            let trimmed = js::trim(s);
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(trimmed) {
                    return Some(items);
                }
            }
            Some(vec![value.clone()])
        }
        Value::Number(_) | Value::Bool(_) => Some(vec![value.clone()]),
        _ => None,
    }
}

fn is_int_text(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// `^-?(?:\d+\.?\d*|\d*\.\d+)(?:[eE][+-]?\d+)?$`
fn is_float_text(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        None => !mantissa.is_empty() && mantissa.bytes().all(|b| b.is_ascii_digit()),
        Some((int, frac)) => {
            int.bytes().all(|b| b.is_ascii_digit())
                && frac.bytes().all(|b| b.is_ascii_digit())
                && !(int.is_empty() && frac.is_empty())
        }
    };
    let exponent_ok = match exponent {
        None => true,
        Some(e) => {
            let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        }
    };
    mantissa_ok && exponent_ok
}

/// Coerce one value to a scalar type; `Err` carries nothing (the caller reports the issue).
fn coerce_scalar(expected: ParamType, value: &Value) -> Result<Value, ()> {
    match expected {
        ParamType::Boolean => match value {
            Value::Bool(_) => Ok(value.clone()),
            Value::String(s) if s == "true" => Ok(Value::Bool(true)),
            Value::String(s) if s == "false" => Ok(Value::Bool(false)),
            _ => Err(()),
        },
        ParamType::Number => match value {
            // serde_json numbers are always finite.
            Value::Number(_) => Ok(value.clone()),
            Value::String(s) => {
                if js::trim(s).is_empty() {
                    return Err(());
                }
                if is_int_text(s) || is_float_text(s) {
                    if let Some(v) = s.parse::<f64>().ok().and_then(js::number_value) {
                        return Ok(v);
                    }
                }
                Err(())
            }
            _ => Err(()),
        },
        ParamType::String => match value {
            Value::String(_) => Ok(value.clone()),
            Value::Number(_) | Value::Bool(_) => Ok(Value::String(js::to_display(value))),
            _ => Err(()),
        },
        ParamType::Object => match value {
            Value::Object(_) => Ok(value.clone()),
            Value::String(s) => match serde_json::from_str::<Value>(s) {
                Ok(parsed @ Value::Object(_)) => Ok(parsed),
                _ => Err(()),
            },
            _ => Err(()),
        },
        ParamType::Json | ParamType::Array => Ok(value.clone()),
    }
}

/// Validate a raw parameter record against an operation schema, collecting all issues.
/// On success the record holds typed values with defaults applied.
pub fn validate(schema: &OperationSchema, raw: &Record, opts: ValidationOptions) -> Outcome<Record> {
    let mut issues: Vec<ValidationIssue> = Vec::new();
    let mut out = Record::new();
    let usage = usage_line(schema, None);
    let reserved = |code_msg: String, param: Option<String>| {
        let mut i = ValidationIssue::new(IssueCode::ReservedName, code_msg, usage.clone(), usage.clone());
        i.op = Some(schema.name.clone());
        i.param = param;
        i
    };

    if is_reserved_name(&schema.name) {
        issues.push(reserved(
            format!(
                "operation name \"{}\" is a reserved name. Expected: {}",
                schema.name, usage
            ),
            None,
        ));
    }
    for p in &schema.params {
        if is_reserved_name(&p.name) {
            issues.push(reserved(
                format!("param \"{}\" is a reserved name. Expected: {}", p.name, usage),
                Some(p.name.clone()),
            ));
        }
        for a in p.alias_list() {
            if is_reserved_name(a) {
                issues.push(reserved(
                    format!(
                        "param \"{}\" alias \"{}\" is a reserved name. Expected: {}",
                        p.name, a, usage
                    ),
                    Some(a.clone()),
                ));
            }
        }
    }

    let mut spec_by_name: HashMap<&str, &ParamSpec> = HashMap::new();
    for p in &schema.params {
        if !is_reserved_name(&p.name) {
            spec_by_name.insert(&p.name, p);
        }
        for a in p.alias_list() {
            if !is_reserved_name(a) {
                spec_by_name.insert(a, p);
            }
        }
    }

    // 1. Unknown parameters.
    for (key, value) in raw {
        if key.starts_with("--") || !spec_by_name.contains_key(key.as_str()) {
            if !key.starts_with("--") && schema.allows_additional() && !is_reserved_name(key) {
                out.insert(key.clone(), value.clone());
            } else {
                let mut i = ValidationIssue::new(
                    IssueCode::UnknownParam,
                    format!("unknown param: {}. Expected: {}", key, usage),
                    usage.clone(),
                    usage.clone(),
                );
                i.param = Some(key.clone());
                i.received = Some(value.clone());
                i.suggestion = find_suggestion(key, &schema.params);
                issues.push(i);
            }
        }
    }

    // 2. Declared parameters.
    for spec in &schema.params {
        if is_reserved_name(&spec.name) {
            continue;
        }
        let synopsis = build_synopsis(spec);
        let issue = |code: IssueCode, message: String, received: Option<Value>| {
            let mut i = ValidationIssue::new(code, message, synopsis.clone(), usage.clone());
            i.param = Some(spec.name.clone());
            i.received = received;
            i
        };
        let mut value: Option<&Value> = raw.get(&spec.name);
        if value.is_none() {
            value = spec.alias_list().iter().find_map(|a| raw.get(a));
        }
        let is_req = is_required(spec);

        // Form restrictions (cliOnly / docOnly).
        if let (Some(form), Some(v)) = (opts.form, value) {
            if spec.cli_only == Some(true) && matches!(form, InputForm::Json | InputForm::Graphql) {
                issues.push(issue(
                    IssueCode::FormNotAllowed,
                    format!(
                        "param \"{}\" is CLI-only and not allowed in {} form. Expected: {}",
                        spec.name,
                        form.as_str(),
                        synopsis
                    ),
                    Some(v.clone()),
                ));
                continue;
            }
            if spec.doc_only == Some(true) && form == InputForm::Flags {
                issues.push(issue(
                    IssueCode::FormNotAllowed,
                    format!(
                        "param \"{}\" is doc-only and not allowed in flags form. Expected: {}",
                        spec.name, synopsis
                    ),
                    Some(v.clone()),
                ));
                continue;
            }
        }

        // Null: optional -> absent; required -> TYPE_MISMATCH.
        if matches!(value, Some(Value::Null)) {
            if is_req {
                issues.push(issue(
                    IssueCode::TypeMismatch,
                    format!(
                        "expected {} for param \"{}\", received null. Expected: {}",
                        spec.kind, spec.name, synopsis
                    ),
                    Some(Value::Null),
                ));
                continue;
            }
            value = None;
        }

        let Some(value) = value else {
            if let Some(d) = &spec.default {
                out.insert(spec.name.clone(), d.clone());
                continue;
            }
            if is_req {
                issues.push(issue(
                    IssueCode::MissingRequired,
                    format!("missing required param: {}. Expected: {}", spec.name, synopsis),
                    None,
                ));
            }
            continue;
        };

        let is_multi =
            matches!(spec.occurs, Some(Occurs::OneOrMore) | Some(Occurs::ZeroOrMore)) || spec.kind == ParamType::Array;
        if is_multi {
            let Some(arr) = to_array_value(value) else {
                issues.push(issue(
                    IssueCode::TypeMismatch,
                    format!("expected array for param \"{}\". Expected: {}", spec.name, synopsis),
                    Some(value.clone()),
                ));
                continue;
            };
            if spec.occurs == Some(Occurs::OneOrMore) && arr.is_empty() {
                issues.push(issue(
                    IssueCode::MissingRequired,
                    format!(
                        "param \"{}\" requires at least one item. Expected: {}",
                        spec.name, synopsis
                    ),
                    Some(value.clone()),
                ));
                continue;
            }
            if arr.is_empty() {
                out.insert(spec.name.clone(), Value::Array(Vec::new()));
                continue;
            }
            let elem = if spec.kind == ParamType::Array {
                spec.items.map(|i| i.as_param_type()).unwrap_or(ParamType::String)
            } else {
                spec.kind
            };
            let mut coerced_items = Vec::with_capacity(arr.len());
            let mut failed = false;
            for item in &arr {
                let Ok(coerced) = coerce_scalar(elem, item) else {
                    issues.push(issue(
                        IssueCode::TypeMismatch,
                        format!(
                            "expected {} item for param \"{}\". Expected: {}",
                            elem, spec.name, synopsis
                        ),
                        Some(item.clone()),
                    ));
                    failed = true;
                    break;
                };
                if let Some(allowed) = spec.enum_list() {
                    if !allowed.iter().any(|a| js::strict_eq(a, &coerced)) {
                        let mut i = issue(
                            IssueCode::EnumViolation,
                            format!(
                                "invalid value for param \"{}\": {}; allowed: {}. Expected: {}",
                                spec.name,
                                js::stringify(&coerced),
                                crate::synopsis::enum_join(allowed),
                                synopsis
                            ),
                            Some(coerced.clone()),
                        );
                        i.allowed = Some(allowed.to_vec());
                        issues.push(i);
                        failed = true;
                        break;
                    }
                }
                coerced_items.push(coerced);
            }
            if !failed {
                out.insert(spec.name.clone(), Value::Array(coerced_items));
            }
            continue;
        }

        let Ok(coerced) = coerce_scalar(spec.kind, value) else {
            issues.push(issue(
                IssueCode::TypeMismatch,
                format!(
                    "expected {} for param \"{}\". Expected: {}",
                    spec.kind, spec.name, synopsis
                ),
                Some(value.clone()),
            ));
            continue;
        };
        if let Some(allowed) = spec.enum_list() {
            if !allowed.iter().any(|a| js::strict_eq(a, &coerced)) {
                let mut i = issue(
                    IssueCode::EnumViolation,
                    format!(
                        "invalid value for param \"{}\": {}; allowed: {}. Expected: {}",
                        spec.name,
                        js::stringify(&coerced),
                        crate::synopsis::enum_join(allowed),
                        synopsis
                    ),
                    Some(coerced.clone()),
                );
                i.allowed = Some(allowed.to_vec());
                issues.push(i);
                continue;
            }
        }
        out.insert(spec.name.clone(), coerced);
    }

    if issues.is_empty() {
        Ok(out)
    } else {
        Err(issues)
    }
}

/// Throwing-style wrapper around [`validate`] (lmts `validateParams`): the first issue's
/// message, with details `{spec, known, issues, ...firstIssue}`.
pub fn validate_params(schema: &OperationSchema, raw: &Record) -> Result<Record, SchemaValidationError> {
    validate(schema, raw, ValidationOptions::default()).map_err(|issues| {
        let mut details = Record::new();
        details.insert(
            "spec".into(),
            issues[0].param.clone().map(Value::String).unwrap_or(Value::Null),
        );
        details.insert(
            "known".into(),
            Value::Array(schema.params.iter().map(|p| Value::String(p.name.clone())).collect()),
        );
        SchemaValidationError::from_issues(&schema.name, &issues, details)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_text_grammar() {
        for ok in ["1", "1.", ".5", "1.5e3", "-2E-2", "0.0"] {
            assert!(is_float_text(ok), "{ok}");
        }
        for bad in [".", "", "e5", "1e", "1.2.3", "--1", "+1", "1e+"] {
            assert!(!is_float_text(bad), "{bad}");
        }
        assert!(is_int_text("-12"));
        assert!(!is_int_text("-"));
    }
}
