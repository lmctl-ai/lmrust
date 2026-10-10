//! Client-side encoders (lmts `encode.ts`): params to an argv array, a JSON envelope or a
//! GraphQL document, each the inverse of the matching server-side parser.
//!
//! Limitations of the argv form: an empty array cannot be expressed (it is omitted), and a
//! `json`-typed param holding a string that looks like a literal (`"true"`, `"12"`) is
//! re-read as that literal.

use crate::js;
use crate::synopsis::to_kebab;
use crate::types::{Access, Occurs, OperationSchema, ParamSpec, ParamType, Record};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;

/// Options for [`encode_argv`].
#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeArgvOptions {
    /// Emit positional params as bare words after `--` instead of as flags.
    pub positional: bool,
}

/// A value or name that the GraphQL subset cannot express.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError(pub String);

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EncodeError {}

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

/// Render one element as the raw string `parse_argv_form` coerces back.
fn argv_value(kind: ParamType, value: &Value) -> String {
    if let Value::String(s) = value {
        if matches!(kind, ParamType::String | ParamType::Json) {
            return s.clone();
        }
    }
    if matches!(kind, ParamType::Number | ParamType::Boolean) {
        return js::to_display(value);
    }
    js::stringify(value)
}

/// Encode params as an argv array (op name not included). Values use `--k=v` so they may
/// start with `-` or contain anything; booleans use `--k` / `--no-k`; multi params repeat.
pub fn encode_argv(schema: &OperationSchema, params: &Record, opts: EncodeArgvOptions) -> Vec<String> {
    let mut out = Vec::new();
    let mut trailing = Vec::new();
    let specs: HashMap<&str, &ParamSpec> = schema.params.iter().map(|p| (p.name.as_str(), p)).collect();
    for (name, value) in params {
        let declared = specs.get(name.as_str()).copied();
        let fallback;
        let spec = match declared {
            Some(s) => s,
            None => {
                fallback = ParamSpec::new(name.clone(), ParamType::Json).optional();
                &fallback
            }
        };
        let flag = format!("--{}", to_kebab(name));
        let kind = element_type(spec);

        if opts.positional && spec.is_positional() {
            match value {
                Value::Array(items) if is_multi(spec) => trailing.extend(items.iter().map(|v| argv_value(kind, v))),
                other => trailing.push(argv_value(kind, other)),
            }
            continue;
        }
        if let (true, Value::Array(items)) = (is_multi(spec), value) {
            out.extend(items.iter().map(|v| format!("{flag}={}", argv_value(kind, v))));
            continue;
        }
        // `--no-k` only resolves for declared params; undeclared ones use `--k=false`.
        if let (Value::Bool(b), Some(_)) = (value, declared) {
            if matches!(kind, ParamType::Boolean | ParamType::Json) {
                out.push(if *b { flag } else { format!("--no-{}", to_kebab(name)) });
                continue;
            }
        }
        out.push(format!("{flag}={}", argv_value(kind, value)));
    }
    if !trailing.is_empty() {
        out.push("--".into());
        out.extend(trailing);
    }
    out
}

/// Encode as the JSON envelope `{"op": name, "params": {...}}`.
pub fn encode_json(schema: &OperationSchema, params: &Record) -> String {
    let mut doc = Record::new();
    doc.insert("op".into(), Value::String(schema.name.clone()));
    doc.insert("params".into(), Value::Object(params.clone()));
    js::stringify(&doc)
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn graphql_value(value: &Value, path: &str) -> Result<String, EncodeError> {
    Ok(match value {
        Value::Null => "null".into(),
        Value::String(_) | Value::Number(_) => js::stringify(value),
        Value::Bool(b) => b.to_string(),
        Value::Array(items) => {
            let parts: Result<Vec<_>, _> = items
                .iter()
                .enumerate()
                .map(|(i, v)| graphql_value(v, &format!("{path}[{i}]")))
                .collect();
            format!("[{}]", parts?.join(", "))
        }
        Value::Object(fields) => {
            let mut parts = Vec::new();
            for (k, v) in fields {
                if !is_ident(k) {
                    return Err(EncodeError(format!("{path}.{k}: object key is not a GraphQL name")));
                }
                parts.push(format!("{k}: {}", graphql_value(v, &format!("{path}.{k}"))?));
            }
            format!("{{{}}}", parts.join(", "))
        }
    })
}

/// Encode as the inline-literal GraphQL subset accepted by the GraphQL parser:
/// `query|mutation { op(arg: <literal>, ...) { __typename } }` (`mutation` for write ops).
pub fn encode_graphql(schema: &OperationSchema, params: &Record) -> Result<String, EncodeError> {
    if !is_ident(&schema.name) {
        return Err(EncodeError(format!("op name {} is not a GraphQL name", schema.name)));
    }
    let mut args = Vec::new();
    for (k, v) in params {
        if !is_ident(k) {
            return Err(EncodeError(format!("param {k} is not a GraphQL name")));
        }
        args.push(format!("{k}: {}", graphql_value(v, k)?));
    }
    let kw = if schema.access == Some(Access::Write) {
        "mutation"
    } else {
        "query"
    };
    let arg_list = if args.is_empty() {
        String::new()
    } else {
        format!("({})", args.join(", "))
    };
    Ok(format!("{kw} {{ {}{arg_list} {{ __typename }} }}", schema.name))
}
