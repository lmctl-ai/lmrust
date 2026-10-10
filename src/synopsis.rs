//! String helpers for parameter synopses and usage lines (lmts `synopsis.ts`).

use crate::js;
use crate::types::{Occurs, OperationSchema, ParamSpec, ParamType};

/// camelCase to kebab-case (`maxFiles` -> `max-files`).
pub fn to_kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// kebab-case to camelCase (`max-files` -> `maxFiles`); only `-` followed by `[a-z0-9]` folds.
pub fn to_camel(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '-' && i + 1 < chars.len() && (chars[i + 1].is_ascii_lowercase() || chars[i + 1].is_ascii_digit()) {
            out.push(chars[i + 1].to_ascii_uppercase());
            i += 2;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Required when occurs is `1` (the default) or `+`.
pub fn is_required(p: &ParamSpec) -> bool {
    matches!(p.occurs, None | Some(Occurs::One) | Some(Occurs::OneOrMore))
}

/// Element type name shown in hints: `items` (default `string`) for arrays, else the type.
pub(crate) fn element_type_name(p: &ParamSpec) -> &'static str {
    if p.kind == ParamType::Array {
        p.items.map(|i| i.as_str()).unwrap_or("string")
    } else {
        p.kind.as_str()
    }
}

fn type_hint(p: &ParamSpec) -> String {
    if p.kind == ParamType::Boolean {
        return String::new();
    }
    let of = p.items.map(|i| format!(" of {i}")).unwrap_or_default();
    format!(" <{}{}>", element_type_name(p), of)
}

pub(crate) fn enum_join(values: &[serde_json::Value]) -> String {
    values.iter().map(js::to_display).collect::<Vec<_>>().join(", ")
}

fn enum_tag(p: &ParamSpec) -> String {
    p.enum_list()
        .map(|e| format!(" one of: {}", enum_join(e)))
        .unwrap_or_default()
}

/// Synopsis fragment without the `[optional]` marker, used in error messages:
/// `--file <string>`, `--state <string> one of: open, closed`, `--ignored`.
pub fn build_synopsis(spec: &ParamSpec) -> String {
    format!("--{}{}{}", to_kebab(&spec.name), type_hint(spec), enum_tag(spec))
}

/// One-line synopsis for a param: `--path <string> [optional]`.
pub fn synopsis_for_param(p: &ParamSpec) -> String {
    let req = if is_required(p) { "" } else { " [optional]" };
    format!("--{}{}{}{}", to_kebab(&p.name), type_hint(p), req, enum_tag(p))
}

/// `[<tool>] <op> [<positional>] [flags]`.
pub fn usage_line(schema: &OperationSchema, tool_name: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(tool) = tool_name.filter(|t| !t.is_empty()) {
        parts.push(tool.to_string());
    }
    parts.push(schema.name.clone());
    for p in &schema.params {
        if p.is_positional() {
            parts.push(format!("[<{}>]", p.name));
        }
    }
    parts.push("[flags]".into());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;
    use serde_json::json;

    #[test]
    fn case_conversion() {
        assert_eq!(to_kebab("maxFiles"), "max-files");
        assert_eq!(to_kebab("already-kebab"), "already-kebab");
        assert_eq!(to_camel("max-files"), "maxFiles");
        assert_eq!(to_camel("dry-run-2"), "dryRun2");
        assert_eq!(to_camel("a-B"), "a-B");
        assert_eq!(to_camel("trailing-"), "trailing-");
    }

    #[test]
    fn synopses() {
        let p = ParamSpec::new("state", ParamType::String).with_enum([json!("open"), json!(2)]);
        assert_eq!(build_synopsis(&p), "--state <string> one of: open, 2");
        assert_eq!(synopsis_for_param(&p), "--state <string> one of: open, 2");
        let q = ParamSpec::new("files", ParamType::Array)
            .items(ItemType::Number)
            .optional();
        assert_eq!(synopsis_for_param(&q), "--files <number of number> [optional]");
        let b = ParamSpec::new("allowEmpty", ParamType::Boolean).optional();
        assert_eq!(synopsis_for_param(&b), "--allow-empty [optional]");
        assert!(!is_required(&b));
        assert!(is_required(
            &ParamSpec::new("x", ParamType::String).occurs(Occurs::OneOrMore)
        ));
    }

    #[test]
    fn usage_lines() {
        let s = OperationSchema::new("add").param(ParamSpec::new("path", ParamType::String).positional());
        assert_eq!(usage_line(&s, None), "add [<path>] [flags]");
        assert_eq!(usage_line(&s, Some("git2")), "git2 add [<path>] [flags]");
    }
}
