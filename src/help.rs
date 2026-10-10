//! Help text derived from operation schemas (lmts `help.ts`). The schema is the single
//! source of truth; help is derived, never hand-written.

use crate::js;
use crate::synopsis::{element_type_name, enum_join, is_required, to_kebab};
use crate::types::{Access, Occurs, OperationSchema, ParamSpec, ParamType};

pub use crate::synopsis::{synopsis_for_param, usage_line};

fn alias_text(a: &str) -> String {
    if a.starts_with('-') {
        a.to_string()
    } else if a.chars().count() == 1 {
        format!("-{a}")
    } else {
        format!("--{}", to_kebab(a))
    }
}

fn aliases_joined(p: &ParamSpec) -> Option<String> {
    let aliases = p.alias_list();
    if aliases.is_empty() {
        None
    } else {
        Some(aliases.iter().map(|a| alias_text(a)).collect::<Vec<_>>().join(", "))
    }
}

/// `--help` for one op: name, description, access, usage line and options.
pub fn render_op_help(tool_name: &str, schema: &OperationSchema) -> String {
    let mut lines: Vec<String> = vec![format!("{} {}", tool_name, schema.name)];
    if let Some(d) = schema.description.as_deref().filter(|d| !d.is_empty()) {
        lines.push(String::new());
        lines.push(d.to_string());
    }
    if let Some(a) = schema.access {
        lines.push(String::new());
        lines.push(format!("Access: {a}"));
    }

    let mut usage_parts = vec![format!("{} {}", tool_name, schema.name)];
    for p in schema.params.iter().filter(|p| p.is_positional()) {
        usage_parts.push(match p.occurs.unwrap_or(Occurs::One) {
            Occurs::One => format!("<{}>", p.name),
            Occurs::Optional => format!("[{}]", p.name),
            Occurs::OneOrMore => format!("<{}>...", p.name),
            Occurs::ZeroOrMore => format!("[{}]...", p.name),
        });
    }
    for p in schema.params.iter().filter(|p| !p.is_positional()) {
        let hint = if p.kind == ParamType::Boolean {
            String::new()
        } else {
            format!(" <{}>", element_type_name(p))
        };
        let item = format!("--{}{}", to_kebab(&p.name), hint);
        usage_parts.push(match p.occurs.unwrap_or(Occurs::One) {
            Occurs::One => item,
            Occurs::Optional => format!("[{item}]"),
            Occurs::OneOrMore => format!("{item}..."),
            Occurs::ZeroOrMore => format!("[{item}]..."),
        });
    }
    lines.push(String::new());
    lines.push("Usage:".into());
    lines.push(format!("  {}", usage_parts.join(" ")));

    if !schema.params.is_empty() {
        lines.push(String::new());
        lines.push("Options:".into());
        for p in &schema.params {
            let flag = format!("--{}", to_kebab(&p.name));
            let repeated = matches!(p.occurs, Some(Occurs::OneOrMore) | Some(Occurs::ZeroOrMore));
            let type_hint = if p.kind == ParamType::Boolean {
                String::new()
            } else {
                let of = p.items.map(|i| format!(" of {i}")).unwrap_or_default();
                format!(" <{}{}>{}", element_type_name(p), of, if repeated { "..." } else { "" })
            };
            let tag = if p.is_positional() { " [positional]" } else { "" };
            let alias_tag = aliases_joined(p)
                .map(|a| format!(" (aliases: {a})"))
                .unwrap_or_default();
            let enum_tag = p
                .enum_list()
                .map(|e| format!(" one of: {}", enum_join(e)))
                .unwrap_or_default();
            let default_tag = p
                .default
                .as_ref()
                .map(|d| format!(" (default: {})", js::stringify(d)))
                .unwrap_or_default();
            let required_tag = if is_required(p) { " [required]" } else { "" };
            let description = p
                .description
                .as_deref()
                .filter(|d| !d.is_empty())
                .map(|d| format!("  {d}"))
                .unwrap_or_default();
            lines.push(format!(
                "  {flag}{type_hint}{tag}{required_tag}{enum_tag}{default_tag}{alias_tag}{description}"
            ));
        }
    }
    lines.join("\n") + "\n"
}

fn pad_end(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// Top-level `--help` for a tool: a summary line per op.
pub fn render_tool_help(
    tool_name: &str,
    tool_description: Option<&str>,
    schemas: &[OperationSchema],
    globals: &[ParamSpec],
) -> String {
    let mut lines: Vec<String> = vec![format!("{tool_name} — structured CLI wrapper")];
    if let Some(d) = tool_description.filter(|d| !d.is_empty()) {
        lines.push(String::new());
        lines.push(d.to_string());
    }
    lines.extend([
        "".into(),
        "Usage:".into(),
        format!("  {tool_name} <operation> [options]"),
    ]);
    if !globals.is_empty() {
        lines.push(String::new());
        lines.push("Global options:".into());
        for g in globals {
            let flag = format!("--{}", to_kebab(&g.name));
            let alias = aliases_joined(g).map(|a| format!(" ({a})")).unwrap_or_default();
            let desc = g
                .description
                .as_deref()
                .filter(|d| !d.is_empty())
                .map(|d| format!("  {d}"))
                .unwrap_or_default();
            lines.push(format!("  {}{}", pad_end(&(flag + &alias), 20), desc));
        }
    }
    lines.extend([
        String::new(),
        "Input forms (when the op declares a schema):".into(),
        "  --flag value                command-line flags".into(),
        "  --input-json '<json>'      --input-file <path>      JSON document".into(),
        "  --input-graphql '<doc>'                              GraphQL document".into(),
        String::new(),
        "Operations (name [access] — description):".into(),
    ]);
    for s in schemas {
        let access = s.access.unwrap_or(Access::Read);
        let first = s
            .description
            .as_deref()
            .and_then(|d| d.split('\n').next())
            .map(|l| js::trim(l).to_string())
            .unwrap_or_default();
        let desc = if first.is_empty() {
            String::new()
        } else {
            format!("  {first}")
        };
        lines.push(format!("  {} [{}]{}", pad_end(&s.name, 24), access, desc));
    }
    lines.join("\n") + "\n"
}
