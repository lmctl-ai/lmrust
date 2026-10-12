//! Clap bridge (cargo feature `clap`): a clap command tree plus an overlay TOML becomes a
//! native lmapi tool (lmapi protocol section 6.1) whose operations self-exec the legacy CLI.
//!
//! A tool crate needs its `clap::Command`, an overlay (embedded with `include_str!`) and a few
//! lines of `main`:
//!
//! ```no_run
//! # fn command() -> clap::Command { clap::Command::new("mytool").subcommand(clap::Command::new("scan")) }
//! # const OVERLAY: &str = "[[op]]\nname = \"scan_host\"\ncommand = \"scan\"\naccess = \"read\"\n";
//! use lmrust::clap_bridge::Bridge;
//!
//! fn main() {
//!     // OVERLAY = include_str!("../lmapi/overlay.toml") in a real tool.
//!     let bridge = Bridge::new(command(), OVERLAY).expect("lmapi overlay").version(env!("CARGO_PKG_VERSION"));
//!     if let Some(code) = bridge.maybe_run(std::env::args_os().skip(1)) {
//!         std::process::exit(code);
//!     }
//!     // ... the tool's own clap parsing continues here, unchanged.
//! }
//! ```
//!
//! - **Schema**: one operation per overlay `[[op]]`, generated from the clap command at its
//!   `command` path (flags, shorts, visible aliases, occurrence, number/boolean/enum types,
//!   defaults, positionals, help text). Output is deterministic: ops in overlay order, params in
//!   the built clap order (the leaf's own options, then propagated globals) with positionals last.
//! - **Calls**: validated params are turned back into the legacy argv ([`legacy_argv`]) and
//!   `std::env::current_exe()` (or [`Bridge::exe`]) runs with it. The overlay's
//!   `legacy_json_args` (the tool's own JSON switch) is appended; JSON on the child's stdout is
//!   the result, other output is returned as `{"<text_key>": "<stdout>"}` (default `text`).
//! - **Failures**: a non-success exit becomes a standard lmapi error (overlay `exit_codes`,
//!   default `COMMAND_FAILED`) with `data.tool_code = "EXIT_<n>"`; a child killed by a signal is
//!   `INTERNAL_ERROR` / `SIGNALED` even if it printed a result.
//! - **Coverage**: [`assert_coverage`] in the tool's tests keeps clap and the overlay in step.
//!
//! See `durable-memory/clap-bridge.md` for the overlay reference.

mod child;
mod coverage;
mod generate;
mod overlay;

pub use coverage::{assert_coverage, coverage_problems};
pub use generate::{camel, plans, Binding, InlineFormPlan, InlinePlan, OpPlan, Slot, ValueKind};
pub use overlay::{
    InlineForm, InlineInput, InlineKind, InlineVia, Overlay, OverlayAccess, OverlayOp, OverlayParam, ResultFile,
    ResultFormat,
};

use crate::server::{
    check_policy, is_lmapi_invocation, print_error, run_cli_io, CheckContext, CliError, CliSpec, ErrorCode, OpDef,
    Policy,
};
use crate::types::{Access, PublishedCliSchema, Record};
use clap::Command;
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

/// Set in the environment of every self-exec child: [`Bridge::maybe_run`] returns `None` when it
/// is present, so the legacy argv a child receives is never taken for an lmapi call.
pub const CHILD_ENV: &str = "LMRUST_CLAP_BRIDGE_CHILD";

type DynamicAccess = Rc<dyn Fn(&Record) -> Access>;

/// A clap tool served as an lmapi tool.
#[derive(Clone)]
pub struct Bridge {
    command: Command,
    overlay: Overlay,
    tool: String,
    version: Option<String>,
    exe: Option<PathBuf>,
    dynamic: HashMap<String, DynamicAccess>,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("tool", &self.tool)
            .field("version", &self.version)
            .field("exe", &self.exe)
            .field("overlay", &self.overlay)
            .finish_non_exhaustive()
    }
}

impl Bridge {
    /// Parse and check the overlay. Op plans are generated lazily (legacy argv never pays for
    /// the clap walk).
    pub fn new(command: Command, overlay_toml: &str) -> Result<Bridge, String> {
        let overlay = Overlay::parse(overlay_toml)?;
        let tool = overlay.tool.clone().unwrap_or_else(|| command.get_name().to_string());
        Ok(Bridge {
            command,
            overlay,
            tool,
            version: None,
            exe: None,
            dynamic: HashMap::new(),
        })
    }

    /// Published version (default: the clap command's version, else `"unknown"`).
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Executable to self-exec (default `std::env::current_exe()`).
    pub fn exe(mut self, path: impl Into<PathBuf>) -> Self {
        self.exe = Some(path.into());
        self
    }

    /// Decide the access of a `dynamic` op per call (for example a query that may contain a
    /// mutation). Without a hook a dynamic op is treated as a write.
    pub fn dynamic_access<F>(mut self, op: &str, decide: F) -> Self
    where
        F: Fn(&Record) -> Access + 'static,
    {
        self.dynamic.insert(op.to_string(), Rc::new(decide));
        self
    }

    /// The checked overlay.
    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// Operation plans, in overlay order.
    pub fn plans(&self) -> Result<Vec<OpPlan>, String> {
        plans(&self.command, &self.overlay, &self.tool)
    }

    fn published_version(&self) -> String {
        self.version
            .clone()
            .or_else(|| self.command.get_version().map(str::to_string))
            .unwrap_or_else(|| "unknown".into())
    }

    fn base_spec(&self) -> CliSpec<Policy> {
        let mut spec = CliSpec::with_policy(self.tool.clone())
            .version(self.published_version())
            .inputs(self.overlay.inputs.iter().copied());
        if let Some(text) = self
            .overlay
            .description
            .clone()
            .or_else(|| self.command.get_about().map(|a| a.to_string()))
        {
            spec = spec.description(text);
        }
        for global in &self.overlay.drop_globals {
            spec = spec.without_global(global);
        }
        spec
    }

    /// The lmapi server spec: one op per plan, policy flags as handler context.
    pub fn spec(&self) -> Result<CliSpec<Policy>, String> {
        let mut spec = self.base_spec();
        let bridge = Rc::new(self.clone());
        for plan in self.plans()? {
            let plan = Rc::new(plan);
            let call = Call {
                plan: plan.clone(),
                bridge: bridge.clone(),
            };
            spec = spec.op(OpDef::new(
                plan.name.clone(),
                plan.access,
                plan.schema.description.clone().unwrap_or_default(),
                move |params, policy| call.run(params, policy),
            )
            .schema(plan.schema.clone()));
        }
        Ok(spec)
    }

    /// The published `cli.schema.v1` document (`<tool> --schema`).
    pub fn schema(&self) -> Result<PublishedCliSchema, String> {
        Ok(crate::server::schema_document(&self.spec()?))
    }

    /// Whether `argv` is lmapi-shaped for this tool (same rule as
    /// [`crate::server::is_lmapi_invocation`]), decided from the overlay alone.
    pub fn is_lmapi_invocation(&self, argv: &[String]) -> bool {
        let mut stub = self.base_spec();
        for op in &self.overlay.ops {
            stub = stub.op(OpDef::new(op.name.clone(), Access::Read, "", |_, _| Ok(Value::Null)));
        }
        is_lmapi_invocation(&stub, argv)
    }

    /// Serve lmapi-shaped `args` (the arguments after the binary name) on the process stdout and
    /// return the exit code; `None` leaves the invocation to the legacy parser (also always
    /// inside a self-exec child and for non-UTF-8 argv).
    pub fn maybe_run<I, S>(&self, args: I) -> Option<i32>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let argv: Vec<String> = args
            .into_iter()
            .map(|a| a.into().into_string().ok())
            .collect::<Option<_>>()?;
        let env: HashMap<String, String> = std::env::vars().collect();
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        let code = self.maybe_run_io(&argv, &mut lock, &env);
        let _ = lock.flush();
        code
    }

    /// [`Bridge::maybe_run`] with an explicit output stream and environment (policy env floors
    /// such as `<TOOL>_READONLY` are read from `env`).
    pub fn maybe_run_io(&self, argv: &[String], out: &mut dyn Write, env: &HashMap<String, String>) -> Option<i32> {
        if std::env::var_os(CHILD_ENV).is_some() || !self.is_lmapi_invocation(argv) {
            return None;
        }
        match self.spec() {
            Ok(spec) => Some(run_cli_io(&spec, argv, out, env)),
            Err(problem) => {
                let err = CliError::new(ErrorCode::InternalError, format!("lmapi overlay: {problem}"));
                print_error(out, &err);
                Some(err.exit_code())
            }
        }
    }

    /// The full argv a call of `plan` passes to the executable: [`legacy_argv`] plus the
    /// overlay's `legacy_root_args` and `legacy_json_args`.
    /// Temp paths (result files, file-delivered inline values) appear under `<tmp>`.
    pub fn child_argv(&self, plan: &OpPlan, params: &Record) -> Result<Vec<String>, CliError> {
        Ok(child_call(plan, &self.overlay, params, Path::new("<tmp>"), false)?.0)
    }

    fn executable(&self) -> Result<PathBuf, CliError> {
        match &self.exe {
            Some(path) => Ok(path.clone()),
            None => std::env::current_exe().map_err(|e| {
                CliError::new(
                    ErrorCode::InternalError,
                    format!("cannot locate the {} executable: {e}", self.tool),
                )
            }),
        }
    }
}

struct Call {
    plan: Rc<OpPlan>,
    bridge: Rc<Bridge>,
}

impl Call {
    fn run(&self, params: &Record, policy: &Policy) -> Result<Value, CliError> {
        let bridge = &self.bridge;
        let plan = &self.plan;
        let temp = TempDir::new()?;
        let (argv, inline_stdin) = child_call(plan, &bridge.overlay, params, temp.path(), true)?;
        let access = match plan.access {
            Access::Dynamic => bridge
                .dynamic
                .get(&plan.name)
                .map_or(Access::Write, |decide| decide(params)),
            access => access,
        };
        if access != Access::Read {
            let command = std::iter::once(bridge.tool.clone()).chain(argv.iter().cloned());
            let ctx = CheckContext::new(plan.name.clone())
                .command(command)
                .dangerous(plan.dangerous);
            if let Some(simulated) = check_policy(Access::Write, policy, &ctx)? {
                return Ok(Value::Object(simulated));
            }
        }
        let stdin = inline_stdin.or_else(|| {
            plan.stdin_binding()
                .and_then(|b| params.get(&b.param))
                .and_then(stdin_text)
        });
        let limits = child::Limits {
            max_stdout: bridge.overlay.max_stdout_bytes,
            max_stderr: bridge.overlay.max_stderr_bytes,
            timeout: bridge.overlay.timeout_ms.map(Duration::from_millis),
        };
        let run = child::run(&bridge.executable()?, &argv, stdin, limits).map_err(|e| match e {
            child::SpawnError::Input(message) => CliError::new(ErrorCode::ValidationError, message),
            child::SpawnError::Internal(message) => CliError::new(ErrorCode::InternalError, message),
        })?;
        let value = outcome(&bridge.tool, &bridge.overlay, run)?;
        match &plan.result_file {
            Some(rf) => read_result_file(temp.path(), rf, &bridge.overlay.text_key),
            None => Ok(value),
        }
    }
}

static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A private temp directory for one call (result files, file-delivered inline values), removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<TempDir, CliError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("lmrust-clap-bridge-{}-{nanos}-{n}", std::process::id()));
        std::fs::create_dir(&dir)
            .map_err(|e| CliError::new(ErrorCode::InternalError, format!("cannot create a temp directory: {e}")))?;
        Ok(TempDir(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Parse the op's result file after a successful run.
fn read_result_file(dir: &Path, rf: &ResultFile, text_key: &str) -> Result<Value, CliError> {
    let name = rf.read.as_deref().or(rf.name.as_deref()).unwrap_or_default();
    let path = dir.join(name);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        CliError::new(
            ErrorCode::CommandFailed,
            format!("the command did not write its result file {name}: {e}"),
        )
        .tool_code("NO_RESULT_FILE")
    })?;
    let parsed = match rf.format {
        ResultFormat::Json => serde_json::from_str(&text).map_err(|e| e.to_string()),
        ResultFormat::Csv => parse_csv(&text),
        ResultFormat::Text => {
            let mut out = Record::new();
            out.insert(text_key.to_string(), Value::String(text.clone()));
            Ok(Value::Object(out))
        }
    };
    parsed.map_err(|e| {
        CliError::new(
            ErrorCode::CommandFailed,
            format!("cannot parse the result file {name}: {e}"),
        )
        .tool_code("BAD_RESULT_FILE")
    })
}

/// RFC 4180 CSV with a header row as an array of row objects. Cells: empty is null, `true`/`false`
/// booleans, finite numbers numbers, anything else a string.
pub fn parse_csv(text: &str) -> Result<Value, String> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut row));
                any = false;
            }
            _ => field.push(c),
        }
    }
    if quoted {
        return Err("unterminated quoted field".into());
    }
    if any {
        row.push(field);
        records.push(row);
    }
    let mut records = records.into_iter();
    let header = records.next().ok_or("empty CSV (no header row)")?;
    let mut out = Vec::new();
    for (i, cells) in records.enumerate() {
        if cells.len() != header.len() {
            return Err(format!(
                "row {} has {} cells, the header has {}",
                i + 2,
                cells.len(),
                header.len()
            ));
        }
        let mut obj = Record::new();
        for (key, cell) in header.iter().zip(cells) {
            obj.insert(key.clone(), csv_cell(cell));
        }
        out.push(Value::Object(obj));
    }
    Ok(Value::Array(out))
}

fn csv_cell(cell: String) -> Value {
    match cell.as_str() {
        "" => Value::Null,
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => match cell
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .and_then(crate::js::number_value)
        {
            Some(n) if !cell.starts_with('+') && !cell.contains(['x', 'X']) => n,
            _ => Value::String(cell),
        },
    }
}

fn value_text(value: &Value) -> String {
    crate::js::to_display(value)
}

/// Stdin text of a stdin param: a string as is, an array one item per line.
fn stdin_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Array(items) => Some(items.iter().map(|v| format!("{}\n", value_text(v))).collect()),
        other => Some(value_text(other)),
    }
}

/// Legacy argv for validated params, without the overlay's root and JSON args: command words
/// with each level's flags, the op's fixed args, then positionals in clap index order (after
/// `--` when a positional starts with `-`, is a `last` positional, or a multi-value option
/// could swallow it).
///
/// Values equal to the published default are left out (clap applies the same default). Comma
/// lists (`value_delimiter`) are joined into one argument; an item containing the delimiter is
/// a `VALIDATION_ERROR`.
pub fn legacy_argv(plan: &OpPlan, params: &Record) -> Result<Vec<String>, CliError> {
    build_argv(plan, params, &[], &[], &[])
}

/// The inline form a call uses: `Ok(None)` when the input itself (or nothing optional) is given.
/// Exactly one of the input and its forms may be present; one is needed when the input is required.
fn chosen_inline<'p>(inline: &'p InlinePlan, params: &Record) -> Result<Option<&'p InlineFormPlan>, CliError> {
    let present = |name: &str| params.get(name).is_some_and(|v| !v.is_null());
    let names: Vec<&str> = std::iter::once(inline.target.as_str())
        .chain(inline.forms.iter().map(|f| f.param.as_str()))
        .collect();
    let given: Vec<&str> = names.iter().copied().filter(|n| present(n)).collect();
    if given.len() > 1 {
        return Err(CliError::new(
            ErrorCode::ValidationError,
            format!("give only one of {} (got {})", names.join(", "), given.join(", ")),
        )
        .with("params", Value::from(given)));
    }
    if given.is_empty() && inline.required {
        return Err(CliError::new(
            ErrorCode::ValidationError,
            format!("one of {} is required", names.join(", ")),
        ));
    }
    Ok(inline.forms.iter().find(|f| present(&f.param)))
}

/// Text of an inline value: records and JSON as compact JSON, text as is, TOML from an object.
fn inline_text(kind: InlineKind, param: &str, value: &Value) -> Result<String, CliError> {
    Ok(match (kind, value) {
        (InlineKind::Text, Value::String(text)) => text.clone(),
        (InlineKind::Text, other) => value_text(other),
        (InlineKind::Toml, other) => toml::to_string(other).map_err(|e| {
            CliError::new(
                ErrorCode::ValidationError,
                format!("{param} cannot be written as TOML: {e}"),
            )
            .with("param", Value::String(param.to_string()))
        })?,
        (_, other) => serde_json::to_string(other).unwrap_or_default(),
    })
}

/// The child's argv (with the overlay's root and JSON args) and the stdin of an inline input.
fn child_call(
    plan: &OpPlan,
    overlay: &Overlay,
    params: &Record,
    temp: &Path,
    materialize: bool,
) -> Result<(Vec<String>, Option<String>), CliError> {
    let json: &[String] = if plan.legacy_json {
        &overlay.legacy_json_args
    } else {
        &[]
    };
    let mut chosen = Vec::new();
    for inline in &plan.inline {
        if let Some(form) = chosen_inline(inline, params)? {
            chosen.push((inline, form));
        }
    }
    let mut params = params.clone();
    if let Some(rf) = &plan.result_file {
        let path = rf.name.as_deref().map_or_else(|| temp.to_path_buf(), |n| temp.join(n));
        params.insert(rf.param.clone(), Value::String(path.to_string_lossy().into_owned()));
    }
    let mut stdin = None;
    let mut tokens = Vec::new();
    for (inline, f) in &chosen {
        let text = inline_text(f.kind, &f.param, &params[&f.param])?;
        match (f.via, &f.file_name) {
            (InlineVia::File, Some(name)) => {
                let path = temp.join(name);
                if materialize {
                    std::fs::write(&path, text).map_err(|e| {
                        CliError::new(
                            ErrorCode::InternalError,
                            format!("cannot write the inline {}: {e}", f.param),
                        )
                    })?;
                }
                tokens.push(path.to_string_lossy().into_owned());
            }
            _ => {
                stdin = Some(text);
                tokens.push(inline.token.clone());
            }
        }
    }
    let inline: Vec<InlineArgs> = chosen
        .iter()
        .zip(&tokens)
        .map(|((inline, f), token)| InlineArgs {
            target: &inline.target,
            token,
            args: &f.args,
        })
        .collect();
    Ok((
        build_argv(plan, &params, &overlay.legacy_root_args, json, &inline)?,
        stdin,
    ))
}

/// The argv side of an inline input in use.
struct InlineArgs<'a> {
    target: &'a str,
    token: &'a str,
    args: &'a [String],
}

fn build_argv(
    plan: &OpPlan,
    params: &Record,
    root: &[String],
    json: &[String],
    inline: &[InlineArgs<'_>],
) -> Result<Vec<String>, CliError> {
    let mut argv: Vec<String> = root.to_vec();
    let mut positionals: Vec<(&Binding, Vec<String>)> = Vec::new();
    let mut greedy = false;
    for (level, word) in plan.words.iter().enumerate() {
        argv.push(word.clone());
        for b in plan.bindings.iter().filter(|b| b.level == level) {
            if let Some(inline) = inline.iter().find(|i| i.target == b.param) {
                // The inline value goes to stdin; the input's slot gets the token (`-`).
                let token = inline.token.to_string();
                match &b.slot {
                    Slot::Positional { .. } => positionals.push((b, vec![token])),
                    Slot::Long(long) => argv.push(format!("--{long}={token}")),
                    Slot::Short(short) => argv.extend([format!("-{short}"), token]),
                    Slot::StdinOnly => {}
                }
                continue;
            }
            let Some(value) = params.get(&b.param) else {
                continue;
            };
            if value.is_null() || b.slot == Slot::StdinOnly {
                continue;
            }
            if let Some(token) = &b.stdin_token {
                // The value goes to stdin; the slot gets the token (`-`), or nothing.
                let Some(token) = token else { continue };
                match &b.slot {
                    Slot::Positional { .. } => positionals.push((b, vec![token.clone()])),
                    Slot::Long(long) => argv.push(format!("--{long}={token}")),
                    Slot::Short(short) => argv.extend([format!("-{short}"), token.clone()]),
                    Slot::StdinOnly => {}
                }
                continue;
            }
            if b.default.as_ref() == Some(value) {
                continue;
            }
            let values: Vec<String> = match value {
                Value::Array(items) if b.multi => items.iter().map(value_text).collect(),
                other => vec![value_text(other)],
            };
            if let Slot::Positional { .. } = b.slot {
                positionals.push((b, values));
                continue;
            }
            let flag = match &b.slot {
                Slot::Long(long) => format!("--{long}"),
                Slot::Short(short) => format!("-{short}"),
                _ => unreachable!("positional and stdin slots are handled above"),
            };
            match b.kind {
                ValueKind::Flag => {
                    if value == &Value::Bool(true) {
                        argv.push(flag);
                    }
                }
                ValueKind::Count => {
                    let times = value.as_u64().unwrap_or(0) as usize;
                    argv.extend(std::iter::repeat(flag).take(times));
                }
                ValueKind::Value => {
                    let joined;
                    let values: &[String] = match b.delimiter {
                        Some(d) if b.multi => {
                            if let Some(bad) = values.iter().find(|v| v.contains(d)) {
                                return Err(CliError::new(
                                    ErrorCode::ValidationError,
                                    format!("{}: item \"{bad}\" contains the list delimiter '{d}'", b.param),
                                )
                                .with("param", Value::String(b.param.clone())));
                            }
                            if values.is_empty() {
                                continue;
                            }
                            joined = [values.join(&d.to_string())];
                            &joined
                        }
                        _ => &values,
                    };
                    if b.multi && !b.append && b.delimiter.is_none() {
                        // `--files a b c` (num_args > 1): one flag, then the values.
                        argv.push(flag);
                        argv.extend(values.iter().cloned());
                        greedy = true;
                        continue;
                    }
                    for v in values {
                        match &b.slot {
                            Slot::Long(_) => argv.push(format!("{flag}={v}")),
                            _ => argv.extend([flag.clone(), v.clone()]),
                        }
                    }
                }
            }
        }
    }
    argv.extend(plan.fixed_args.iter().cloned());
    for i in inline {
        argv.extend(i.args.iter().cloned());
    }
    argv.extend(json.iter().cloned());
    positionals.sort_by_key(|(b, _)| match b.slot {
        Slot::Positional { index, .. } => index,
        _ => 0,
    });
    let needs_separator = greedy
        || positionals.iter().any(|(b, vs)| {
            let token_slot = b.stdin_token.is_some() || inline.iter().any(|i| i.target == b.param);
            matches!(b.slot, Slot::Positional { last: true, .. })
                || vs.iter().any(|v| v.starts_with('-') && !token_slot)
        });
    if needs_separator && !positionals.is_empty() {
        argv.push("--".into());
    }
    for (_, values) in positionals {
        argv.extend(values);
    }
    Ok(argv)
}

/// Rust's exit code for a panic (`panic = "unwind"` in `main`).
const PANIC_EXIT: i32 = 101;

/// Map a finished child to an lmapi result or error.
fn outcome(tool: &str, overlay: &Overlay, run: child::ChildRun) -> Result<Value, CliError> {
    let text = String::from_utf8_lossy(&run.stdout).into_owned();
    let parsed = serde_json::from_str::<Value>(text.trim()).ok();
    let stderr_line = run.stderr.trim().lines().last().map(str::to_string);
    if run.timed_out {
        let limit = overlay.timeout_ms.unwrap_or_default();
        return Err(
            CliError::new(ErrorCode::Timeout, format!("{tool} did not finish within {limit} ms"))
                .tool_code("TIMEOUT")
                .with("timeout_ms", Value::from(limit)),
        );
    }
    let Some(code) = run.exit_code else {
        // Killed by a signal: never a success, whatever it printed before dying.
        let mut err = CliError::new(
            ErrorCode::InternalError,
            stderr_line.unwrap_or_else(|| format!("{tool} was killed by a signal")),
        )
        .tool_code("SIGNALED");
        if let Some(signal) = run.signal {
            err = err.with("signal", Value::from(signal));
        }
        return Err(err);
    };
    if run.stdout_overflow {
        let limit = overlay.max_stdout_bytes;
        return Err(CliError::new(
            ErrorCode::CommandFailed,
            format!("{tool} wrote more than {limit} bytes to stdout"),
        )
        .tool_code("OUTPUT_TOO_LARGE")
        .with("limit", Value::from(limit))
        .with("legacy_exit_code", Value::from(code)));
    }
    if overlay.success_exit_codes.contains(&code) {
        return Ok(parsed.unwrap_or_else(|| {
            let mut result = Record::new();
            result.insert(overlay.text_key.clone(), Value::String(text));
            Value::Object(result)
        }));
    }
    let (standard, tool_code) = match overlay.mapped_exit(code) {
        Some(mapped) => (mapped, format!("EXIT_{code}")),
        None if code == PANIC_EXIT => (ErrorCode::InternalError, "PANIC".to_string()),
        None => (ErrorCode::CommandFailed, format!("EXIT_{code}")),
    };
    let mut err = CliError::new(
        standard,
        stderr_line.unwrap_or_else(|| format!("{tool} exited with code {code}")),
    )
    .tool_code(tool_code)
    .with("legacy_exit_code", Value::from(code));
    if !run.stderr.is_empty() {
        err = err.with("stderr", Value::String(run.stderr));
    }
    match parsed {
        Some(result) => err = err.with("result", result),
        None if !text.is_empty() => err = err.with(&overlay.text_key, Value::String(text)),
        None => {}
    }
    Err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Arg, ArgAction};
    use serde_json::json;

    fn run(code: Option<i32>, stdout: &str) -> child::ChildRun {
        child::ChildRun {
            exit_code: code,
            signal: if code.is_none() { Some(9) } else { None },
            stdout: stdout.as_bytes().to_vec(),
            stdout_overflow: false,
            stderr: String::new(),
            timed_out: false,
        }
    }

    fn overlay() -> Overlay {
        Overlay::parse("success_exit_codes = [0, 1]\n[exit_codes]\n\"2\" = \"CONFLICT\"\n").unwrap()
    }

    #[test]
    fn outcomes() {
        let o = overlay();
        assert_eq!(outcome("t", &o, run(Some(0), "{\"a\":1}\n")).unwrap(), json!({"a": 1}));
        assert_eq!(
            outcome("t", &o, run(Some(1), "plain")).unwrap(),
            json!({"text": "plain"})
        );
        let e = outcome("t", &o, run(Some(2), "")).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrorCode::Conflict, "t exited with code 2")
        );
        let e = outcome("t", &o, run(Some(PANIC_EXIT), "")).unwrap_err();
        assert_eq!(e.code, ErrorCode::InternalError);
        assert_eq!(e.data.unwrap()["tool_code"], "PANIC");
        let e = outcome("t", &o, run(None, "{\"status\":\"ok\"}")).unwrap_err();
        assert_eq!(e.code, ErrorCode::InternalError);
        let data = e.data.unwrap();
        assert_eq!(
            (data["tool_code"].clone(), data["signal"].clone()),
            (json!("SIGNALED"), json!(9))
        );
        let mut timed = run(None, "{}");
        timed.timed_out = true;
        assert_eq!(outcome("t", &o, timed).unwrap_err().code, ErrorCode::Timeout);
    }

    fn plan() -> OpPlan {
        let cmd = Command::new("t").subcommand(
            Command::new("go")
                .arg(Arg::new("files").long("files").num_args(1..))
                .arg(Arg::new("n").short('n').value_parser(clap::value_parser!(u8)))
                .arg(Arg::new("stdin").long("stdin"))
                .arg(Arg::new("target")),
        );
        let o = Overlay::parse(
            "[[op]]\nname = \"go_op\"\ncommand = \"go\"\naccess = \"read\"\n[op.params.stdin]\nstdin = true\n",
        )
        .unwrap();
        plans(&cmd, &o, "t").unwrap().remove(0)
    }

    #[test]
    fn greedy_options_force_the_separator_and_short_flags_take_separate_values() {
        let params = json!({"files": ["a", "b"], "n": 3, "target": "x", "stdin": "body"});
        let argv = legacy_argv(&plan(), params.as_object().unwrap()).unwrap();
        assert_eq!(argv, ["go", "--files", "a", "b", "-n", "3", "--stdin=-", "--", "x"]);
        assert_eq!(stdin_text(&json!(["a", 1])).as_deref(), Some("a\n1\n"));
        assert_eq!(stdin_text(&json!("x")).as_deref(), Some("x"));
        assert_eq!(stdin_text(&Value::Null), None);
    }

    #[test]
    fn csv_result_parsing() {
        let v = parse_csv("a,b,c\r\n1,\"x,\"\"y\"\"\",\r\n-2.5,false,0x10\n").unwrap();
        assert_eq!(
            v,
            json!([{"a": 1, "b": "x,\"y\"", "c": null}, {"a": -2.5, "b": false, "c": "0x10"}])
        );
        assert_eq!(
            parse_csv("a\n+3\nNaN\ninf\n").unwrap(),
            json!([{"a": "+3"}, {"a": "NaN"}, {"a": "inf"}])
        );
        assert_eq!(parse_csv("a,b\n").unwrap(), json!([]));
        assert!(parse_csv("").unwrap_err().contains("no header"));
        assert!(parse_csv("a,b\n1\n").unwrap_err().contains("row 2 has 1 cells"));
        assert!(parse_csv("a\n\"open\n").unwrap_err().contains("unterminated"));
    }

    #[test]
    fn result_file_and_file_inline_rules() {
        let cmd = Command::new("t").subcommand(
            Command::new("go")
                .arg(Arg::new("out").long("out"))
                .arg(Arg::new("many").long("many").action(ArgAction::Append))
                .arg(Arg::new("config").long("config")),
        );
        let op = "[[op]]\nname = \"go_op\"\ncommand = \"go\"\naccess = \"read\"\n";
        let err = |extra: &str| Overlay::parse(&format!("{op}{extra}")).unwrap_err();
        assert!(
            err("[op.result_file]\nparam = \"out\"\nname = \"../x\"\nformat = \"csv\"\n").contains("plain file name")
        );
        assert!(err("[op.result_file]\nparam = \"out\"\nformat = \"csv\"\n").contains("needs `read`"));
        let form = "[inline_inputs.config]\n[[inline_inputs.config.form]]\nparam = \"spec\"\nkind = \"json\"\n";
        assert!(err(&format!("{form}via = \"file\"\n")).contains("needs file_name"));
        assert!(err(&format!("{form}file_name = \"a.json\"\n")).contains("needs via"));
        assert!(err(&format!("{form}ops = [\"nope\"]\n")).contains("unknown op nope"));
        let plan_err = |extra: &str| plans(&cmd, &Overlay::parse(&format!("{op}{extra}")).unwrap(), "t").unwrap_err();
        assert!(
            plan_err("[op.result_file]\nparam = \"nope\"\nname = \"r.csv\"\nformat = \"csv\"\n")
                .contains("is not a clap arg")
        );
        assert!(
            plan_err("[op.result_file]\nparam = \"many\"\nname = \"r.csv\"\nformat = \"csv\"\n")
                .contains("single value")
        );
        let o = Overlay::parse(&format!("{op}[op.result_file]\nparam = \"out\"\nname = \"r.csv\"\nformat = \"csv\"\n{form}via = \"file\"\nfile_name = \"spec.json\"\n")).unwrap();
        let p = plans(&cmd, &o, "t").unwrap().remove(0);
        let bridge = Bridge::new(cmd.clone(), &format!("{op}[op.result_file]\nparam = \"out\"\nname = \"r.csv\"\nformat = \"csv\"\n{form}via = \"file\"\nfile_name = \"spec.json\"\n")).unwrap();
        let argv = bridge
            .child_argv(&p, json!({"spec": {"a": 1}}).as_object().unwrap())
            .unwrap();
        assert_eq!(argv, ["go", "--out=<tmp>/r.csv", "--config=<tmp>/spec.json"]);
        assert!(p.schema.params.iter().all(|x| x.name != "out"));
    }

    #[test]
    fn count_and_flags() {
        let cmd = Command::new("t").subcommand(
            Command::new("go")
                .arg(Arg::new("v").short('v').action(ArgAction::Count))
                .arg(Arg::new("off").long("no-color").action(ArgAction::SetFalse)),
        );
        let o = Overlay::parse("[[op]]\nname = \"go_op\"\ncommand = \"go\"\naccess = \"read\"\n").unwrap();
        let p = plans(&cmd, &o, "t").unwrap().remove(0);
        let argv = legacy_argv(&p, json!({"v": 3, "noColor": true}).as_object().unwrap()).unwrap();
        assert_eq!(argv, ["go", "-v", "-v", "-v", "--no-color"]);
        let argv = legacy_argv(&p, json!({"v": 0, "noColor": false}).as_object().unwrap()).unwrap();
        assert_eq!(argv, ["go"]);
    }
}
