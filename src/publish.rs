//! Publish and check `cli.schema.v1` documents (lmts `publish.ts`): "Swagger for CLI".
//! A consumer fetches the document, validates input against it and generates docs from it.

use crate::js;
use crate::types::Record;
use crate::types::{
    is_reserved_name, InputForm, IssueCode, OperationSchema, Outcome, ParamSpec, PolicyFlags, ProductInfo,
    PublishedCliSchema, ValidationIssue,
};
use crate::validate::SchemaValidationError;
use crate::version::{count_regexp_groups, is_valid_product_argv0, is_valid_product_name, is_valid_supported_range};
use serde_json::Value;

/// Document-level options of [`publish_schema`].
#[derive(Debug, Clone, Default)]
pub struct PublishOptions {
    /// Tool version (default `"unknown"`).
    pub version: Option<String>,
    pub globals: Option<Vec<ParamSpec>>,
    /// Input forms (default flags, json, graphql).
    pub inputs: Option<Vec<InputForm>>,
    /// Policy flags (default `{dryrun: true, readonly: true}`).
    pub policy: Option<PolicyFlags>,
    /// Accepted for parity with lmts; not part of the document.
    pub tool_description: Option<String>,
    pub product: Option<ProductInfo>,
}

/// Build the publishable document for a tool from every op that declares a schema.
///
/// ```
/// use lmrust::{parse_published_schema, publish_schema, OperationSchema, PublishOptions};
/// let doc = publish_schema("git2", vec![OperationSchema::new("status")], PublishOptions::default());
/// assert_eq!(doc.version.as_deref(), Some("unknown"));
/// let json = serde_json::to_value(&doc).unwrap();
/// assert!(parse_published_schema(&json).is_ok());
/// ```
pub fn publish_schema(tool: &str, operations: Vec<OperationSchema>, opts: PublishOptions) -> PublishedCliSchema {
    PublishedCliSchema {
        schema: "cli.schema.v1".into(),
        tool: tool.into(),
        version: Some(opts.version.unwrap_or_else(|| "unknown".into())),
        policy: Some(opts.policy.unwrap_or(PolicyFlags {
            dryrun: true,
            readonly: true,
        })),
        inputs: Some(
            opts.inputs
                .unwrap_or_else(|| vec![InputForm::Flags, InputForm::Json, InputForm::Graphql]),
        ),
        globals: opts.globals,
        operations,
        warnings: None,
        product: opts.product,
    }
}

/// Missing descriptions on operations, params and globals; also stored in `doc.warnings`.
pub fn schema_warnings(doc: &mut PublishedCliSchema) -> Vec<String> {
    let blank = |d: &Option<String>| d.as_deref().map_or(true, |s| js::trim(s).is_empty());
    let mut warnings = Vec::new();
    for op in &doc.operations {
        if blank(&op.description) {
            warnings.push(format!("operation \"{}\" is missing a description", op.name));
        }
        for p in &op.params {
            if blank(&p.description) {
                warnings.push(format!(
                    "param \"{}\" of operation \"{}\" is missing a description",
                    p.name, op.name
                ));
            }
        }
    }
    for g in doc.globals.iter().flatten() {
        if blank(&g.description) {
            warnings.push(format!("global param \"{}\" is missing a description", g.name));
        }
    }
    doc.warnings = Some(warnings.clone());
    warnings
}

const VALID_PARAM_TYPES: [&str; 6] = ["string", "number", "boolean", "object", "array", "json"];
const VALID_PARAM_OCCURS: [&str; 4] = ["1", "?", "+", "*"];
const VALID_PARAM_ITEMS: [&str; 4] = ["string", "number", "boolean", "object"];
const VALID_ACCESS: [&str; 3] = ["read", "write", "dynamic"];
const VALID_RETURNS: [&str; 6] = ["object", "array", "string", "number", "boolean", "null"];
const VALID_INPUTS: [&str; 3] = ["flags", "json", "graphql"];
const ALLOWED_PARAM_KEYS: [&str; 12] = [
    "name",
    "type",
    "occurs",
    "default",
    "enum",
    "positional",
    "items",
    "description",
    "aliases",
    "cliOnly",
    "docOnly",
    "derived",
];

/// `^[a-z][a-z0-9_]*$`
pub(crate) fn is_tool_or_op_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// `^[A-Za-z_]\w*$`
fn is_param_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// `^[A-Za-z_][A-Za-z0-9_-]*$`
fn is_global_param_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

fn str_list(items: &[&str]) -> Vec<Value> {
    items.iter().map(|s| Value::String((*s).into())).collect()
}

fn scalar_matches(val: &Value, kind: &str) -> bool {
    match kind {
        "string" => val.is_string(),
        "number" => val.is_number(),
        "boolean" => val.is_boolean(),
        "object" => val.is_object(),
        "array" => val.is_array(),
        "json" => true,
        _ => false,
    }
}

fn default_matches(
    val: &Value,
    kind: &str,
    items: Option<&str>,
    occurs: Option<&str>,
    allowed: Option<&[Value]>,
) -> bool {
    // An enum constrains each value: every item of an array default, or the scalar default.
    let in_enum = |v: &Value| match allowed.filter(|a| !a.is_empty()) {
        Some(allowed) => allowed.iter().any(|a| js::strict_eq(a, v)),
        None => true,
    };
    if occurs == Some("+") || occurs == Some("*") || kind == "array" {
        let Value::Array(list) = val else { return false };
        let elem = items.unwrap_or(if kind == "array" { "string" } else { kind });
        return list.iter().all(|i| in_enum(i) && scalar_matches(i, elem));
    }
    in_enum(val) && scalar_matches(val, kind)
}

struct Checker {
    issues: Vec<ValidationIssue>,
}

#[derive(Default)]
struct At<'a> {
    op: Option<&'a str>,
    param: Option<&'a str>,
    received: Option<Value>,
    allowed: Option<Vec<Value>>,
}

impl Checker {
    fn add(&mut self, code: IssueCode, message: String, at: At<'_>) {
        let usage = match at.op {
            Some(op) if !op.is_empty() => format!("{op} [flags]"),
            _ => "<unknown> [flags]".to_string(),
        };
        let mut i = ValidationIssue::new(code, format!("{message}. Expected: {usage}"), usage.clone(), usage);
        i.op = at.op.filter(|o| !o.is_empty()).map(String::from);
        i.param = at.param.filter(|p| !p.is_empty()).map(String::from);
        i.received = at.received;
        i.allowed = at.allowed;
        self.issues.push(i);
    }

    fn param_spec(&mut self, p: &Value, op_name: Option<&str>, is_global: bool) {
        let in_op = op_name.map(|o| format!(" in \"{o}\"")).unwrap_or_default();
        let Value::Object(po) = p else {
            self.add(
                IssueCode::TypeMismatch,
                format!("cli.schema.v1: param{in_op} must be an object"),
                At {
                    op: op_name,
                    received: Some(p.clone()),
                    ..At::default()
                },
            );
            return;
        };
        let Some(Value::String(p_name)) = po.get("name") else {
            self.add(
                IssueCode::TypeMismatch,
                format!("cli.schema.v1: param{in_op} needs a string name"),
                At {
                    op: op_name,
                    received: po.get("name").cloned(),
                    ..At::default()
                },
            );
            return;
        };
        let p_name = p_name.as_str();
        let at = |received: Option<Value>, allowed: Option<Vec<Value>>| At {
            op: op_name,
            param: Some(p_name),
            received,
            allowed,
        };

        for key in po.keys() {
            if !ALLOWED_PARAM_KEYS.contains(&key.as_str()) {
                self.add(
                    IssueCode::UnknownParam,
                    format!("cli.schema.v1: param \"{p_name}\"{in_op} has unknown property \"{key}\""),
                    at(Some(Value::String(key.clone())), Some(str_list(&ALLOWED_PARAM_KEYS))),
                );
            }
        }

        let name_ok = if is_global {
            is_global_param_name(p_name)
        } else {
            is_param_name(p_name)
        };
        if !name_ok {
            let pattern = if is_global {
                "^[A-Za-z_][A-Za-z0-9_-]*$"
            } else {
                "^[A-Za-z_]\\w*$"
            };
            self.add(
                IssueCode::DocumentParse,
                format!("cli.schema.v1: param \"{p_name}\"{in_op} name must match {pattern}"),
                at(Some(Value::String(p_name.into())), None),
            );
        } else if is_reserved_name(p_name) {
            let g = if is_global { "global " } else { "" };
            self.add(
                IssueCode::ReservedName,
                format!("cli.schema.v1: {g}param \"{p_name}\"{in_op} is a reserved name"),
                at(Some(Value::String(p_name.into())), None),
            );
        }

        if !is_global && p_name == "dryrun" {
            self.add(
                IssueCode::DocumentParse,
                "cli.schema.v1: param cannot be named \"dryrun\" (reserved global policy flag)".into(),
                at(Some(Value::String(p_name.into())), None),
            );
        }

        if let Some(Value::Array(aliases)) = po.get("aliases") {
            for alias in aliases {
                if let Value::String(a) = alias {
                    if is_reserved_name(a) {
                        self.add(
                            IssueCode::ReservedName,
                            format!("cli.schema.v1: param \"{p_name}\"{in_op} alias \"{a}\" is a reserved name"),
                            at(Some(alias.clone()), None),
                        );
                    }
                }
            }
        }

        let p_type = po
            .get("type")
            .and_then(Value::as_str)
            .filter(|t| VALID_PARAM_TYPES.contains(t));
        if p_type.is_none() {
            let shown = po.get("type").map(js::stringify).unwrap_or_else(|| "undefined".into());
            self.add(
                IssueCode::EnumViolation,
                format!("cli.schema.v1: param \"{p_name}\"{in_op} has invalid type: {shown}"),
                at(po.get("type").cloned(), Some(str_list(&VALID_PARAM_TYPES))),
            );
        }

        let occurs = po.get("occurs");
        let occurs_str = occurs.and_then(Value::as_str);
        if let Some(o) = occurs {
            if !occurs_str.is_some_and(|s| VALID_PARAM_OCCURS.contains(&s)) {
                self.add(
                    IssueCode::EnumViolation,
                    format!(
                        "cli.schema.v1: param \"{p_name}\"{in_op} has invalid occurs: {}",
                        js::stringify(o)
                    ),
                    at(Some(o.clone()), Some(str_list(&VALID_PARAM_OCCURS))),
                );
            }
        }

        let p_enum = po.get("enum");
        if let Some(e) = p_enum {
            let ok = matches!(e, Value::Array(list) if list.iter().all(|v| v.is_string() || v.is_number()));
            if !ok {
                self.add(
                    IssueCode::TypeMismatch,
                    format!("cli.schema.v1: param \"{p_name}\"{in_op} enum must be an array of strings or numbers"),
                    at(Some(e.clone()), None),
                );
            }
        }

        let items = po.get("items");
        let items_str = items.and_then(Value::as_str);
        if let Some(i) = items {
            if !items_str.is_some_and(|s| VALID_PARAM_ITEMS.contains(&s)) {
                self.add(
                    IssueCode::EnumViolation,
                    format!(
                        "cli.schema.v1: param \"{p_name}\"{in_op} has invalid items type: {}",
                        js::stringify(i)
                    ),
                    at(Some(i.clone()), Some(str_list(&VALID_PARAM_ITEMS))),
                );
            }
        }

        if let (Some(default), Some(kind)) = (po.get("default"), p_type) {
            let allowed = match p_enum {
                Some(Value::Array(list)) => Some(list.as_slice()),
                _ => None,
            };
            if !default_matches(default, kind, items_str, occurs_str, allowed) {
                self.add(
                    IssueCode::TypeMismatch,
                    format!("cli.schema.v1: param \"{p_name}\"{in_op} default value does not match declared type \"{kind}\""),
                    at(Some(default.clone()), None),
                );
            }
        }
    }

    fn product(&mut self, raw: &Value) {
        let Value::Object(prod) = raw else {
            self.add(
                IssueCode::TypeMismatch,
                "cli.schema.v1: product must be an object".into(),
                At {
                    received: Some(raw.clone()),
                    ..At::default()
                },
            );
            return;
        };
        let allowed_keys = ["name", "supported", "detect"];
        for k in prod.keys() {
            if !allowed_keys.contains(&k.as_str()) {
                self.add(
                    IssueCode::UnknownParam,
                    format!("cli.schema.v1: unknown product property \"{k}\""),
                    At {
                        received: Some(Value::String(k.clone())),
                        allowed: Some(str_list(&allowed_keys)),
                        ..At::default()
                    },
                );
            }
        }
        match prod.get("name") {
            Some(Value::String(n)) if !is_valid_product_name(n) => self.add(
                IssueCode::DocumentParse,
                format!(
                    "cli.schema.v1: product.name must match ^[a-z][a-z0-9_.-]{{0,63}}$, received {}",
                    js::stringify(n)
                ),
                At {
                    received: Some(Value::String(n.clone())),
                    ..At::default()
                },
            ),
            Some(Value::String(_)) => {}
            other => self.add(
                IssueCode::TypeMismatch,
                "cli.schema.v1: product.name must be a string".into(),
                At {
                    received: other.cloned(),
                    ..At::default()
                },
            ),
        }
        match prod.get("supported") {
            Some(Value::String(s)) if !is_valid_supported_range(s) => self.add(
                IssueCode::DocumentParse,
                format!(
                    "cli.schema.v1: product.supported must be a conjunction of 1 to 4 comparators (>=, >, <=, <, = followed by numeric x.y.z), received {}",
                    js::stringify(s)
                ),
                At { received: Some(Value::String(s.clone())), ..At::default() },
            ),
            Some(Value::String(_)) => {}
            other => self.add(
                IssueCode::TypeMismatch,
                "cli.schema.v1: product.supported must be a string".into(),
                At { received: other.cloned(), ..At::default() },
            ),
        }
        let Some(raw_detect) = prod.get("detect") else { return };
        let Value::Object(detect) = raw_detect else {
            self.add(
                IssueCode::TypeMismatch,
                "cli.schema.v1: product.detect must be an object".into(),
                At {
                    received: Some(raw_detect.clone()),
                    ..At::default()
                },
            );
            return;
        };
        let allowed_detect = ["argv", "pattern"];
        for k in detect.keys() {
            if !allowed_detect.contains(&k.as_str()) {
                self.add(
                    IssueCode::UnknownParam,
                    format!("cli.schema.v1: unknown product.detect property \"{k}\""),
                    At {
                        received: Some(Value::String(k.clone())),
                        allowed: Some(str_list(&allowed_detect)),
                        ..At::default()
                    },
                );
            }
        }
        let argv = detect.get("argv");
        let argv_list = match argv {
            Some(Value::Array(list))
                if (1..=8).contains(&list.len()) && list.iter().all(|a| a.as_str().is_some_and(|s| !s.is_empty())) =>
            {
                Some(list)
            }
            _ => None,
        };
        match argv_list {
            None => self.add(
                IssueCode::DocumentParse,
                "cli.schema.v1: product.detect.argv must be an array of 1 to 8 non-empty strings".into(),
                At {
                    received: argv.cloned(),
                    ..At::default()
                },
            ),
            Some(list) => {
                let argv0 = list[0].as_str().unwrap_or_default();
                if !is_valid_product_argv0(argv0) {
                    self.add(
                        IssueCode::DocumentParse,
                        format!(
                            "cli.schema.v1: product.detect.argv[0] must match ^[a-z][a-z0-9_.-]*$ (no slashes), received {}",
                            js::stringify(argv0)
                        ),
                        At { received: Some(Value::String(argv0.into())), ..At::default() },
                    );
                }
            }
        }
        match detect.get("pattern") {
            Some(Value::String(pattern)) => {
                let len = pattern.chars().count();
                if len > 200 {
                    self.add(
                        IssueCode::DocumentParse,
                        format!(
                            "cli.schema.v1: product.detect.pattern exceeds maximum length of 200 characters ({len})"
                        ),
                        At {
                            received: Some(Value::String(pattern.clone())),
                            ..At::default()
                        },
                    );
                } else {
                    let groups = count_regexp_groups(pattern);
                    if groups != Some(1) {
                        let found = groups.map(|g| g.to_string()).unwrap_or_else(|| "-1".into());
                        self.add(
                            IssueCode::DocumentParse,
                            format!("cli.schema.v1: product.detect.pattern must be a valid RegExp with exactly one capture group, found {found}"),
                            At { received: Some(Value::String(pattern.clone())), ..At::default() },
                        );
                    }
                }
            }
            other => self.add(
                IssueCode::TypeMismatch,
                "cli.schema.v1: product.detect.pattern must be a string".into(),
                At {
                    received: other.cloned(),
                    ..At::default()
                },
            ),
        }
    }
}

fn shown_type(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(v) => js::type_of(v),
    }
}

/// Check that an arbitrary JSON value is a well-formed `cli.schema.v1` document,
/// reporting every issue found.
pub fn parse_published_schema(value: &Value) -> Outcome<PublishedCliSchema> {
    let mut c = Checker { issues: Vec::new() };
    let Value::Object(doc) = value else {
        c.add(
            IssueCode::DocumentParse,
            "cli.schema.v1: document must be an object".into(),
            At {
                received: Some(value.clone()),
                ..At::default()
            },
        );
        return Err(c.issues);
    };

    if doc.get("schema").and_then(Value::as_str) != Some("cli.schema.v1") {
        let shown = doc
            .get("schema")
            .map(js::stringify)
            .unwrap_or_else(|| "undefined".into());
        c.add(
            IssueCode::DocumentParse,
            format!("cli.schema.v1: unsupported schema tag: {shown}"),
            At {
                received: doc.get("schema").cloned(),
                allowed: Some(str_list(&["cli.schema.v1"])),
                ..At::default()
            },
        );
    }

    match doc.get("tool") {
        Some(Value::String(tool)) if is_tool_or_op_name(tool) => {
            if is_reserved_name(tool) {
                c.add(
                    IssueCode::ReservedName,
                    format!("cli.schema.v1: tool cannot be named \"{tool}\" (reserved name)"),
                    At {
                        received: Some(Value::String(tool.clone())),
                        ..At::default()
                    },
                );
            }
        }
        other => {
            let shown = other.map(js::stringify).unwrap_or_else(|| "undefined".into());
            c.add(
                IssueCode::DocumentParse,
                format!("cli.schema.v1: tool must be a string matching ^[a-z][a-z0-9_]*$, received {shown}"),
                At {
                    received: other.cloned(),
                    ..At::default()
                },
            );
        }
    }

    if let Some(v) = doc.get("version") {
        if !v.is_string() {
            c.add(
                IssueCode::TypeMismatch,
                format!(
                    "cli.schema.v1: version must be a string, received {}",
                    shown_type(Some(v))
                ),
                At {
                    received: Some(v.clone()),
                    ..At::default()
                },
            );
        }
    }

    if let Some(policy) = doc.get("policy") {
        match policy {
            Value::Object(pol) => {
                if !pol.get("dryrun").is_some_and(Value::is_boolean)
                    || !pol.get("readonly").is_some_and(Value::is_boolean)
                {
                    c.add(
                        IssueCode::TypeMismatch,
                        "cli.schema.v1: policy must contain boolean dryrun and readonly properties".into(),
                        At {
                            received: Some(policy.clone()),
                            ..At::default()
                        },
                    );
                }
            }
            _ => c.add(
                IssueCode::TypeMismatch,
                "cli.schema.v1: policy must be an object".into(),
                At {
                    received: Some(policy.clone()),
                    ..At::default()
                },
            ),
        }
    }

    if let Some(inputs) = doc.get("inputs") {
        match inputs {
            Value::Array(list) => {
                for input in list {
                    if !input.as_str().is_some_and(|s| VALID_INPUTS.contains(&s)) {
                        c.add(
                            IssueCode::EnumViolation,
                            format!(
                                "cli.schema.v1: inputs contains unsupported form \"{}\"",
                                js::to_display(input)
                            ),
                            At {
                                received: Some(input.clone()),
                                allowed: Some(str_list(&VALID_INPUTS)),
                                ..At::default()
                            },
                        );
                    }
                }
            }
            _ => c.add(
                IssueCode::TypeMismatch,
                "cli.schema.v1: inputs must be an array".into(),
                At {
                    received: Some(inputs.clone()),
                    ..At::default()
                },
            ),
        }
    }

    if let Some(globals) = doc.get("globals") {
        match globals {
            Value::Array(list) => {
                for g in list {
                    c.param_spec(g, None, true);
                }
            }
            _ => c.add(
                IssueCode::TypeMismatch,
                "cli.schema.v1: globals must be an array".into(),
                At {
                    received: Some(globals.clone()),
                    ..At::default()
                },
            ),
        }
    }

    match doc.get("operations") {
        Some(Value::Array(ops)) => {
            let mut seen: Vec<&str> = Vec::new();
            for op in ops {
                let Value::Object(op_obj) = op else {
                    c.add(
                        IssueCode::DocumentParse,
                        "cli.schema.v1: every operation must be an object".into(),
                        At {
                            received: Some(op.clone()),
                            ..At::default()
                        },
                    );
                    continue;
                };
                let Some(Value::String(op_name)) = op_obj.get("name") else {
                    c.add(
                        IssueCode::DocumentParse,
                        "cli.schema.v1: every operation needs a string name".into(),
                        At {
                            received: op_obj.get("name").cloned(),
                            ..At::default()
                        },
                    );
                    continue;
                };
                let op_name = op_name.as_str();
                let at_op = |received: Option<Value>, allowed: Option<Vec<Value>>| At {
                    op: Some(op_name),
                    param: None,
                    received,
                    allowed,
                };
                if !is_tool_or_op_name(op_name) {
                    c.add(
                        IssueCode::DocumentParse,
                        format!("cli.schema.v1: operation name \"{op_name}\" must match ^[a-z][a-z0-9_]*$"),
                        at_op(Some(Value::String(op_name.into())), None),
                    );
                } else if is_reserved_name(op_name) {
                    c.add(
                        IssueCode::ReservedName,
                        format!("cli.schema.v1: operation cannot be named \"{op_name}\" (reserved name)"),
                        at_op(Some(Value::String(op_name.into())), None),
                    );
                }
                if seen.contains(&op_name) {
                    c.add(
                        IssueCode::DocumentParse,
                        format!("cli.schema.v1: duplicate operation name \"{op_name}\""),
                        at_op(None, None),
                    );
                }
                seen.push(op_name);

                if let Some(access) = op_obj.get("access") {
                    if !access.as_str().is_some_and(|a| VALID_ACCESS.contains(&a)) {
                        c.add(
                            IssueCode::EnumViolation,
                            format!(
                                "cli.schema.v1: operation \"{op_name}\" has invalid access: {}",
                                js::stringify(access)
                            ),
                            at_op(Some(access.clone()), Some(str_list(&VALID_ACCESS))),
                        );
                    }
                }
                if let Some(returns) = op_obj.get("returns") {
                    if !returns.as_str().is_some_and(|r| VALID_RETURNS.contains(&r)) {
                        c.add(
                            IssueCode::EnumViolation,
                            format!(
                                "cli.schema.v1: operation \"{op_name}\" has invalid returns: {}",
                                js::stringify(returns)
                            ),
                            at_op(Some(returns.clone()), Some(str_list(&VALID_RETURNS))),
                        );
                    }
                }
                match op_obj.get("params") {
                    Some(Value::Array(params)) => {
                        for p in params {
                            c.param_spec(p, Some(op_name), false);
                        }
                    }
                    other => c.add(
                        IssueCode::DocumentParse,
                        format!("cli.schema.v1: operation \"{op_name}\" needs a params array"),
                        at_op(other.cloned(), None),
                    ),
                }
            }
        }
        other => c.add(
            IssueCode::DocumentParse,
            "cli.schema.v1: operations must be an array".into(),
            At {
                received: other.cloned(),
                ..At::default()
            },
        ),
    }

    if let Some(product) = doc.get("product") {
        c.product(product);
    }

    if !c.issues.is_empty() {
        return Err(c.issues);
    }
    // lmts accepts any value for fields it does not check (e.g. a non-string description);
    // the typed document needs them well-typed, so such a document is reported here.
    serde_json::from_value::<PublishedCliSchema>(value.clone()).map_err(|e| {
        let mut c = Checker { issues: Vec::new() };
        c.add(IssueCode::TypeMismatch, format!("cli.schema.v1: {e}"), At::default());
        c.issues
    })
}

/// Throwing-style wrapper around [`parse_published_schema`].
pub fn parse_published_schema_or_err(value: &Value) -> Result<PublishedCliSchema, SchemaValidationError> {
    parse_published_schema(value).map_err(|issues| {
        let first = &issues[0];
        let mut details = Record::new();
        details.insert("code".into(), Value::String(first.code.as_str().into()));
        details.insert("synopsis".into(), Value::String(first.synopsis.clone()));
        details.insert("issues".into(), serde_json::to_value(&issues).unwrap_or(Value::Null));
        SchemaValidationError::new(
            first.op.clone().unwrap_or_else(|| "<unknown>".into()),
            first.message.clone(),
            details,
        )
    })
}

/// Look up an op in a published document.
pub fn find_operation<'a>(doc: &'a PublishedCliSchema, name: &str) -> Option<&'a OperationSchema> {
    doc.operations.iter().find(|op| op.name == name)
}
