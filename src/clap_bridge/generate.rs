//! Operation plans from a clap command tree plus the overlay.
//!
//! For each overlay op the clap command at `command` is walked after `Command::build` (so
//! global args are propagated to leaves): every visible argument becomes a parameter with its
//! long/short/aliases, occurrence (`1 ? + *` from required/num_args/action), type (from the value
//! parser: numbers, booleans, value enums), default and help text. The [`OpPlan`] also records
//! how to turn validated params back into the legacy argv ([`super::legacy_argv`]).

use super::overlay::{InlineKind, InlineVia, Overlay, OverlayAccess, OverlayOp, ResultFile};
use crate::types::{Access, ItemType, Occurs, OperationSchema, ParamSpec, ParamType, ReturnType};
use clap::{Arg, ArgAction, Command};
use serde_json::Value;
use std::any::TypeId;

/// Where one published param goes on the legacy command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    /// `--long=value` (or `--long` for booleans).
    Long(String),
    /// `-s value` (or `-s` for booleans) when the arg has no long name.
    Short(char),
    /// A positional value (clap index); `last` positionals follow `--`.
    Positional { index: usize, last: bool },
    /// Not on the command line: the value only goes to stdin.
    StdinOnly,
}

/// How the arg consumes values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// On/off flag (`SetTrue`/`SetFalse`): passed when the param is `true`.
    Flag,
    /// Counted flag (`-vvv`): repeated `n` times.
    Count,
    /// Takes values.
    Value,
}

/// How one published param maps back to the legacy command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    /// Published param name.
    pub param: String,
    /// Index into [`OpPlan::words`] of the command level the arg belongs to.
    pub level: usize,
    pub slot: Slot,
    pub kind: ValueKind,
    /// Array param (`Append`, or `num_args` above one).
    pub multi: bool,
    /// `ArgAction::Append`: each value repeats the flag.
    pub append: bool,
    /// clap `value_delimiter`: a multi value is joined into one argument.
    pub delimiter: Option<char>,
    /// The published default; a value equal to it is not passed (clap applies it).
    pub default: Option<Value>,
    /// The value is piped to stdin; the slot receives this token (`None`: slot left out).
    pub stdin_token: Option<Option<String>>,
}

/// An op's file-like input that may be given inline (overlay `inline_inputs`).
#[derive(Debug, Clone, PartialEq)]
pub struct InlinePlan {
    /// Published name of the input param (e.g. `file`).
    pub target: String,
    /// The clap arg was required: a call must give the input or one form.
    pub required: bool,
    /// What the input's argv slot receives when a form is used (`-`).
    pub token: String,
    pub forms: Vec<InlineFormPlan>,
}

/// One inline form of an [`InlinePlan`].
#[derive(Debug, Clone, PartialEq)]
pub struct InlineFormPlan {
    pub param: String,
    pub kind: InlineKind,
    pub args: Vec<String>,
    pub via: InlineVia,
    /// Temp file name when `via` is `File`.
    pub file_name: Option<String>,
}

/// One published operation and how to run it through the legacy CLI.
#[derive(Debug, Clone)]
pub struct OpPlan {
    pub name: String,
    /// Legacy command words, e.g. `["data", "load"]`.
    pub words: Vec<String>,
    pub access: Access,
    pub dangerous: bool,
    pub schema: OperationSchema,
    pub bindings: Vec<Binding>,
    pub fixed_args: Vec<String>,
    /// Append the overlay's `legacy_json_args`.
    pub legacy_json: bool,
    /// The op's inline-capable input, if any.
    pub inline: Option<InlinePlan>,
    /// The op's data comes back through a file (overlay `result_file`).
    pub result_file: Option<ResultFile>,
}

impl OpPlan {
    /// The binding whose value is piped to stdin, if any.
    pub fn stdin_binding(&self) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.stdin_token.is_some())
    }
}

/// kebab-case or snake_case to camelCase (`exit-zero-on-no-match` -> `exitZeroOnNoMatch`).
pub fn camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '-' || c == '_' {
            upper = !out.is_empty();
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn access_of(access: OverlayAccess) -> Access {
    match access {
        OverlayAccess::Read => Access::Read,
        OverlayAccess::Write => Access::Write,
        OverlayAccess::Dynamic => Access::Dynamic,
    }
}

/// Every leaf command path of a built tree (`["data", "load"]`), clap's `help` excluded.
pub(crate) fn leaf_paths(root: &Command) -> Vec<Vec<String>> {
    fn walk(cmd: &Command, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        let subs: Vec<&Command> = cmd.get_subcommands().filter(|c| c.get_name() != "help").collect();
        if subs.is_empty() {
            out.push(prefix.clone());
            return;
        }
        for sub in subs {
            prefix.push(sub.get_name().to_string());
            walk(sub, prefix, out);
            prefix.pop();
        }
    }
    let mut out = Vec::new();
    walk(root, &mut Vec::new(), &mut out);
    out
}

pub(crate) fn find_path<'a>(root: &'a Command, words: &[String]) -> Option<Vec<&'a Command>> {
    let mut chain = vec![root];
    for word in words {
        let next = chain.last()?.get_subcommands().find(|c| c.get_name() == word)?;
        chain.push(next);
    }
    Some(chain)
}

fn is_type<T: 'static>(arg: &Arg) -> bool {
    arg.get_value_parser().type_id() == TypeId::of::<T>()
}

fn is_number(arg: &Arg) -> bool {
    is_type::<u8>(arg)
        || is_type::<u16>(arg)
        || is_type::<u32>(arg)
        || is_type::<u64>(arg)
        || is_type::<usize>(arg)
        || is_type::<i8>(arg)
        || is_type::<i16>(arg)
        || is_type::<i32>(arg)
        || is_type::<i64>(arg)
        || is_type::<isize>(arg)
        || is_type::<f32>(arg)
        || is_type::<f64>(arg)
}

fn convert(text: &str, kind: ParamType) -> Value {
    match kind {
        ParamType::Number => text
            .parse::<f64>()
            .ok()
            .and_then(crate::js::number_value)
            .unwrap_or_else(|| Value::String(text.to_string())),
        ParamType::Boolean => Value::Bool(text == "true"),
        _ => Value::String(text.to_string()),
    }
}

fn help_text(arg: &Arg) -> Option<String> {
    arg.get_help()
        .or_else(|| arg.get_long_help())
        .map(|h| h.to_string().lines().map(str::trim).collect::<Vec<_>>().join(" "))
        .filter(|h| !h.trim().is_empty())
}

/// The param and binding of one clap arg; `None` for args that are never published.
fn param_for(arg: &Arg, level: usize) -> Option<(ParamSpec, Binding)> {
    let action = arg.get_action();
    let kind = match action {
        ArgAction::SetTrue | ArgAction::SetFalse => ValueKind::Flag,
        ArgAction::Count => ValueKind::Count,
        ArgAction::Set | ArgAction::Append => ValueKind::Value,
        _ => return None, // help, version
    };
    let positional = arg.is_positional();
    let name = match (positional, arg.get_long()) {
        (false, Some(long)) => camel(long),
        _ => camel(arg.get_id().as_str()),
    };
    let slot = if positional {
        Slot::Positional {
            index: arg.get_index().unwrap_or(0),
            last: arg.is_last_set(),
        }
    } else if let Some(long) = arg.get_long() {
        Slot::Long(long.to_string())
    } else {
        Slot::Short(arg.get_short()?)
    };
    let max_values = arg.get_num_args().map(|r| r.max_values()).unwrap_or(1);
    let append = matches!(action, ArgAction::Append);
    let multi = kind == ValueKind::Value && (append || max_values > 1);
    let enum_values: Vec<Value> = if kind == ValueKind::Value && !is_type::<bool>(arg) {
        arg.get_possible_values()
            .into_iter()
            .filter(|v| !v.is_hide_set())
            .map(|v| Value::String(v.get_name().to_string()))
            .collect()
    } else {
        Vec::new()
    };
    let elem = match kind {
        ValueKind::Flag => ParamType::Boolean,
        ValueKind::Count => ParamType::Number,
        ValueKind::Value if is_type::<bool>(arg) => ParamType::Boolean,
        ValueKind::Value if is_number(arg) => ParamType::Number,
        ValueKind::Value => ParamType::String,
    };
    let required = arg.is_required_set();
    let occurs = match (multi, required, kind) {
        (_, _, ValueKind::Flag | ValueKind::Count) => Occurs::Optional,
        (true, true, _) => Occurs::OneOrMore,
        (true, false, _) => Occurs::ZeroOrMore,
        (false, true, _) => Occurs::One,
        (false, false, _) => Occurs::Optional,
    };
    let mut spec = ParamSpec::new(name.clone(), elem).occurs(occurs);
    if multi {
        spec.kind = ParamType::Array;
        spec.items = Some(match elem {
            ParamType::Number => ItemType::Number,
            ParamType::Boolean => ItemType::Boolean,
            _ => ItemType::String,
        });
    }
    if !enum_values.is_empty() {
        spec.enum_values = Some(enum_values);
    }
    if positional {
        spec.positional = Some(true);
    }
    let mut aliases: Vec<String> = Vec::new();
    if !positional {
        if let Some(short) = arg.get_short() {
            if arg.get_long().is_some() {
                aliases.push(short.to_string());
            }
        }
        for alias in arg.get_visible_aliases().unwrap_or_default() {
            aliases.push(alias.to_string());
        }
        for alias in arg.get_visible_short_aliases().unwrap_or_default() {
            aliases.push(alias.to_string());
        }
    }
    if !aliases.is_empty() {
        spec.aliases = Some(aliases);
    }
    spec.description = help_text(arg);
    let delimiter = arg.get_value_delimiter();
    let default = if kind == ValueKind::Value && !arg.get_default_values().is_empty() {
        let texts: Vec<String> = arg
            .get_default_values()
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        if multi {
            let items: Vec<Value> = texts
                .iter()
                .flat_map(|t| match delimiter {
                    Some(d) => t.split(d).map(str::to_string).collect::<Vec<_>>(),
                    None => vec![t.clone()],
                })
                .map(|t| convert(&t, elem))
                .collect();
            Some(Value::Array(items))
        } else {
            texts.first().map(|t| convert(t, elem))
        }
    } else {
        None
    };
    spec.default = default.clone();
    Some((
        spec,
        Binding {
            param: name,
            level,
            slot,
            kind,
            multi,
            append,
            delimiter,
            default,
            stdin_token: None,
        },
    ))
}

/// Plans for every overlay op, in overlay order (the published order).
pub fn plans(root: &Command, overlay: &Overlay, tool: &str) -> Result<Vec<OpPlan>, String> {
    let mut root = root.clone();
    root.build();
    let problems = overlay.problems();
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    overlay.ops.iter().map(|op| plan(&root, overlay, op, tool)).collect()
}

/// The plan of one op; `root` must be built (`Command::build`).
pub(crate) fn plan(root: &Command, overlay: &Overlay, op: &OverlayOp, tool: &str) -> Result<OpPlan, String> {
    let first = op.command.split_whitespace().next().unwrap_or_default();
    if overlay.excluded.iter().any(|e| e == &op.command || e == first) {
        return Err(format!("op {}: command `{}` is excluded", op.name, op.command));
    }
    let words: Vec<String> = op.command.split_whitespace().map(str::to_string).collect();
    let chain = find_path(root, &words).ok_or_else(|| format!("op {}: no clap command `{}`", op.name, op.command))?;
    if words.is_empty() {
        return Err(format!("op {}: empty command", op.name));
    }
    let leaf = *chain.last().expect("chain has the root");
    if leaf.get_subcommands().any(|c| c.get_name() != "help") {
        return Err(format!(
            "op {}: `{}` is a command group, not a leaf",
            op.name, op.command
        ));
    }
    let root_args: Vec<&str> = chain[0].get_arguments().map(|a| a.get_id().as_str()).collect();
    let excluded = |id: &str| {
        overlay.exclude_params.iter().chain(&op.exclude_params).any(|e| e == id)
            || (!op.common_options && root_args.contains(&id))
            || overlay.only_for.get(id).is_some_and(|ops| !ops.contains(&op.name))
    };
    let mut params: Vec<ParamSpec> = Vec::new();
    let mut bindings: Vec<Binding> = Vec::new();
    let mut fixed_args = op.fixed_args.clone();
    // chain[0] is the root: its global args reappear on the leaf after `build`.
    for (depth, cmd) in chain.iter().enumerate().skip(1) {
        let level = depth - 1;
        let is_leaf = depth == chain.len() - 1;
        let mut args: Vec<&Arg> = cmd.get_arguments().collect();
        // Options in declaration order, then positionals in index order (stable sort).
        args.sort_by_key(|a| (a.is_positional(), a.get_index().unwrap_or(0)));
        for arg in args {
            if arg.is_hide_set() || excluded(arg.get_id().as_str()) || (arg.is_global_set() && !is_leaf) {
                continue;
            }
            // A required on/off flag is a marker the legacy command always needs: a published
            // boolean could only ever be `true`, so it becomes a fixed arg.
            if is_leaf && arg.is_required_set() && matches!(arg.get_action(), ArgAction::SetTrue) {
                if let Some(long) = arg.get_long() {
                    fixed_args.push(format!("--{long}"));
                    continue;
                }
            }
            let Some((spec, binding)) = param_for(arg, level) else {
                continue;
            };
            if params.iter().any(|p| p.name == spec.name) {
                continue; // a local arg shadows a propagated global of the same name
            }
            params.push(spec);
            bindings.push(binding);
        }
    }
    for (name, setting) in &op.params {
        match params.iter().position(|p| &p.name == name) {
            Some(i) => {
                if let Some(text) = &setting.description {
                    params[i].description = Some(text.clone());
                }
                if setting.list {
                    return Err(format!(
                        "op {}: param {name}: `list` is only for a param that is not a clap arg",
                        op.name
                    ));
                }
                if setting.stdin {
                    bindings[i].stdin_token = Some(match setting.stdin_token.as_deref() {
                        None => Some("-".to_string()),
                        Some("") => None,
                        Some(token) => Some(token.to_string()),
                    });
                }
            }
            None => {
                if !setting.stdin {
                    return Err(format!(
                        "op {}: param {name} is not a clap arg of `{}`",
                        op.name, op.command
                    ));
                }
                let Some(description) = &setting.description else {
                    return Err(format!(
                        "op {}: stdin param {name} is not a clap arg and needs a description",
                        op.name
                    ));
                };
                let spec = if setting.list {
                    ParamSpec::new(name.clone(), ParamType::Array)
                        .items(ItemType::String)
                        .occurs(Occurs::OneOrMore)
                } else {
                    ParamSpec::new(name.clone(), ParamType::String)
                };
                params.push(spec.describe(description.clone()));
                bindings.push(Binding {
                    param: name.clone(),
                    level: words.len() - 1,
                    slot: Slot::StdinOnly,
                    kind: ValueKind::Value,
                    multi: setting.list,
                    append: false,
                    delimiter: None,
                    default: None,
                    stdin_token: Some(None),
                });
            }
        }
    }
    let inline = inline_plan(overlay, op, &mut params, &bindings)?;
    if let Some(rf) = &op.result_file {
        let at = params.iter().position(|p| p.name == rf.param).ok_or_else(|| {
            format!(
                "op {}: result_file param {} is not a clap arg of `{}`",
                op.name, rf.param, op.command
            )
        })?;
        let b = &bindings[at];
        if b.multi || b.kind != ValueKind::Value || b.slot == Slot::StdinOnly {
            return Err(format!(
                "op {}: result_file param {} must take a single value",
                op.name, rf.param
            ));
        }
        // Supplied by the bridge on every call: not published.
        params.remove(at);
    }
    if overlay.named_positionals {
        for spec in &mut params {
            spec.positional = None;
        }
    }
    let description = op
        .description
        .clone()
        .or_else(|| leaf.get_about().map(|a| a.to_string()))
        .unwrap_or_else(|| format!("{tool} {}", op.command));
    let access = access_of(op.access);
    let mut schema = OperationSchema::new(op.name.clone())
        .access(access)
        .describe(description);
    schema.returns = Some(ReturnType::Object);
    schema.params = params;
    Ok(OpPlan {
        name: op.name.clone(),
        words,
        access,
        dangerous: op.dangerous,
        schema,
        bindings,
        fixed_args,
        legacy_json: op.legacy_json,
        inline,
        result_file: op.result_file.clone(),
    })
}

/// Apply `inline_inputs` to one op: the input becomes optional and the forms are added.
fn inline_plan(
    overlay: &Overlay,
    op: &OverlayOp,
    params: &mut Vec<ParamSpec>,
    bindings: &[Binding],
) -> Result<Option<InlinePlan>, String> {
    let targets: Vec<&Binding> = bindings
        .iter()
        .filter(|b| b.slot != Slot::StdinOnly && overlay.inline_inputs.contains_key(&b.param))
        .collect();
    let target = match targets.as_slice() {
        [] => return Ok(None),
        [one] => *one,
        many => {
            let names: Vec<&str> = many.iter().map(|b| b.param.as_str()).collect();
            return Err(format!(
                "op {}: more than one inline input ({})",
                op.name,
                names.join(", ")
            ));
        }
    };
    if target.multi || target.kind != ValueKind::Value {
        return Err(format!(
            "op {}: inline input {} must be a single value",
            op.name, target.param
        ));
    }
    let inline = &overlay.inline_inputs[&target.param];
    let applicable: Vec<_> = inline
        .forms
        .iter()
        .filter(|f| f.ops.as_ref().map_or(true, |ops| ops.contains(&op.name)))
        .collect();
    if applicable.is_empty() {
        return Ok(None);
    }
    if applicable.iter().any(|f| f.via == InlineVia::Stdin) && bindings.iter().any(|b| b.stdin_token.is_some()) {
        return Err(format!(
            "op {}: inline input {} and a stdin param both need stdin",
            op.name, target.param
        ));
    }
    let spec = params.iter_mut().find(|p| p.name == target.param).expect("bound param");
    let required = spec.occurs.unwrap_or(Occurs::One) == Occurs::One;
    spec.occurs = Some(Occurs::Optional);
    if spec.description.is_none() {
        spec.description = inline.description.clone();
    }
    let mut forms = Vec::new();
    for form in applicable {
        if params.iter().any(|p| p.name == form.param) {
            return Err(format!(
                "op {}: inline param {} clashes with a param of the op",
                op.name, form.param
            ));
        }
        let mut spec = match form.kind {
            InlineKind::Records => ParamSpec::new(form.param.clone(), ParamType::Array).items(ItemType::Object),
            InlineKind::Text => ParamSpec::new(form.param.clone(), ParamType::String),
            InlineKind::Json => ParamSpec::new(form.param.clone(), ParamType::Json),
            InlineKind::Toml => ParamSpec::new(form.param.clone(), ParamType::Object),
        }
        .occurs(Occurs::Optional);
        spec.description = form.description.clone();
        params.push(spec);
        forms.push(InlineFormPlan {
            param: form.param.clone(),
            kind: form.kind,
            args: form.args.clone(),
            via: form.via,
            file_name: form.file_name.clone(),
        });
    }
    Ok(Some(InlinePlan {
        target: target.param.clone(),
        required,
        token: inline.token.clone(),
        forms,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn camel_case() {
        assert_eq!(camel("exit-zero-on-no-match"), "exitZeroOnNoMatch");
        assert_eq!(camel("vars_file"), "varsFile");
        assert_eq!(camel("path"), "path");
        assert_eq!(camel("_x"), "x");
    }

    fn toy() -> Command {
        Command::new("toy")
            .arg(
                Arg::new("verbose")
                    .short('v')
                    .long("verbose")
                    .action(ArgAction::Count)
                    .global(true),
            )
            .subcommand(
                Command::new("grp")
                    .arg(Arg::new("repo").long("repo").default_value(".").global(true))
                    .subcommand(
                        Command::new("leaf")
                            .about("A leaf")
                            .arg(
                                Arg::new("cols")
                                    .long("col")
                                    .value_delimiter(',')
                                    .action(ArgAction::Append)
                                    .default_value("a,b"),
                            )
                            .arg(Arg::new("files").long("files").num_args(1..))
                            .arg(Arg::new("flag").long("flag").value_parser(clap::value_parser!(bool)))
                            .arg(Arg::new("secret").long("secret").hide(true))
                            .arg(
                                Arg::new("marker")
                                    .long("marker")
                                    .action(ArgAction::SetTrue)
                                    .required(true),
                            )
                            .arg(Arg::new("target").required(true)),
                    ),
            )
            .subcommand(Command::new("solo").arg(Arg::new("x").short('x')))
    }

    fn overlay(text: &str) -> Overlay {
        Overlay::parse(text).unwrap()
    }

    #[test]
    fn generates_params_and_bindings() {
        let o = overlay("[[op]]\nname = \"grp_leaf\"\ncommand = \"grp leaf\"\naccess = \"read\"\n");
        let p = &plans(&toy(), &o, "toy").unwrap()[0];
        let doc = serde_json::to_value(&p.schema).unwrap();
        let names: Vec<&str> = doc["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["col", "files", "flag", "repo", "verbose", "target"],
            "own args, propagated globals, positionals"
        );
        let by = |n: &str| {
            doc["params"]
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["name"] == n)
                .unwrap()
                .clone()
        };
        assert_eq!(by("col")["default"], json!(["a", "b"]));
        assert_eq!(by("files")["type"], "array");
        assert_eq!(by("flag")["type"], "boolean");
        assert_eq!(p.fixed_args, ["--marker"]);
        let col = p.bindings.iter().find(|b| b.param == "col").unwrap();
        assert_eq!((col.append, col.delimiter), (true, Some(',')));
        let files = p.bindings.iter().find(|b| b.param == "files").unwrap();
        assert_eq!((files.multi, files.append, files.delimiter), (true, false, None));
        assert_eq!(p.bindings.iter().find(|b| b.param == "repo").unwrap().level, 1);
    }

    #[test]
    fn overlay_params_and_errors() {
        let o = overlay(
            "[[op]]\nname = \"s\"\ncommand = \"solo\"\naccess = \"write\"\n[op.params.x]\nstdin = true\nstdin_token = \"\"\n\
             description = \"X on stdin\"\n[op.params.extra]\nstdin = false\n",
        );
        assert!(plans(&toy(), &o, "toy")
            .unwrap_err()
            .contains("param extra is not a clap arg"));
        let o = overlay("[[op]]\nname = \"s\"\ncommand = \"solo\"\naccess = \"write\"\n[op.params.x]\nstdin = true\nstdin_token = \"\"\n");
        let p = &plans(&toy(), &o, "toy").unwrap()[0];
        assert_eq!(p.stdin_binding().unwrap().stdin_token, Some(None));
        let o =
            overlay("[[op]]\nname = \"s\"\ncommand = \"solo\"\naccess = \"write\"\n[op.params.body]\nstdin = true\n");
        assert!(plans(&toy(), &o, "toy").unwrap_err().contains("needs a description"));
        let o = overlay("[[op]]\nname = \"g\"\ncommand = \"grp\"\naccess = \"read\"\n");
        assert!(plans(&toy(), &o, "toy").unwrap_err().contains("command group"));
        let o = overlay("excluded = [\"solo\"]\n[[op]]\nname = \"s\"\ncommand = \"solo\"\naccess = \"read\"\n");
        assert!(plans(&toy(), &o, "toy").unwrap_err().contains("excluded"));
        let o = overlay("[[op]]\nname = \"s\"\ncommand = \"\"\naccess = \"read\"\n");
        assert!(plans(&toy(), &o, "toy").unwrap_err().contains("empty command"));
    }

    #[test]
    fn inline_input_errors() {
        let cmd = Command::new("t").subcommand(
            Command::new("go")
                .arg(Arg::new("left").required(true))
                .arg(Arg::new("right").required(true))
                .arg(Arg::new("many").long("many").action(ArgAction::Append))
                .arg(Arg::new("records").long("records")),
        );
        let form = |input: &str, param: &str| {
            format!("[inline_inputs.{input}]\n[[inline_inputs.{input}.form]]\nparam = \"{param}\"\nkind = \"text\"\n")
        };
        let op = "[[op]]\nname = \"go_op\"\ncommand = \"go\"\naccess = \"read\"\n";
        let err = |extra: &str| plans(&cmd, &overlay(&format!("{op}{extra}")), "t").unwrap_err();
        assert!(err(&format!("{}{}", form("left", "a"), form("right", "b")))
            .contains("more than one inline input (left, right)"));
        assert!(err(&form("many", "a")).contains("must be a single value"));
        assert!(err(&form("left", "records")).contains("inline param records clashes"));
        assert!(err(&format!("[op.params.right]\nstdin = true\n{}", form("left", "a"))).contains("both need stdin"));
        let with_doc = "[inline_inputs.left]\ndescription = \"Table path\"\n[[inline_inputs.left.form]]\nparam = \"a\"\nkind = \"text\"\n";
        let documented = plans(&cmd, &overlay(&format!("{op}{with_doc}")), "t").unwrap();
        let left = documented[0].schema.params.iter().find(|p| p.name == "left").unwrap();
        assert_eq!(
            left.description.as_deref(),
            Some("Table path"),
            "used when clap has no help"
        );
        let ok = plans(
            &cmd,
            &overlay(&format!("named_positionals = true\n{op}{}", form("left", "a"))),
            "t",
        )
        .unwrap();
        let inline = ok[0].inline.as_ref().unwrap();
        assert_eq!(
            (inline.target.as_str(), inline.required, inline.token.as_str()),
            ("left", true, "-")
        );
        let doc = serde_json::to_value(&ok[0].schema).unwrap();
        let left = doc["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "left")
            .unwrap()
            .clone();
        assert_eq!(left["occurs"], "?", "the input is optional once it has inline forms");
        assert!(doc["params"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p.get("positional").is_none()));
        assert_eq!(doc["params"].as_array().unwrap().last().unwrap()["name"], "a");
    }

    #[test]
    fn leaf_paths_skip_help() {
        let mut cmd = toy();
        cmd.build();
        assert_eq!(
            leaf_paths(&cmd),
            [vec!["grp".to_string(), "leaf".to_string()], vec!["solo".to_string()]]
        );
    }
}
