//! Interpreter and classifier for LLM-produced command lines (lmts `interpret.ts`):
//! safe tokenization, strict validation against published schemas, effect
//! classification and compact prompt rendering. Never runs a shell.

use crate::argv_form::parse_argv_form;
use crate::js;
use crate::synopsis::to_kebab;
use crate::types::{
    is_reserved_name, Effect, Interpreted, IssueCode, Occurs, OperationSchema, Outcome, ParamSpec, ParamType,
    PublishedCliSchema, Record, ValidationIssue, ValidationOptions,
};
use crate::validate::validate;

const DEFAULT_SYNOPSIS: &str = "<tool> <operation> [flags]";
const DEFAULT_USAGE: &str = "<tool> <operation> [flags]";

/// Shell operators and characters refused outside quotes.
const REFUSED_OUTSIDE: [char; 14] = [';', '&', '|', '<', '>', '(', ')', '{', '}', '$', '`', '*', '?', '['];

/// Whether an operation is effectful: true unless `access` is `read` (a missing access
/// is effectful, never read-only).
pub fn is_effectful(op: &OperationSchema) -> bool {
    op.access != Some(crate::types::Access::Read)
}

/// Tokenize one command line into argv.
///
/// POSIX-style quoting: single quotes are literal; double quotes allow backslash
/// escaping of `"` and `\` only and refuse `$` and backticks; backslash escapes outside
/// quotes. Refused (`SHELL_SYNTAX`): unquoted `; & | < > ( ) { } $` and backtick,
/// newline/CR/NUL, unterminated quotes, unquoted globs `* ? [`, words starting with an
/// unquoted `~` or `#`, and a leading `NAME=value` assignment. Nothing is ever expanded.
pub fn tokenize_simple_command(line: &str) -> Outcome<Vec<String>> {
    let mut issues: Vec<ValidationIssue> = Vec::new();
    let syntax = |message: String, issues: &mut Vec<ValidationIssue>| {
        issues.push(ValidationIssue::new(
            IssueCode::ShellSyntax,
            format!("{message}; quote it or send one simple command"),
            DEFAULT_SYNOPSIS,
            DEFAULT_USAGE,
        ));
    };

    if line.contains(['\r', '\n', '\0']) {
        syntax("newline, carriage return, or NUL character".into(), &mut issues);
        return Err(issues);
    }

    #[derive(PartialEq)]
    enum Mode {
        Normal,
        Single,
        Double,
    }
    let chars: Vec<char> = line.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut mode = Mode::Normal;
    let mut word_start = true;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        let next = chars.get(i + 1).copied();
        match mode {
            Mode::Single => {
                if ch == '\'' {
                    mode = Mode::Normal;
                } else {
                    current.push(ch);
                }
                i += 1;
                continue;
            }
            Mode::Double => {
                if ch == '"' {
                    mode = Mode::Normal;
                } else if ch == '$' {
                    if next == Some('(') {
                        syntax(
                            "command substitution \"$(...)\" inside double quotes".into(),
                            &mut issues,
                        );
                        i += 1;
                    } else {
                        syntax("variable expansion \"$\" inside double quotes".into(), &mut issues);
                    }
                } else if ch == '`' {
                    syntax(
                        "backtick command substitution \"`\" inside double quotes".into(),
                        &mut issues,
                    );
                } else if ch == '\\' {
                    match next {
                        None => syntax("unterminated escape in double quotes".into(), &mut issues),
                        Some(n @ ('"' | '\\')) => {
                            current.push(n);
                            i += 1;
                        }
                        Some('$') => {
                            syntax("variable expansion \"$\" inside double quotes".into(), &mut issues);
                            i += 1;
                        }
                        Some('`') => {
                            syntax(
                                "backtick command substitution \"`\" inside double quotes".into(),
                                &mut issues,
                            );
                            i += 1;
                        }
                        Some(n) => {
                            // POSIX: a backslash before other characters is preserved literally.
                            current.push('\\');
                            current.push(n);
                            i += 1;
                        }
                    }
                } else {
                    current.push(ch);
                }
                i += 1;
                continue;
            }
            Mode::Normal => {}
        }

        if ch == ' ' || ch == '\t' {
            if in_token {
                tokens.push(std::mem::take(&mut current));
                in_token = false;
            }
            word_start = true;
            i += 1;
            continue;
        }

        if word_start {
            if ch == '#' {
                syntax("shell comment starting with \"#\"".into(), &mut issues);
                break;
            }
            if ch == '~' {
                syntax("tilde expansion \"~\"".into(), &mut issues);
                word_start = false;
                in_token = true;
                i += 1;
                continue;
            }
        }

        if ch == '\'' || ch == '"' {
            in_token = true;
            mode = if ch == '\'' { Mode::Single } else { Mode::Double };
            word_start = false;
            i += 1;
            continue;
        }

        if ch == '\\' {
            in_token = true;
            word_start = false;
            match next {
                None => syntax("unterminated backslash escape at end of line".into(), &mut issues),
                Some(n) if REFUSED_OUTSIDE.contains(&n) => {
                    syntax(format!("shell character \"{n}\""), &mut issues);
                    i += 1;
                }
                Some(n) => {
                    current.push(n);
                    i += 1;
                }
            }
            i += 1;
            continue;
        }

        if REFUSED_OUTSIDE.contains(&ch) {
            let pair = |c: char| next == Some(c);
            if ch == '&' && pair('&') {
                syntax("shell operator \"&&\"".into(), &mut issues);
                i += 1;
            } else if ch == '|' && pair('|') {
                syntax("shell operator \"||\"".into(), &mut issues);
                i += 1;
            } else if ch == '>' && pair('>') {
                syntax("redirection operator \">>\"".into(), &mut issues);
                i += 1;
            } else if ch == '<' && pair('<') {
                syntax("redirection operator \"<<\"".into(), &mut issues);
                i += 1;
            } else if ch == '$' && pair('(') {
                syntax("command substitution \"$(...)\"".into(), &mut issues);
                i += 1;
            } else {
                let msg = match ch {
                    ';' | '&' | '|' => format!("shell operator \"{ch}\""),
                    '<' | '>' => format!("redirection operator \"{ch}\""),
                    '(' | ')' => format!("subshell operator \"{ch}\""),
                    '{' | '}' => format!("brace operator \"{ch}\""),
                    '$' => "variable expansion or subshell \"$\"".to_string(),
                    '`' => "backtick command substitution \"`\"".to_string(),
                    _ => format!("glob pattern \"{ch}\""),
                };
                syntax(msg, &mut issues);
            }
            in_token = true;
            word_start = false;
            i += 1;
            continue;
        }

        in_token = true;
        current.push(ch);
        word_start = false;
        i += 1;
    }

    match mode {
        Mode::Single => syntax("unterminated single quote".into(), &mut issues),
        Mode::Double => syntax("unterminated double quote".into(), &mut issues),
        Mode::Normal => {}
    }
    if in_token {
        tokens.push(current);
    }
    if let Some(first) = tokens.first() {
        if is_env_assignment(first) {
            syntax(format!("leading environment assignment \"{first}\""), &mut issues);
        }
    }
    if issues.is_empty() {
        Ok(tokens)
    } else {
        Err(issues)
    }
}

/// `^[a-zA-Z_][a-zA-Z0-9_]*=`
fn is_env_assignment(word: &str) -> bool {
    let Some(eq) = word.find('=') else { return false };
    let name = &word[..eq];
    let b = name.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// `read` only when `access` is `read`; missing, `write` and `dynamic` are `effectful`.
/// A schema can never upgrade itself to read-only.
pub fn classify_operation(op: &OperationSchema) -> Effect {
    if is_effectful(op) {
        Effect::Effectful
    } else {
        Effect::Read
    }
}

fn suggest<'a, I: IntoIterator<Item = &'a str>>(word: &str, candidates: I) -> Option<String> {
    let mut best = None;
    let mut min = 3;
    for c in candidates {
        let d = js::levenshtein(word, c);
        if d < min {
            min = d;
            best = Some(c.to_string());
        }
    }
    best
}

/// Interpret and validate an LLM-produced command line against one or more published
/// schemas, with optional extra runner globals. Never executes anything.
pub fn interpret_command(schemas: &[PublishedCliSchema], line: &str, globals: &[ParamSpec]) -> Outcome<Interpreted> {
    let known_tools: Vec<&str> = schemas.iter().map(|s| s.tool.as_str()).collect();
    let tools_usage = if known_tools.is_empty() {
        DEFAULT_USAGE.to_string()
    } else {
        known_tools
            .iter()
            .map(|t| format!("{t} <op> [flags]"))
            .collect::<Vec<_>>()
            .join(" | ")
    };

    let tokens = tokenize_simple_command(line)?;
    let Some(tool_name) = tokens.first() else {
        return Err(vec![ValidationIssue::new(
            IssueCode::UnknownTool,
            format!(
                "empty command; expected a tool name. Available tools: {}",
                known_tools.join(", ")
            ),
            tools_usage.clone(),
            tools_usage,
        )]);
    };

    let Some(tool) = schemas.iter().find(|s| &s.tool == tool_name) else {
        let mut i = ValidationIssue::new(
            IssueCode::UnknownTool,
            format!(
                "unknown tool \"{tool_name}\". Available tools: {}",
                known_tools.join(", ")
            ),
            tools_usage.clone(),
            tools_usage,
        );
        i.suggestion = suggest(tool_name, known_tools.iter().copied());
        return Err(vec![i]);
    };

    let op_names: Vec<&str> = tool.operations.iter().map(|o| o.name.as_str()).collect();
    let tool_usage = format!(
        "{} <op> [flags] (available operations: {})",
        tool.tool,
        op_names.join(", ")
    );

    let Some(second) = tokens.get(1) else {
        return Err(vec![ValidationIssue::new(
            IssueCode::UnknownOperation,
            format!(
                "missing operation for tool \"{}\". Available operations: {}",
                tool.tool,
                op_names.join(", ")
            ),
            tool_usage.clone(),
            tool_usage,
        )]);
    };

    if second.starts_with('-') {
        return Err(vec![ValidationIssue::new(
            IssueCode::FlagBeforeOp,
            format!(
                "flag \"{second}\" provided where an operation name was expected; specify an operation before flags. Available operations: {}",
                op_names.join(", ")
            ),
            tool_usage.clone(),
            tool_usage,
        )]);
    }

    let Some(op) = tool.operations.iter().find(|o| &o.name == second) else {
        let mut i = ValidationIssue::new(
            IssueCode::UnknownOperation,
            format!(
                "unknown operation \"{second}\" for tool \"{}\". Available operations: {}",
                tool.tool,
                op_names.join(", ")
            ),
            tool_usage.clone(),
            tool_usage,
        );
        i.op = Some(second.clone());
        i.suggestion = suggest(second, op_names.iter().copied());
        return Err(vec![i]);
    };

    // Tool globals, then caller globals (a caller global replaces a same-named tool global in place).
    let mut combined: Vec<ParamSpec> = Vec::new();
    for g in tool.globals.iter().flatten().chain(globals) {
        match combined.iter_mut().find(|c| c.name == g.name) {
            Some(slot) => *slot = g.clone(),
            None => combined.push(g.clone()),
        }
    }

    let mut issues = Vec::new();
    let argv_result = parse_argv_form(&tokens[2..], op, &combined);
    let raw_params = match &argv_result {
        Ok(v) => v.params.clone(),
        Err(i) => {
            issues.extend(i.iter().cloned());
            Record::new()
        }
    };
    let validated = validate(op, &raw_params, ValidationOptions::form(crate::types::InputForm::Flags));
    let params = match validated {
        Ok(p) => p,
        Err(i) => {
            issues.extend(i);
            return Err(issues);
        }
    };
    let argv_value = match argv_result {
        Ok(v) if issues.is_empty() => v,
        _ => return Err(issues),
    };

    let safe = |rec: Record| -> Record { rec.into_iter().filter(|(k, _)| !is_reserved_name(k)).collect() };
    Ok(Interpreted {
        tool: tool.tool.clone(),
        op: op.name.clone(),
        argv: tokens.clone(),
        params: safe(params),
        positionals: argv_value.positionals,
        globals: safe(argv_value.globals),
        effect: classify_operation(op),
        access: op.access,
    })
}

fn format_param_for_prompt(p: &ParamSpec) -> String {
    let occurs = p.occurs.unwrap_or(Occurs::One);
    let is_list = p.kind == ParamType::Array || matches!(occurs, Occurs::OneOrMore | Occurs::ZeroOrMore);
    let is_optional = matches!(occurs, Occurs::Optional | Occurs::ZeroOrMore);
    if p.is_positional() {
        let base = if is_optional {
            format!("[{}]", p.name)
        } else {
            format!("<{}>", p.name)
        };
        return if is_list { format!("{base}...") } else { base };
    }
    let type_hint = if let Some(e) = p.enum_list() {
        format!("<{}>", e.iter().map(js::to_display).collect::<Vec<_>>().join("|"))
    } else if p.kind == ParamType::Array {
        format!("<{}>...", p.items.map(|i| i.as_str()).unwrap_or("string"))
    } else if p.kind != ParamType::Boolean {
        format!("<{}>", p.kind)
    } else {
        String::new()
    };
    let flag = format!("--{}", to_kebab(&p.name));
    let flag_with_val = if type_hint.is_empty() {
        flag
    } else {
        format!("{flag} {type_hint}")
    };
    if is_optional {
        format!("[{flag_with_val}]")
    } else {
        flag_with_val
    }
}

/// Compact, deterministic text description of schemas for LLM prompts: a header per
/// tool, one synopsis line per op with its effect and description, and three rules.
pub fn describe_for_prompt(schemas: &[PublishedCliSchema]) -> String {
    let mut list: Vec<&PublishedCliSchema> = schemas.iter().collect();
    list.sort_by(|a, b| js::locale_compare(&a.tool, &b.tool));
    let mut lines: Vec<String> = Vec::new();
    for s in list {
        lines.push(format!("# {}", s.tool));
        let mut ops: Vec<&OperationSchema> = s.operations.iter().collect();
        ops.sort_by(|a, b| js::locale_compare(&a.name, &b.name));
        for op in ops {
            let mut flags: Vec<&ParamSpec> = op.params.iter().filter(|p| !p.is_positional()).collect();
            flags.sort_by(|a, b| js::locale_compare(&to_kebab(&a.name), &to_kebab(&b.name)));
            let mut parts = vec![format!("{} {}", s.tool, op.name)];
            parts.extend(
                op.params
                    .iter()
                    .filter(|p| p.is_positional())
                    .map(format_param_for_prompt),
            );
            parts.extend(flags.into_iter().map(format_param_for_prompt));
            parts.push(format!("({})", classify_operation(op).as_str()));
            if let Some(d) = op.description.as_deref().filter(|d| !d.is_empty()) {
                parts.push(format!("— {d}"));
            }
            lines.push(parts.join(" "));
        }
    }
    lines.push(String::new());
    lines.push("Rules:".into());
    lines.push(
        "1. One simple command: execute exactly one command line starting with the tool name; do not chain commands."
            .into(),
    );
    lines.push(
        "2. Quote values with spaces: wrap parameter values containing spaces or special characters in quotes.".into(),
    );
    lines.push("3. No shell operators: operators, pipes, redirects, subshells, backticks, globs, and env prefixes are refused.".into());
    lines.join("\n")
}
