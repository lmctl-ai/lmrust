//! The CLI runner (lmts `server/cli.ts`): a [`CliSpec`] (operation schemas plus one
//! handler per operation) becomes a complete lmapi tool.
//!
//! ```text
//! <tool> <op> [--flag value ...]          flags form (globals may come before or after the op)
//! <tool> --schema | --list | --help        discovery (no op)
//! <tool> --explain <op> [--format text]    one op's schema or help
//! <tool> [<op>] --input-json '<json>' | --input-file <path> | --input-graphql '<doc>'
//! global flags: --dryrun/--dry-run --readonly/--read-only --confirm/--i-understand --json
//! ```
//!
//! Every result is one JSON document on stdout; every failure is a JSON error envelope on
//! stdout with the exit code of its error code. A host binary (for example one built
//! with clap) can hand its raw arguments to [`run_cli`] without writing a wrapper.

use super::errors::{CliError, ErrorCode};
use super::output::{print_error, print_result};
use super::policy::Policy;
use super::product::{check_product_version, CheckProductVersionOptions, ProductCheckResult};
use crate::argv_form::parse_argv_form;
use crate::graphql_form::parse_graphql_document;
use crate::help::{render_op_help, render_tool_help};
use crate::inputs::{resolve_input, ResolveArgs};
use crate::json_form::parse_json_document;
use crate::publish::{publish_schema, PublishOptions};
use crate::synopsis::to_kebab;
use crate::types::{
    Access, InputForm, IssueCode, OperationSchema, ParamSpec, ParamType, ProductInfo, PublishedCliSchema, Record,
    ValidationIssue,
};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// An operation implementation: validated params plus the shared context.
pub type Handler<C> = Box<dyn Fn(&Record, &C) -> Result<Value, CliError>>;

/// Builds the context handed to every handler once policy is known.
pub type ContextBuilder<C> = Box<dyn Fn(&Policy) -> C>;

/// Overrides the product check (tests, or hosts with their own detection).
pub type ProductChecker = Box<dyn Fn(&ProductInfo, &HashMap<String, String>) -> ProductCheckResult>;

/// One operation of a tool.
pub struct OpDef<C> {
    /// Programmatic name, e.g. `status`, `list_issues`.
    pub name: String,
    /// One-line description for help.
    pub description: String,
    /// `read` always runs; `write` is governed by policy; `dynamic` decides at run time.
    pub access: Access,
    /// Declared input schema. Without one, the op accepts any flags (`additionalParams`).
    pub input_schema: Option<OperationSchema>,
    pub run: Handler<C>,
}

impl<C> OpDef<C> {
    pub fn new<F>(name: impl Into<String>, access: Access, description: impl Into<String>, run: F) -> Self
    where
        F: Fn(&Record, &C) -> Result<Value, CliError> + 'static,
    {
        OpDef {
            name: name.into(),
            description: description.into(),
            access,
            input_schema: None,
            run: Box::new(run),
        }
    }

    /// Declare the input schema.
    pub fn schema(mut self, schema: OperationSchema) -> Self {
        self.input_schema = Some(schema);
        self
    }
}

/// Everything the runner needs to serve a tool.
pub struct CliSpec<C> {
    /// Binary name, used in help/error text and env prefixes (`<TOOL>_READONLY`).
    pub tool_name: String,
    /// Tool version published by `--schema` (default `"unknown"`).
    pub version: Option<String>,
    /// Summary at the top of `--help`.
    pub description: Option<String>,
    pub ops: Vec<OpDef<C>>,
    pub build_context: ContextBuilder<C>,
    /// Product compatibility: every op invocation is refused with
    /// `UNSUPPORTED_PRODUCT_VERSION` (exit 10) when the product does not match.
    pub product: Option<ProductInfo>,
    /// Replace the product check (default: [`check_product_version`] with the run's env).
    pub product_checker: Option<ProductChecker>,
    /// Runner globals in use (default [`global_flags`]); published as `globals` and parsed.
    pub globals: Vec<GlobalFlag>,
    /// Input forms published as `inputs` (default all three); the others are refused
    /// with `FORM_NOT_ALLOWED`.
    pub inputs: Vec<InputForm>,
}

/// What a runner global does, independent of the flag name a tool publishes for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlobalRole {
    DryRun,
    ReadOnly,
    Confirm,
    Json,
    Help,
    List,
    Schema,
    Explain,
    Format,
    InputJson,
    InputGraphql,
    InputFile,
}

impl GlobalRole {
    /// Every role, in the order of [`global_flags`].
    pub const ALL: [GlobalRole; 12] = [
        GlobalRole::DryRun,
        GlobalRole::ReadOnly,
        GlobalRole::Confirm,
        GlobalRole::Json,
        GlobalRole::Help,
        GlobalRole::List,
        GlobalRole::Schema,
        GlobalRole::Explain,
        GlobalRole::Format,
        GlobalRole::InputJson,
        GlobalRole::InputGraphql,
        GlobalRole::InputFile,
    ];

    /// The lmapi name of the flag (`dryrun`, `input-json`, ...).
    pub fn default_name(self) -> &'static str {
        match self {
            GlobalRole::DryRun => "dryrun",
            GlobalRole::ReadOnly => "readonly",
            GlobalRole::Confirm => "confirm",
            GlobalRole::Json => "json",
            GlobalRole::Help => "help",
            GlobalRole::List => "list",
            GlobalRole::Schema => "schema",
            GlobalRole::Explain => "explain",
            GlobalRole::Format => "format",
            GlobalRole::InputJson => "input-json",
            GlobalRole::InputGraphql => "input-graphql",
            GlobalRole::InputFile => "input-file",
        }
    }
}

/// A runner global: its role and the flag a tool publishes for it.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalFlag {
    pub role: GlobalRole,
    pub spec: ParamSpec,
}

/// The default globals with their roles.
pub fn default_globals() -> Vec<GlobalFlag> {
    GlobalRole::ALL
        .into_iter()
        .zip(global_flags())
        .map(|(role, spec)| GlobalFlag { role, spec })
        .collect()
}

/// Every command-line spelling of a global: `--name`, `--kebab-name`, `--alias`, `-a`.
fn flag_tokens(spec: &ParamSpec) -> Vec<String> {
    let mut out = vec![format!("--{}", spec.name), format!("--{}", to_kebab(&spec.name))];
    for a in spec.alias_list() {
        out.push(if a.chars().count() == 1 {
            format!("-{a}")
        } else {
            format!("--{a}")
        });
    }
    out
}

impl<C> CliSpec<C> {
    pub fn new<F>(tool_name: impl Into<String>, build_context: F) -> Self
    where
        F: Fn(&Policy) -> C + 'static,
    {
        CliSpec {
            tool_name: tool_name.into(),
            version: None,
            description: None,
            ops: Vec::new(),
            build_context: Box::new(build_context),
            product: None,
            product_checker: None,
            globals: default_globals(),
            inputs: vec![InputForm::Flags, InputForm::Json, InputForm::Graphql],
        }
    }

    /// Stop recognising and publishing a global, by its current name or lmapi name (e.g. a
    /// native tool that owns `--format`). Policy env floors still apply when a policy flag is
    /// dropped. Without `format`, `--explain <op>` prints JSON and `<tool> <op> --help` the text.
    pub fn without_global(mut self, name: &str) -> Self {
        self.globals
            .retain(|g| g.spec.name != name && g.role.default_name() != name);
        self
    }

    /// Publish and parse a global under another name (aliases are kept), e.g.
    /// `.rename_global("format", "explain-format")`.
    pub fn rename_global(mut self, name: &str, new_name: impl Into<String>) -> Self {
        let new_name = new_name.into();
        if let Some(g) = self
            .globals
            .iter_mut()
            .find(|g| g.spec.name == name || g.role.default_name() == name)
        {
            g.spec.name = new_name;
        }
        self
    }

    /// Publish (and accept) only these input forms; an unpublished form is refused with
    /// `FORM_NOT_ALLOWED` (exit 2). lmapi protocol section 3.
    pub fn inputs<I: IntoIterator<Item = InputForm>>(mut self, forms: I) -> Self {
        self.inputs = forms.into_iter().collect();
        self
    }

    /// The global flags in use, as published.
    pub fn global_specs(&self) -> Vec<ParamSpec> {
        self.globals.iter().map(|g| g.spec.clone()).collect()
    }

    fn global_spec(&self, role: GlobalRole) -> Option<&ParamSpec> {
        self.globals.iter().find(|g| g.role == role).map(|g| &g.spec)
    }

    /// `argv` contains a spelling of the role's flag (exact token, as lmts `argv.includes`).
    fn has_role(&self, argv: &[String], role: GlobalRole) -> bool {
        self.global_spec(role).is_some_and(|spec| {
            let tokens = flag_tokens(spec);
            argv.iter().any(|a| tokens.contains(a))
        })
    }

    /// Position of the first token that spells the role's flag.
    fn role_position(&self, argv: &[String], role: GlobalRole) -> Option<usize> {
        let tokens = flag_tokens(self.global_spec(role)?);
        argv.iter().position(|a| tokens.contains(a))
    }

    /// The parsed value of a role's global.
    fn role_value<'v>(&self, values: &'v Record, role: GlobalRole) -> Option<&'v Value> {
        self.global_spec(role).and_then(|spec| values.get(&spec.name))
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn product(mut self, product: ProductInfo) -> Self {
        self.product = Some(product);
        self
    }

    pub fn product_checker<F>(mut self, f: F) -> Self
    where
        F: Fn(&ProductInfo, &HashMap<String, String>) -> ProductCheckResult + 'static,
    {
        self.product_checker = Some(Box::new(f));
        self
    }

    /// Add an operation.
    pub fn op(mut self, op: OpDef<C>) -> Self {
        self.ops.push(op);
        self
    }

    /// Attach (or replace) the handler of the operation `name`, creating it from scratch
    /// (no schema, `write` access) when the spec does not declare it.
    pub fn handle<F>(mut self, name: &str, run: F) -> Self
    where
        F: Fn(&Record, &C) -> Result<Value, CliError> + 'static,
    {
        match self.ops.iter_mut().find(|o| o.name == name) {
            Some(op) => op.run = Box::new(run),
            None => self.ops.push(OpDef::new(name, Access::Write, "", run)),
        }
        self
    }

    /// Build a spec from a published document: one op per document operation (schema,
    /// description and access taken from the document; a missing access is treated as
    /// `write`, never read-only). Attach implementations with [`CliSpec::handle`]; an op
    /// without a handler fails with `INTERNAL_ERROR`.
    pub fn from_document<F>(doc: &PublishedCliSchema, build_context: F) -> Self
    where
        F: Fn(&Policy) -> C + 'static,
    {
        let mut spec = CliSpec::new(doc.tool.clone(), build_context);
        spec.version = doc.version.clone();
        spec.product = doc.product.clone();
        if let Some(inputs) = &doc.inputs {
            spec.inputs = inputs.clone();
        }
        for op in &doc.operations {
            let name = op.name.clone();
            spec.ops.push(OpDef {
                name: op.name.clone(),
                description: op.description.clone().unwrap_or_default(),
                access: op.access.unwrap_or(Access::Write),
                input_schema: Some(op.clone()),
                run: Box::new(move |_, _| {
                    Err(CliError::new(
                        ErrorCode::InternalError,
                        format!("operation \"{name}\" has no handler"),
                    ))
                }),
            });
        }
        spec
    }
}

impl CliSpec<Policy> {
    /// A spec whose handler context is the invocation's [`Policy`].
    pub fn with_policy(tool_name: impl Into<String>) -> Self {
        CliSpec::new(tool_name, |p: &Policy| *p)
    }

    /// [`CliSpec::from_document`] with the [`Policy`] as handler context.
    pub fn from_document_with_policy(doc: &PublishedCliSchema) -> Self {
        CliSpec::from_document(doc, |p: &Policy| *p)
    }
}

fn global(name: &str, kind: ParamType, aliases: &[&str], description: &str) -> ParamSpec {
    let mut p = ParamSpec::new(name, kind).optional().describe(description);
    if !aliases.is_empty() {
        p = p.aliases(aliases.iter().copied());
    }
    p
}

/// Global flags understood by every tool (published under `globals`).
pub fn global_flags() -> Vec<ParamSpec> {
    vec![
        global("dryrun", ParamType::Boolean, &["dry-run"], "Simulate write operations"),
        global(
            "readonly",
            ParamType::Boolean,
            &["read-only"],
            "Refuse to run write operations",
        ),
        global(
            "confirm",
            ParamType::Boolean,
            &["i-understand"],
            "Confirm dangerous operations",
        ),
        global("json", ParamType::Boolean, &[], "Output as JSON"),
        global("help", ParamType::Boolean, &["h"], "Show help"),
        global("list", ParamType::Boolean, &[], "List operations as JSON"),
        global("schema", ParamType::Boolean, &[], "Print schema document"),
        global("explain", ParamType::String, &[], "Explain an operation"),
        global("format", ParamType::String, &[], "Format for explain (text or json)"),
        global("input-json", ParamType::String, &["inputJson"], "JSON document input"),
        global(
            "input-graphql",
            ParamType::String,
            &["inputGraphql"],
            "GraphQL document input",
        ),
        global(
            "input-file",
            ParamType::String,
            &["inputFile"],
            "Path to file containing JSON document input",
        ),
    ]
}

fn env_flag(env: &HashMap<String, String>, tool_name: &str, suffix: &str) -> bool {
    let prefix: String = tool_name
        .to_uppercase()
        .chars()
        .map(|c| {
            if c.is_ascii_uppercase() || c.is_ascii_digit() {
                c
            } else {
                '_'
            }
        })
        .collect();
    env.get(&format!("{prefix}_{suffix}"))
        .is_some_and(|v| ["1", "true", "yes", "on"].contains(&v.to_lowercase().as_str()))
}

/// `<TOOL>_READONLY=1|true|yes|on` enables read-only mode (a floor flags cannot loosen).
pub fn read_only_from_env(tool_name: &str, env: &HashMap<String, String>) -> bool {
    env_flag(env, tool_name, "READONLY")
}

/// `<TOOL>_DRYRUN=1|true|yes|on` enables dry-run (a floor flags cannot loosen).
pub fn dry_run_from_env(tool_name: &str, env: &HashMap<String, String>) -> bool {
    env_flag(env, tool_name, "DRYRUN")
}

/// The schema of an op without a declared one: no params, `additionalParams: true`.
pub fn synthesize_schema<C>(op: &OpDef<C>) -> OperationSchema {
    OperationSchema {
        name: op.name.clone(),
        access: Some(op.access),
        returns: None,
        description: Some(op.description.clone()),
        params: Vec::new(),
        additional_params: Some(true),
    }
}

fn schema_of<C>(op: &OpDef<C>) -> OperationSchema {
    op.input_schema.clone().unwrap_or_else(|| synthesize_schema(op))
}

/// The `cli.schema.v1` document `--schema` prints.
pub fn schema_document<C>(spec: &CliSpec<C>) -> PublishedCliSchema {
    publish_schema(
        &spec.tool_name,
        spec.ops.iter().map(schema_of).collect(),
        PublishOptions {
            version: Some(spec.version.clone().unwrap_or_else(|| "unknown".into())),
            globals: Some(spec.global_specs()),
            inputs: Some(spec.inputs.clone()),
            product: spec.product.clone(),
            ..PublishOptions::default()
        },
    )
}

/// Run a tool against the process: arguments after the binary name, stdout, and the
/// process environment. Returns the exit code for `std::process::exit`.
pub fn run_cli<C, I, S>(spec: &CliSpec<C>, args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let argv: Vec<String> = args.into_iter().map(Into::into).collect();
    let env: HashMap<String, String> = std::env::vars().collect();
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let code = run_cli_io(spec, &argv, &mut lock, &env);
    let _ = lock.flush();
    code
}

/// [`run_cli`] with an explicit output stream and environment (embedding and tests).
///
/// ```
/// use lmrust::server::{run_cli_io, CliSpec, OpDef};
/// use lmrust::Access;
/// use serde_json::{json, Value};
/// use std::collections::HashMap;
///
/// let spec = CliSpec::with_policy("echo")
///     .op(OpDef::new("say", Access::Read, "Echo flags back", |params, _| Ok(Value::Object(params.clone()))));
/// let mut out = Vec::new();
/// let argv: Vec<String> = ["say", "--word", "hi"].map(String::from).to_vec();
/// let code = run_cli_io(&spec, &argv, &mut out, &HashMap::new());
/// assert_eq!(code, 0);
/// assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), json!({"word": "hi"}));
///
/// let mut out = Vec::new();
/// let code = run_cli_io(&spec, &["nope".to_string()], &mut out, &HashMap::new());
/// assert_eq!(code, 2); // INVALID_REQUEST envelope on stdout
/// ```
pub fn run_cli_io<C>(spec: &CliSpec<C>, argv: &[String], out: &mut dyn Write, env: &HashMap<String, String>) -> i32 {
    Runner { spec, out, env }.run(argv)
}

/// Root globals that make an invocation lmapi-shaped on their own (no op token needed).
/// `--help`/`-h`, `--json` and `--format` are deliberately absent: a native CLI keeps them.
const ROOT_TRIGGERS: [GlobalRole; 9] = [
    GlobalRole::Schema,
    GlobalRole::List,
    GlobalRole::Explain,
    GlobalRole::InputJson,
    GlobalRole::InputFile,
    GlobalRole::InputGraphql,
    GlobalRole::DryRun,
    GlobalRole::ReadOnly,
    GlobalRole::Confirm,
];

/// Dispatch guard for native CLIs (lmapi protocol section 6.1): whether `argv` (the
/// arguments after the binary name) is lmapi-shaped, i.e. should go to [`run_cli`] while
/// everything else stays with the tool's own parser.
///
/// The rule, using the spec's configured globals:
/// 1. Skip leading runner globals (and their values), as the runner does; a `--` makes the
///    next token the op token.
/// 2. If an op token is found: lmapi-shaped iff it is a published operation name
///    (`tool status --help` is lmapi-shaped and prints the op's help; `tool scan ...` with an
///    unpublished `scan` is native).
/// 3. Otherwise: lmapi-shaped iff a root trigger global is present (`--schema`, `--list`,
///    `--explain <op>`, `--input-json`, `--input-file`, `--input-graphql`, `--dryrun`,
///    `--readonly`, `--confirm`, in any of their spellings, also as `--flag=value`).
///    `--help`/`-h` alone, `--json`, `--format` and empty argv are NOT lmapi-shaped, so a
///    native CLI keeps its own help and no-argument behaviour.
///
/// ```
/// use lmrust::server::{is_lmapi_invocation, CliSpec, OpDef};
/// use lmrust::Access;
/// use serde_json::json;
///
/// let spec = CliSpec::with_policy("probe").op(OpDef::new("status", Access::Read, "", |_, _| Ok(json!({}))));
/// let shaped = |args: &[&str]| is_lmapi_invocation(&spec, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
/// assert!(shaped(&["--schema"]));
/// assert!(shaped(&["status", "--help"]));
/// assert!(shaped(&["--explain", "status"]));
/// assert!(!shaped(&["--help"]));
/// assert!(!shaped(&[]));
/// assert!(!shaped(&["scan", "example.com"]));
/// ```
pub fn is_lmapi_invocation<C>(spec: &CliSpec<C>, argv: &[String]) -> bool {
    if argv.is_empty() {
        return false;
    }
    match find_op_token_index(argv, &spec.global_specs()) {
        Some(i) => spec.ops.iter().any(|o| o.name == argv[i]),
        None => ROOT_TRIGGERS.iter().any(|role| {
            spec.global_spec(*role).is_some_and(|g| {
                let tokens = flag_tokens(g);
                argv.iter().any(|a| {
                    tokens
                        .iter()
                        .any(|t| a == t || (a.starts_with(t.as_str()) && a[t.len()..].starts_with('=')))
                })
            })
        }),
    }
}

/// [`run_cli`] only for lmapi-shaped argv ([`is_lmapi_invocation`]); `None` leaves the
/// invocation to the native CLI's own parser.
///
/// ```no_run
/// # use lmrust::server::{maybe_run_cli, CliSpec};
/// # let spec = CliSpec::with_policy("probe");
/// let args: Vec<String> = std::env::args().skip(1).collect();
/// if let Some(code) = maybe_run_cli(&spec, args.clone()) {
///     std::process::exit(code);
/// }
/// // ... the tool's own (clap) parsing of `args` continues here.
/// ```
pub fn maybe_run_cli<C, I, S>(spec: &CliSpec<C>, args: I) -> Option<i32>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let argv: Vec<String> = args.into_iter().map(Into::into).collect();
    is_lmapi_invocation(spec, &argv).then(|| run_cli(spec, argv))
}

/// [`maybe_run_cli`] with an explicit output stream and environment.
pub fn maybe_run_cli_io<C>(
    spec: &CliSpec<C>,
    argv: &[String],
    out: &mut dyn Write,
    env: &HashMap<String, String>,
) -> Option<i32> {
    is_lmapi_invocation(spec, argv).then(|| run_cli_io(spec, argv, out, env))
}

struct Runner<'a, C> {
    spec: &'a CliSpec<C>,
    out: &'a mut dyn Write,
    env: &'a HashMap<String, String>,
}

struct Docs {
    json: Option<String>,
    graphql: Option<String>,
}

fn op_synopsis(schema: &OperationSchema) -> String {
    let mut parts = vec![schema.name.clone()];
    for p in &schema.params {
        let hint = if p.kind == ParamType::Boolean {
            String::new()
        } else {
            format!(" <{}>", p.kind)
        };
        parts.push(format!("[--{}{}]", to_kebab(&p.name), hint));
    }
    parts.join(" ")
}

/// Index of the first token that is not a global flag or a global flag's value; `None`
/// when the invocation names no op. A `--` makes the next token the op.
fn find_op_token_index(argv: &[String], globals: &[ParamSpec]) -> Option<usize> {
    let mut booleans: Vec<String> = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    for g in globals {
        let bag = if g.kind == ParamType::Boolean {
            &mut booleans
        } else {
            &mut strings
        };
        for n in std::iter::once(g.name.clone()).chain(g.alias_list().iter().cloned()) {
            bag.push(to_kebab(&n));
            bag.push(n);
        }
    }
    let mut i = 0;
    while i < argv.len() {
        let tok = &argv[i];
        if tok == "--" {
            return if i + 1 < argv.len() { Some(i + 1) } else { None };
        }
        if let Some(body) = tok.strip_prefix("--") {
            let (name, has_eq) = match body.find('=') {
                Some(eq) => (&body[..eq], true),
                None => (body, false),
            };
            let is_bool = |n: &str| booleans.iter().any(|b| b == n);
            if is_bool(name) || name.strip_prefix("no-").is_some_and(is_bool) {
                i += 1;
                continue;
            }
            if strings.iter().any(|s| s == name) && !has_eq && argv.get(i + 1).is_some_and(|n| !n.starts_with("--")) {
                i += 1;
            }
            i += 1;
            continue;
        }
        if tok.starts_with('-') {
            i += 1;
            continue;
        }
        return Some(i);
    }
    None
}

impl<C> Runner<'_, C> {
    fn fail(&mut self, err: CliError) -> i32 {
        print_error(self.out, &err);
        err.exit_code()
    }

    fn write_text(&mut self, text: &str) -> i32 {
        let _ = self.out.write_all(text.as_bytes());
        0
    }

    fn run(&mut self, argv: &[String]) -> i32 {
        let spec = self.spec;
        if argv.is_empty() {
            return self.print_help();
        }
        let Some(op_idx) = find_op_token_index(argv, &spec.global_specs()) else {
            if spec.has_role(argv, GlobalRole::Help) {
                return self.print_help();
            }
            if spec.has_role(argv, GlobalRole::List) {
                return self.print_op_list();
            }
            if spec.has_role(argv, GlobalRole::Schema) {
                let doc = schema_document(self.spec);
                print_result(self.out, &doc);
                return 0;
            }
            if spec.has_role(argv, GlobalRole::Explain) {
                return self.explain(argv);
            }
            // A document passed without an op names the op itself (`{op, params}` / the GraphQL field).
            let dummy = OperationSchema::new("<unknown>").additional_params(true);
            let globals = parse_argv_form(argv, &dummy, &spec.global_specs())
                .map(|v| v.globals)
                .unwrap_or_default();
            let docs = match self.extract_docs(&globals) {
                Ok(d) => d,
                Err(e) => return self.fail(e),
            };
            let inferred = if let Some(json) = &docs.json {
                parse_json_document(json).ok().and_then(|d| d.op)
            } else if let Some(gql) = &docs.graphql {
                parse_graphql_document(gql).ok().and_then(|d| d.op)
            } else {
                None
            };
            let Some(op) = inferred.filter(|o| !o.is_empty()) else {
                let tool = self.spec.tool_name.clone();
                return self.fail(CliError::new(ErrorCode::InvalidRequest, "No operation specified").with(
                    "hint",
                    Value::String(format!("Run \"{tool} --help\" to see available operations.")),
                ));
            };
            return self.run_op(&op, argv);
        };

        let op_name = argv[op_idx].clone();
        let rest: Vec<String> = argv
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != op_idx)
            .map(|(_, a)| a.clone())
            .collect();
        if spec.has_role(&rest, GlobalRole::Help) {
            if let Some(op) = self.spec.ops.iter().find(|o| o.name == op_name) {
                let text = render_op_help(&self.spec.tool_name, &schema_of(op));
                return self.write_text(&text);
            }
        }
        self.run_op(&op_name, &rest)
    }

    fn print_help(&mut self) -> i32 {
        let schemas: Vec<OperationSchema> = self.spec.ops.iter().map(schema_of).collect();
        let text = render_tool_help(
            &self.spec.tool_name,
            self.spec.description.as_deref(),
            &schemas,
            &self.spec.global_specs(),
        );
        self.write_text(&text)
    }

    fn print_op_list(&mut self) -> i32 {
        let list: Vec<Value> = self
            .spec
            .ops
            .iter()
            .map(|op| {
                let mut entry = Record::new();
                entry.insert("name".into(), Value::String(op.name.clone()));
                entry.insert("access".into(), Value::String(op.access.as_str().into()));
                entry.insert("description".into(), Value::String(op.description.clone()));
                if let Some(s) = &op.input_schema {
                    entry.insert("schema".into(), serde_json::to_value(s).unwrap_or(Value::Null));
                }
                Value::Object(entry)
            })
            .collect();
        print_result(self.out, &list);
        0
    }

    fn explain(&mut self, argv: &[String]) -> i32 {
        let idx = self.spec.role_position(argv, GlobalRole::Explain).unwrap_or(0);
        let op_name = argv.get(idx + 1);
        let wants_text = self
            .spec
            .role_position(argv, GlobalRole::Format)
            .and_then(|f| argv.get(f + 1))
            .is_some_and(|v| v == "text");
        let Some(op_name) = op_name.filter(|n| !n.starts_with("--")) else {
            self.fail(CliError::new(
                ErrorCode::InvalidRequest,
                "Missing operation name for --explain",
            ));
            return 2;
        };
        let Some(op) = self.spec.ops.iter().find(|o| &o.name == op_name) else {
            self.fail(CliError::new(
                ErrorCode::InvalidRequest,
                format!("Unknown operation for --explain: {op_name}"),
            ));
            return 2;
        };
        let schema = schema_of(op);
        if wants_text {
            let text = render_op_help(&self.spec.tool_name, &schema);
            return self.write_text(&text);
        }
        print_result(self.out, &schema);
        0
    }

    fn extract_docs(&self, globals: &Record) -> Result<Docs, CliError> {
        let text = |role| {
            self.spec
                .role_value(globals, role)
                .and_then(Value::as_str)
                .map(String::from)
        };
        let json_raw = text(GlobalRole::InputJson);
        let graphql = text(GlobalRole::InputGraphql);
        let mut from_file = None;
        if let Some(path) = text(GlobalRole::InputFile) {
            match std::fs::read_to_string(&path) {
                Ok(text) => from_file = Some(text),
                Err(e) => {
                    return Err(
                        CliError::new(ErrorCode::InvalidRequest, format!("cannot read input file: {path}"))
                            .with("detail", Value::String(e.to_string())),
                    )
                }
            }
        }
        Ok(Docs {
            json: from_file.or(json_raw),
            graphql,
        })
    }

    fn check_product(&self) -> Option<CliError> {
        let product = self.spec.product.as_ref()?;
        let result = match &self.spec.product_checker {
            Some(check) => check(product, self.env),
            None => check_product_version(
                product,
                &CheckProductVersionOptions {
                    env: Some(self.env.clone()),
                    ..CheckProductVersionOptions::default()
                },
            ),
        };
        result
            .err()
            .map(|f| CliError::new(ErrorCode::UnsupportedProductVersion, f.message.clone()).with_data(f.data()))
    }

    fn run_op(&mut self, op_name: &str, argv: &[String]) -> i32 {
        if let Some(err) = self.check_product() {
            return self.fail(err);
        }
        let spec = self.spec;
        let Some(op) = spec.ops.iter().find(|o| o.name == op_name) else {
            let tool = spec.tool_name.clone();
            return self.fail(
                CliError::new(ErrorCode::InvalidRequest, format!("Unknown operation: {op_name}")).with(
                    "hint",
                    Value::String(format!("Run \"{tool} --list\" to see available operations.")),
                ),
            );
        };
        let schema = schema_of(op);
        let globals = spec.global_specs();
        let parsed = parse_argv_form(argv, &schema, &globals);
        let (global_values, op_params, positionals) = match &parsed {
            Ok(v) => (v.globals.clone(), v.params.clone(), v.positionals.clone()),
            Err(_) => (Record::new(), Record::new(), Vec::new()),
        };
        let docs = match self.extract_docs(&global_values) {
            Ok(d) => d,
            Err(e) => return self.fail(e),
        };

        // Only published input forms are accepted (lmapi protocol section 3).
        let form = if docs.json.is_some() {
            Some(InputForm::Json)
        } else if docs.graphql.is_some() {
            Some(InputForm::Graphql)
        } else if !op_params.is_empty() || !positionals.is_empty() || parsed.is_err() {
            Some(InputForm::Flags)
        } else {
            None // only globals: no input form is used
        };
        if let Some(form) = form.filter(|f| !spec.inputs.contains(f)) {
            let published: Vec<&str> = spec.inputs.iter().map(|f| f.as_str()).collect();
            let message = format!(
                "input form \"{}\" is not published by {} (inputs: {})",
                form.as_str(),
                spec.tool_name,
                if published.is_empty() {
                    "none".to_string()
                } else {
                    published.join(", ")
                }
            );
            let usage = format!("{} {} [flags]", spec.tool_name, op.name);
            let mut issue = ValidationIssue::new(
                IssueCode::FormNotAllowed,
                format!("{message}. Expected: {usage}"),
                usage.clone(),
                usage,
            );
            issue.op = Some(op.name.clone());
            issue.received = Some(Value::String(form.as_str().into()));
            issue.allowed = Some(published.iter().map(|f| Value::String((*f).into())).collect());
            return self.fail(validation_error(message, &op.name, &[issue]));
        }

        if docs.json.is_some() || docs.graphql.is_some() {
            // Documents may accompany global flags, but not op flags or positionals.
            if !op_params.is_empty() || !positionals.is_empty() {
                let which = if docs.json.is_some() {
                    "--input-json"
                } else {
                    "--input-graphql"
                };
                let synopsis = op_synopsis(&schema);
                let mut issue = ValidationIssue::new(
                    IssueCode::MixedInput,
                    format!(
                        "cannot mix {which} with other flags for op \"{}\". Expected: {synopsis}",
                        op.name
                    ),
                    synopsis,
                    format!("{} {} [flags]", spec.tool_name, op.name),
                );
                issue.op = Some(op.name.clone());
                return self.fail(validation_error(
                    format!("cannot mix {which} with other flags for op \"{}\"", op.name),
                    &op.name,
                    &[issue],
                ));
            }
            if let Err(issues) = &parsed {
                return self.fail(validation_error(
                    format!("invalid arguments for \"{}\"", op.name),
                    &op.name,
                    issues,
                ));
            }
        }

        let on = |role| spec.role_value(&global_values, role) == Some(&Value::Bool(true));
        let policy = Policy {
            dry_run: on(GlobalRole::DryRun) || dry_run_from_env(&spec.tool_name, self.env),
            read_only: on(GlobalRole::ReadOnly) || read_only_from_env(&spec.tool_name, self.env),
            confirmed: on(GlobalRole::Confirm),
        };

        let resolved = resolve_input(ResolveArgs {
            schema: &schema,
            argv,
            globals: &globals,
            json_document: docs.json.as_deref(),
            graphql_document: docs.graphql.as_deref(),
        });
        let params = match resolved {
            Ok(r) => r.params,
            Err(err) => {
                let raw = err.issues();
                let issues: Vec<ValidationIssue> = if raw.is_empty() {
                    let code = err
                        .details
                        .get("code")
                        .and_then(|c| serde_json::from_value::<IssueCode>(c.clone()).ok())
                        .unwrap_or(IssueCode::DocumentParse);
                    let synopsis = err
                        .details
                        .get("synopsis")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .unwrap_or_else(|| format!("{} [flags]", op.name));
                    let mut i = ValidationIssue::new(
                        code,
                        err.message.clone(),
                        synopsis,
                        format!("{} {} [flags]", spec.tool_name, op.name),
                    );
                    i.op = Some(op.name.clone());
                    vec![i]
                } else {
                    raw
                };
                let issues: Vec<ValidationIssue> = issues
                    .into_iter()
                    .map(|mut i| {
                        if i.code == IssueCode::UnknownParam {
                            i.synopsis = op_synopsis(&schema);
                        }
                        i
                    })
                    .collect();
                let message = issues[0].message.clone();
                return self.fail(validation_error(message, &op.name, &issues));
            }
        };

        let ctx = (spec.build_context)(&policy);
        let outcome = catch_unwind(AssertUnwindSafe(|| (op.run)(&params, &ctx)));
        match outcome {
            Ok(Ok(result)) => {
                print_result(self.out, &result);
                0
            }
            Ok(Err(err)) => self.fail(err),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "Unknown error".into());
                self.fail(CliError::new(ErrorCode::UnknownError, message))
            }
        }
    }
}

fn validation_error(message: String, op: &str, issues: &[ValidationIssue]) -> CliError {
    CliError::new(ErrorCode::ValidationError, message)
        .with("op", Value::String(op.to_string()))
        .with("issues", serde_json::to_value(issues).unwrap_or(Value::Null))
}
