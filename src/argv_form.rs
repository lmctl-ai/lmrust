//! Array-based argv (flags) form (lmts `argv-form.ts`). The shell already split the
//! command line, so every element is taken as is. Value types come from the schema,
//! never from the token shape; required and enum checks are left to [`crate::validate`].
//!
//! ```text
//! --k v | --k=v     value (may contain spaces, '=', JSON, or start with '-')
//! --k               boolean true (boolean / json params)
//! --no-k            boolean false (boolean / json params)
//! --k a --k b       accumulates for occurs '+'/'*' or type 'array'
//! --                ends flag parsing; the rest are positionals
//! ```

use crate::js;
use crate::synopsis::{synopsis_for_param, to_camel, to_kebab, usage_line};
use crate::types::{
    is_reserved_name, IssueCode, Occurs, OperationSchema, Outcome, ParamSpec, ParamType, Record, ValidationIssue,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Parsed flags: op params, leftover positionals and runner globals.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArgvFormValue {
    pub params: Record,
    pub positionals: Vec<String>,
    pub globals: Record,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Param,
    Global,
}

impl Scope {
    fn tag(self) -> &'static str {
        match self {
            Scope::Param => "param",
            Scope::Global => "global",
        }
    }
}

fn is_multi(spec: &ParamSpec) -> bool {
    spec.kind == ParamType::Array || matches!(spec.occurs, Some(Occurs::OneOrMore) | Some(Occurs::ZeroOrMore))
}

fn element_type(spec: &ParamSpec) -> ParamType {
    if spec.kind == ParamType::Array {
        spec.items.map(|i| i.as_param_type()).unwrap_or(ParamType::String)
    } else {
        spec.kind
    }
}

/// `^-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?$`
pub(crate) fn is_json_number_text(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        let s = i + 1;
        i = s;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == s {
            return false;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let s = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == s {
            return false;
        }
    }
    i == b.len()
}

/// Loose literal coercion used for `json` params and `additionalParams`:
/// `true`/`false`/`null`, numbers, `{...}`/`[...]` JSON, otherwise the raw string.
pub fn coerce_json_literal(raw: &str) -> Value {
    match raw {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        _ => {}
    }
    if is_json_number_text(raw) {
        if let Some(v) = raw.parse::<f64>().ok().and_then(js::number_value) {
            return v;
        }
    }
    let t = js::trim(raw);
    if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']')) {
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            return v;
        }
    }
    Value::String(raw.to_string())
}

fn register<'a>(lookup: &mut HashMap<String, (&'a ParamSpec, Scope)>, spec: &'a ParamSpec, scope: Scope) {
    if is_reserved_name(&spec.name) {
        return;
    }
    let keys = [spec.name.clone(), to_kebab(&spec.name)]
        .into_iter()
        .chain(spec.alias_list().iter().cloned());
    for key in keys {
        if !is_reserved_name(&key) && !lookup.contains_key(&key) {
            lookup.insert(key, (spec, scope));
        }
    }
}

/// Parse an argv array (op name already removed) against an op schema and the runner's
/// global flags. Collects every issue instead of stopping at the first.
pub fn parse_argv_form(argv: &[String], schema: &OperationSchema, globals: &[ParamSpec]) -> Outcome<ArgvFormValue> {
    let mut params = Record::new();
    let mut global_values = Record::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut positional_index: Vec<usize> = Vec::new();
    let mut issues: Vec<ValidationIssue> = Vec::new();
    let usage = usage_line(schema, None);
    let mut seen: HashSet<String> = HashSet::new();
    let mut extra_count: HashMap<String, usize> = HashMap::new();

    // kebab name, camel name and aliases -> spec. Op params win over globals on collision.
    let mut lookup: HashMap<String, (&ParamSpec, Scope)> = HashMap::new();
    for p in &schema.params {
        register(&mut lookup, p, Scope::Param);
    }
    for g in globals {
        register(&mut lookup, g, Scope::Global);
    }

    let issue = |issues: &mut Vec<ValidationIssue>,
                 code: IssueCode,
                 message: String,
                 argv_index: usize,
                 spec: Option<&ParamSpec>,
                 received: Option<Value>,
                 allowed: Option<Vec<Value>>| {
        let synopsis = spec.map(synopsis_for_param).unwrap_or_else(|| usage.clone());
        let mut i = ValidationIssue::new(
            code,
            format!("{message}. Expected: {synopsis}"),
            synopsis,
            usage.clone(),
        );
        i.param = spec.map(|s| s.name.clone());
        i.argv_index = Some(argv_index);
        i.received = received;
        i.allowed = allowed;
        issues.push(i);
    };

    // Coerce one raw string by element type; reports and returns None on failure.
    let coerce = |issues: &mut Vec<ValidationIssue>, spec: &ParamSpec, raw: &str, idx: usize| -> Option<Value> {
        match element_type(spec) {
            ParamType::String => Some(Value::String(raw.to_string())),
            ParamType::Number => {
                let n = if js::trim(raw).is_empty() {
                    f64::NAN
                } else {
                    js::parse_number(raw)
                };
                if n.is_nan() {
                    issue(
                        issues,
                        IssueCode::TypeMismatch,
                        format!("--{}: expected a number, received \"{}\"", to_kebab(&spec.name), raw),
                        idx,
                        Some(spec),
                        Some(Value::String(raw.to_string())),
                        None,
                    );
                    return None;
                }
                // ±Infinity has no JSON form; keep the text so the validator rejects it (TYPE_MISMATCH).
                Some(js::number_value(n).unwrap_or_else(|| Value::String(raw.to_string())))
            }
            ParamType::Boolean => match raw {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                _ => {
                    issue(
                        issues,
                        IssueCode::TypeMismatch,
                        format!(
                            "--{}: expected true or false, received \"{}\"",
                            to_kebab(&spec.name),
                            raw
                        ),
                        idx,
                        Some(spec),
                        Some(Value::String(raw.to_string())),
                        Some(vec![Value::Bool(true), Value::Bool(false)]),
                    );
                    None
                }
            },
            ParamType::Object | ParamType::Array => match serde_json::from_str::<Value>(raw) {
                Ok(v) => Some(v),
                Err(e) => {
                    issue(
                        issues,
                        IssueCode::TypeMismatch,
                        format!("--{}: value is not valid JSON ({})", to_kebab(&spec.name), e),
                        idx,
                        Some(spec),
                        Some(Value::String(raw.to_string())),
                        None,
                    );
                    None
                }
            },
            ParamType::Json => Some(coerce_json_literal(raw)),
        }
    };

    let assign = |issues: &mut Vec<ValidationIssue>,
                  seen: &mut HashSet<String>,
                  params: &mut Record,
                  global_values: &mut Record,
                  target: (&ParamSpec, Scope),
                  value: Value,
                  idx: usize| {
        let (spec, scope) = target;
        let bag = if scope == Scope::Param { params } else { global_values };
        let key = format!("{}:{}", scope.tag(), spec.name);
        if is_multi(spec) {
            match bag.get_mut(&spec.name) {
                Some(Value::Array(arr)) => arr.push(value),
                _ => {
                    bag.insert(spec.name.clone(), Value::Array(vec![value]));
                }
            }
        } else if seen.contains(&key) {
            issue(
                issues,
                IssueCode::DuplicateFlag,
                format!("--{} given more than once", to_kebab(&spec.name)),
                idx,
                Some(spec),
                Some(value),
                None,
            );
        } else {
            bag.insert(spec.name.clone(), value);
        }
        seen.insert(key);
    };

    let mut flags_done = false;
    let mut i = 0usize;
    while i < argv.len() {
        let tok = &argv[i];
        if flags_done {
            positionals.push(tok.clone());
            positional_index.push(i);
            i += 1;
            continue;
        }
        if tok == "--" {
            flags_done = true;
            i += 1;
            continue;
        }

        let mut name: Option<String> = None;
        let mut inline: Option<String> = None;
        if tok.starts_with("--") && tok.len() > 2 {
            let body = &tok[2..];
            match body.find('=') {
                Some(eq) => {
                    name = Some(body[..eq].to_string());
                    inline = Some(body[eq + 1..].to_string());
                }
                None => name = Some(body.to_string()),
            }
        } else {
            let b = tok.as_bytes();
            if b.len() == 2 && b[0] == b'-' && b[1].is_ascii_alphabetic() && lookup.contains_key(&tok[1..]) {
                name = Some(tok[1..].to_string());
            }
        }
        let Some(name) = name else {
            positionals.push(tok.clone());
            positional_index.push(i);
            i += 1;
            continue;
        };

        let mut target = lookup.get(&name).or_else(|| lookup.get(&to_camel(&name))).copied();
        let mut negated = false;
        if target.is_none() {
            if let Some(base) = name.strip_prefix("no-") {
                if let Some(t) = lookup.get(base).or_else(|| lookup.get(&to_camel(base))).copied() {
                    if element_type(t.0) == ParamType::Boolean || t.0.kind == ParamType::Json {
                        target = Some(t);
                        negated = true;
                    }
                }
            }
        }

        let next = argv.get(i + 1);
        let next_is_value = next.is_some_and(|n| !n.starts_with("--"));

        let Some(target) = target else {
            if schema.allows_additional() && !is_reserved_name(&name) && !is_reserved_name(&to_camel(&name)) {
                let key = to_camel(&name);
                let value = if let Some(v) = &inline {
                    coerce_json_literal(v)
                } else if next_is_value {
                    i += 1;
                    coerce_json_literal(&argv[i])
                } else {
                    Value::Bool(true)
                };
                // A single occurrence stays unwrapped; repeats accumulate into an array.
                let count = extra_count.get(&key).copied().unwrap_or(0);
                if count == 0 {
                    params.insert(key.clone(), value);
                } else if count == 1 {
                    let first = params.get(&key).cloned().unwrap_or(Value::Null);
                    params.insert(key.clone(), Value::Array(vec![first, value]));
                } else if let Some(Value::Array(arr)) = params.get_mut(&key) {
                    arr.push(value);
                }
                extra_count.insert(key, count + 1);
                i += 1;
                continue;
            }
            let mut known: Vec<String> = Vec::new();
            for p in &schema.params {
                let k = to_kebab(&p.name);
                if !known.contains(&k) {
                    known.push(k);
                }
            }
            let mut best: Option<(&String, usize)> = None;
            for k in &known {
                let d = js::levenshtein(&name, k);
                if d <= 2 && best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((k, d));
                }
            }
            let mut swallowed: Option<String> = None;
            // Swallow a following value so it does not become a stray positional.
            if inline.is_none() && next_is_value {
                i += 1;
                swallowed = Some(argv[i].clone());
            }
            let swallowed_str = swallowed
                .as_ref()
                .map(|s| format!(" (swallowed value '{s}')"))
                .unwrap_or_default();
            let expected = if known.is_empty() {
                "(none)".to_string()
            } else {
                known.iter().map(|k| format!("--{k}")).collect::<Vec<_>>().join(", ")
            };
            let mut iss = ValidationIssue::new(
                IssueCode::UnknownParam,
                format!("unknown flag --{name}{swallowed_str}. Expected one of: {expected}"),
                usage.clone(),
                usage.clone(),
            );
            iss.param = Some(to_camel(&name));
            iss.argv_index = Some(i - usize::from(swallowed.is_some()));
            iss.received = Some(Value::String(tok.clone()));
            iss.suggestion = best.map(|(k, _)| format!("--{k}"));
            issues.push(iss);
            i += 1;
            continue;
        };

        let spec = target.0;
        if negated {
            if let Some(v) = inline {
                issue(
                    &mut issues,
                    IssueCode::TypeMismatch,
                    format!("--{name} does not take a value"),
                    i,
                    Some(spec),
                    Some(Value::String(v)),
                    None,
                );
            } else {
                assign(
                    &mut issues,
                    &mut seen,
                    &mut params,
                    &mut global_values,
                    target,
                    Value::Bool(false),
                    i,
                );
            }
            i += 1;
            continue;
        }

        if let Some(v) = inline {
            if let Some(val) = coerce(&mut issues, spec, &v, i) {
                assign(&mut issues, &mut seen, &mut params, &mut global_values, target, val, i);
            }
            i += 1;
            continue;
        }

        if element_type(spec) == ParamType::Boolean {
            match next.map(String::as_str) {
                Some(b @ ("true" | "false")) => {
                    assign(
                        &mut issues,
                        &mut seen,
                        &mut params,
                        &mut global_values,
                        target,
                        Value::Bool(b == "true"),
                        i,
                    );
                    i += 1;
                }
                _ => assign(
                    &mut issues,
                    &mut seen,
                    &mut params,
                    &mut global_values,
                    target,
                    Value::Bool(true),
                    i,
                ),
            }
            i += 1;
            continue;
        }

        if !next_is_value {
            if spec.kind == ParamType::Json {
                assign(
                    &mut issues,
                    &mut seen,
                    &mut params,
                    &mut global_values,
                    target,
                    Value::Bool(true),
                    i,
                );
                i += 1;
                continue;
            }
            let follow = next.map(|n| format!(", but was followed by {n}")).unwrap_or_default();
            issue(
                &mut issues,
                IssueCode::MissingValue,
                format!("--{} requires a value{}", to_kebab(&spec.name), follow),
                i,
                Some(spec),
                None,
                None,
            );
            i += 1;
            continue;
        }

        let raw = next.cloned().unwrap_or_default();
        let r = coerce(&mut issues, spec, &raw, i + 1);
        i += 1;
        if let Some(val) = r {
            assign(
                &mut issues,
                &mut seen,
                &mut params,
                &mut global_values,
                target,
                val,
                i - 1,
            );
        }
        i += 1;
    }

    // Bind positionals in order to positional params not already set by flags.
    let slots: Vec<&ParamSpec> = schema.params.iter().filter(|p| p.is_positional()).collect();
    let mut slot = 0usize;
    for (k, raw) in positionals.iter().enumerate() {
        while slot < slots.len() && params.contains_key(&slots[slot].name) && !is_multi(slots[slot]) {
            slot += 1;
        }
        let idx = positional_index[k];
        let Some(spec) = slots.get(slot).copied() else {
            if !schema.allows_additional() {
                let mut iss = ValidationIssue::new(
                    IssueCode::ExtraPositional,
                    format!("unexpected positional argument \"{raw}\". Expected: {usage}"),
                    usage.clone(),
                    usage.clone(),
                );
                iss.argv_index = Some(idx);
                iss.received = Some(Value::String(raw.clone()));
                issues.push(iss);
            }
            continue;
        };
        let Some(value) = coerce(&mut issues, spec, raw, idx) else {
            if !is_multi(spec) {
                slot += 1;
            }
            continue;
        };
        if is_multi(spec) {
            match params.get_mut(&spec.name) {
                Some(Value::Array(arr)) => arr.push(value),
                _ => {
                    params.insert(spec.name.clone(), Value::Array(vec![value]));
                }
            }
        } else {
            params.insert(spec.name.clone(), value);
            slot += 1;
        }
    }

    if issues.is_empty() {
        Ok(ArgvFormValue {
            params,
            positionals,
            globals: global_values,
        })
    } else {
        Err(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_literals() {
        assert_eq!(coerce_json_literal("true"), json!(true));
        assert_eq!(coerce_json_literal("false"), json!(false));
        assert_eq!(coerce_json_literal("null"), json!(null));
        assert_eq!(coerce_json_literal("-1.5e2"), json!(-150));
        assert_eq!(coerce_json_literal(" {\"a\":1} "), json!({"a": 1}));
        assert_eq!(coerce_json_literal("[1,"), json!("[1,"));
        assert_eq!(coerce_json_literal("1e999"), json!("1e999"));
        assert_eq!(coerce_json_literal("plain"), json!("plain"));
        assert!(!is_json_number_text("1."));
        assert!(!is_json_number_text("-"));
        assert!(!is_json_number_text("1e"));
        assert!(is_json_number_text("0.25E+3"));
    }
}
