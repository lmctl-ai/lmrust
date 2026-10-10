//! Native (wrapper-less) tools embedding the runner, lmapi protocol section 6.1: the
//! dispatch guard, configurable globals, a published `inputs` subset, tool-specific error
//! codes, domain statuses and an omitted `product`.

use lmrust::server::{
    is_lmapi_invocation, maybe_run_cli_io, run_cli_io, schema_document, CliError, CliSpec, ErrorCode, GlobalRole,
    OpDef, Policy,
};
use lmrust::{Access, InputForm, OperationSchema, ParamSpec, ParamType, PublishedCliSchema};
use serde_json::{json, Value};
use std::collections::HashMap;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// A native tool shape: `lookup` (read, positional host, own `--format`) and `flush` (write).
fn probe() -> CliSpec<Policy> {
    CliSpec::with_policy("probe")
        .version(env!("CARGO_PKG_VERSION"))
        .op(
            OpDef::new("lookup", Access::Read, "Look a host up", |p, policy: &Policy| {
                let host = p["host"].as_str().unwrap_or_default();
                if host == "missing.example" {
                    // A search that matched nothing is a result, not a failure.
                    return Ok(json!({"status": "no_match", "host": host}));
                }
                if host == "bad" {
                    return Err(CliError::new(ErrorCode::NotFound, "no such host").tool_code("E_NXDOMAIN"));
                }
                Ok(json!({"status": "ok", "host": host, "format": p.get("format"), "dryRun": policy.dry_run}))
            })
            .schema(
                OperationSchema::new("lookup")
                    .access(Access::Read)
                    .describe("Look a host up")
                    .param(ParamSpec::new("host", ParamType::String).positional())
                    .param(ParamSpec::new("format", ParamType::String).optional()),
            ),
        )
        .op(OpDef::new(
            "flush",
            Access::Write,
            "Flush the cache",
            |_, policy: &Policy| Ok(json!({"flushed": !policy.dry_run, "dryRun": policy.dry_run})),
        ))
}

fn run(spec: &CliSpec<Policy>, args: &[&str], env: &[(&str, &str)]) -> (i32, Value) {
    let env: HashMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let mut out = Vec::new();
    let code = run_cli_io(spec, &argv(args), &mut out, &env);
    let text = String::from_utf8(out).unwrap();
    (code, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

// ---------------------------------------------------------------------------------------------
// 1. Dispatch guard
// ---------------------------------------------------------------------------------------------

#[test]
fn guard_accepts_lmapi_shaped_argv() {
    let spec = probe();
    for args in [
        &["--schema"][..],
        &["--list"],
        &["--explain", "lookup"],
        &["--explain=lookup"],
        &["--input-json", r#"{"op":"lookup","params":{"host":"a"}}"#],
        &["--input-json={}"],
        &["--input-file", "doc.json"],
        &["--inputFile", "doc.json"],
        &["--input-graphql", "{ lookup(host: \"a\") }"],
        &["--dryrun"],
        &["--dry-run"],
        &["--readonly"],
        &["--read-only"],
        &["--confirm"],
        &["--i-understand"],
        &["lookup", "example.com"],
        &["lookup", "--help"],
        &["--dryrun", "flush"],
        &["--readonly", "--", "lookup", "x"],
        &["flush", "--dryrun"],
    ] {
        assert!(is_lmapi_invocation(&spec, &argv(args)), "{args:?}");
    }
}

#[test]
fn guard_leaves_native_argv_alone() {
    let spec = probe();
    for args in [
        &[][..],
        &["--help"],
        &["-h"],
        &["--json"],
        &["--format", "table"],
        &["--version"],
        &["-v", "example.com"],
        &["scan", "example.com"],
        &["scan", "--schema"],
        &["--format", "lookup"],
        &["example.com"],
        &["--", "scan"],
        &["--"],
        &["--input-jsonx"],
    ] {
        assert!(!is_lmapi_invocation(&spec, &argv(args)), "{args:?}");
    }
}

#[test]
fn maybe_run_returns_none_for_native_argv_and_runs_otherwise() {
    let spec = probe();
    let env = HashMap::new();
    let mut out = Vec::new();
    assert_eq!(maybe_run_cli_io(&spec, &argv(&["--help"]), &mut out, &env), None);
    assert!(out.is_empty(), "nothing is written for native argv");
    let code = maybe_run_cli_io(&spec, &argv(&["lookup", "example.com"]), &mut out, &env);
    assert_eq!(code, Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["host"],
        json!("example.com")
    );
}

#[test]
fn guard_follows_configured_globals() {
    let spec = probe().without_global("dryrun").rename_global("schema", "lmapi-schema");
    assert!(!is_lmapi_invocation(&spec, &argv(&["--dryrun"])));
    assert!(!is_lmapi_invocation(&spec, &argv(&["--schema"])));
    assert!(is_lmapi_invocation(&spec, &argv(&["--lmapi-schema"])));
    let (code, doc) = run(&spec, &["--lmapi-schema"], &[]);
    assert_eq!((code, doc["tool"].clone()), (0, json!("probe")));
}

// ---------------------------------------------------------------------------------------------
// 2. Configurable globals
// ---------------------------------------------------------------------------------------------

fn global_names(doc: &Value) -> Vec<String> {
    doc["globals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn default_globals_are_unchanged() {
    let (_, doc) = run(&probe(), &["--schema"], &[]);
    assert_eq!(
        global_names(&doc),
        [
            "dryrun",
            "readonly",
            "confirm",
            "json",
            "help",
            "list",
            "schema",
            "explain",
            "format",
            "input-json",
            "input-graphql",
            "input-file"
        ]
    );
    let (code, text) = run(&probe(), &["--explain", "lookup", "--format", "text"], &[]);
    assert_eq!(code, 0);
    assert!(text.as_str().unwrap().starts_with("probe lookup\n"));
    assert_eq!(GlobalRole::Format.default_name(), "format");
}

#[test]
fn a_dropped_global_is_not_published_and_the_tool_owns_the_flag() {
    let spec = probe().without_global("format");
    let (_, doc) = run(&spec, &["--schema"], &[]);
    assert!(!global_names(&doc).contains(&"format".to_string()));
    assert_eq!(global_names(&doc).len(), 11);
    assert!(lmrust::parse_published_schema(&doc).is_ok());
    // `--format` now reaches the op's own param.
    let (code, out) = run(&spec, &["lookup", "a", "--format", "table"], &[]);
    assert_eq!((code, out["format"].clone()), (0, json!("table")));
    // With the global `--format`, the same flag would have been taken as the runner global.
    let (_, out) = run(&probe(), &["lookup", "a", "--format", "table"], &[]);
    assert_eq!(
        out["format"],
        json!("table"),
        "op params win over globals in either case"
    );
    // Explain without `format`: JSON from --explain, text from `<op> --help`. A trailing
    // `--format text` is no longer a runner flag: `text` becomes the op token.
    let (code, schema) = run(&spec, &["--explain", "lookup"], &[]);
    assert_eq!((code, schema["name"].clone()), (0, json!("lookup")));
    let (code, out) = run(&spec, &["--explain", "lookup", "--format", "text"], &[]);
    assert_eq!(
        (code, out["error"]["message"].clone()),
        (2, json!("Unknown operation: text"))
    );
    let (code, text) = run(&spec, &["lookup", "--help"], &[]);
    assert_eq!(code, 0);
    assert!(text.as_str().unwrap().contains("probe lookup <host>"));
    // Tool help lists only the globals in use.
    let (_, help) = run(&spec, &["--help"], &[]);
    assert!(!help.as_str().unwrap().contains("--format"));
}

#[test]
fn a_renamed_global_is_published_and_parsed_under_its_new_name() {
    let spec = probe()
        .rename_global("format", "explain-format")
        .rename_global("dryrun", "simulate");
    let (_, doc) = run(&spec, &["--schema"], &[]);
    let names = global_names(&doc);
    assert!(names.contains(&"explain-format".to_string()) && names.contains(&"simulate".to_string()));
    assert!(!names.contains(&"format".to_string()) && !names.contains(&"dryrun".to_string()));
    let (code, text) = run(&spec, &["--explain", "lookup", "--explain-format", "text"], &[]);
    assert_eq!(code, 0);
    assert!(text.as_str().unwrap().starts_with("probe lookup\n"));
    // The renamed policy flag (and its kept alias) set the policy.
    let (_, out) = run(&spec, &["flush", "--simulate"], &[]);
    assert_eq!(out, json!({"flushed": false, "dryRun": true}));
    let (_, out) = run(&spec, &["flush", "--dry-run"], &[]);
    assert_eq!(out["dryRun"], json!(true));
    // The old name is no longer a global (an op with a declared schema rejects it).
    let (code, out) = run(&spec, &["lookup", "a", "--dryrun"], &[]);
    assert_eq!(
        (code, out["error"]["data"]["issues"][0]["code"].clone()),
        (2, json!("UNKNOWN_PARAM"))
    );
}

#[test]
fn dropping_a_policy_flag_keeps_the_env_floor() {
    let spec = probe().without_global("dryrun").without_global("readonly");
    let (code, out) = run(&spec, &["lookup", "a", "--dryrun"], &[]);
    assert_eq!((code, out["error"]["code"].clone()), (2, json!("VALIDATION_ERROR")));
    let (code, out) = run(&spec, &["flush"], &[("PROBE_DRYRUN", "1")]);
    assert_eq!((code, out), (0, json!({"flushed": false, "dryRun": true})));
}

// ---------------------------------------------------------------------------------------------
// 3. Published inputs subset
// ---------------------------------------------------------------------------------------------

fn form_refused(out: &Value, form: &str) {
    assert_eq!(out["error"]["code"], json!("VALIDATION_ERROR"));
    let issue = &out["error"]["data"]["issues"][0];
    assert_eq!(issue["code"], json!("FORM_NOT_ALLOWED"));
    assert_eq!(issue["received"], json!(form));
    assert!(!issue["synopsis"].as_str().unwrap().is_empty() && !issue["usage"].as_str().unwrap().is_empty());
}

#[test]
fn flags_only_tool_refuses_documents() {
    let spec = probe().inputs([InputForm::Flags]);
    let (_, doc) = run(&spec, &["--schema"], &[]);
    assert_eq!(doc["inputs"], json!(["flags"]));
    let (code, out) = run(&spec, &["lookup", "--input-json", r#"{"host":"a"}"#], &[]);
    assert_eq!(code, 2);
    form_refused(&out, "json");
    assert_eq!(out["error"]["data"]["issues"][0]["allowed"], json!(["flags"]));
    let (code, out) = run(
        &spec,
        &["--input-json", r#"{"op":"lookup","params":{"host":"a"}}"#],
        &[],
    );
    assert_eq!(code, 2);
    form_refused(&out, "json");
    let (code, out) = run(&spec, &["--input-graphql", r#"{ lookup(host: "a") }"#], &[]);
    assert_eq!(code, 2);
    form_refused(&out, "graphql");
    let (code, out) = run(&spec, &["lookup", "a"], &[]);
    assert_eq!((code, out["status"].clone()), (0, json!("ok")));
}

#[test]
fn json_only_tool_refuses_flags_but_accepts_globals_alone() {
    let path = std::env::temp_dir().join(format!("lmrust-native-{}.json", std::process::id()));
    std::fs::write(&path, r#"{"host":"from-file"}"#).unwrap();
    let spec = probe().inputs([InputForm::Json]);
    let (code, out) = run(&spec, &["lookup", "a"], &[]);
    assert_eq!(code, 2);
    form_refused(&out, "flags");
    assert_eq!(out["error"]["data"]["issues"][0]["allowed"], json!(["json"]));
    let (code, out) = run(&spec, &["lookup", "--input-file", path.to_str().unwrap()], &[]);
    assert_eq!((code, out["host"].clone()), (0, json!("from-file")));
    // No op flags at all (only runner globals) uses no input form.
    let (code, out) = run(&spec, &["flush", "--dryrun"], &[]);
    assert_eq!((code, out["dryRun"].clone()), (0, json!(true)));
    let (code, out) = run(
        &spec.inputs([InputForm::Json]),
        &["lookup", "--input-graphql", "{ lookup(host: \"a\") }"],
        &[],
    );
    assert_eq!(code, 2);
    form_refused(&out, "graphql");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn an_unparsable_flags_invocation_counts_as_the_flags_form() {
    let (code, out) = run(&probe().inputs([InputForm::Json]), &["lookup", "--nope"], &[]);
    assert_eq!(code, 2);
    form_refused(&out, "flags");
    let (code, out) = run(&probe().inputs(Vec::<InputForm>::new()), &["lookup", "a"], &[]);
    assert_eq!(code, 2);
    assert!(out["error"]["message"].as_str().unwrap().contains("(inputs: none)"));
}

#[test]
fn from_document_takes_the_published_inputs() {
    let doc: PublishedCliSchema = serde_json::from_value(json!({
        "schema": "cli.schema.v1", "tool": "native", "version": "2.0.0", "inputs": ["flags", "graphql"],
        "operations": [{"name": "ping", "access": "read", "params": []}]
    }))
    .unwrap();
    let spec = CliSpec::from_document_with_policy(&doc).handle("ping", |_, _| Ok(json!({"pong": true})));
    assert_eq!(spec.inputs, [InputForm::Flags, InputForm::Graphql]);
    let (code, out) = run(&spec, &["ping", "--input-json", "{}"], &[]);
    assert_eq!(code, 2);
    form_refused(&out, "json");
    let (code, out) = run(&spec, &["ping", "--input-graphql", "{ ping }"], &[]);
    assert_eq!((code, out), (0, json!({"pong": true})));
    assert_eq!(
        serde_json::to_value(schema_document(&spec)).unwrap()["inputs"],
        json!(["flags", "graphql"])
    );
}

// ---------------------------------------------------------------------------------------------
// 4. Tool-specific error codes
// ---------------------------------------------------------------------------------------------

#[test]
fn tool_code_is_kept_under_data_and_exit_code_follows_the_standard_code() {
    for code in ErrorCode::ALL {
        let spec = CliSpec::with_policy("t").op(OpDef::new("op", Access::Read, "", move |_, _| {
            Err(CliError::new(code, "failed")
                .tool_code("E_TOOL")
                .with("detail", json!(1)))
        }));
        let (exit, out) = run(&spec, &["op"], &[]);
        assert_eq!(exit, code.exit_code(), "{code}");
        assert_eq!(
            out,
            json!({"success": false, "error": {"code": code.as_str(), "message": "failed", "data": {"tool_code": "E_TOOL", "detail": 1}}}),
            "{code}"
        );
    }
    let (exit, out) = run(&probe(), &["lookup", "bad"], &[]);
    assert_eq!(
        (
            exit,
            out["error"]["code"].clone(),
            out["error"]["data"]["tool_code"].clone()
        ),
        (4, json!("NOT_FOUND"), json!("E_NXDOMAIN"))
    );
    // A numeric legacy code is kept as is.
    let e = CliError::new(ErrorCode::CommandFailed, "x").tool_code(3);
    assert_eq!((e.exit_code(), e.data.unwrap()["tool_code"].clone()), (1, json!(3)));
}

// ---------------------------------------------------------------------------------------------
// 5. Domain statuses
// ---------------------------------------------------------------------------------------------

#[test]
fn a_domain_status_in_the_result_exits_zero() {
    // lmapi 6.1: "no match" is a result (exit 0 with the status inside), never a legacy exit code.
    let (code, out) = run(&probe(), &["lookup", "missing.example"], &[]);
    assert_eq!(
        (code, out),
        (0, json!({"status": "no_match", "host": "missing.example"}))
    );
}

// ---------------------------------------------------------------------------------------------
// 6. product omitted
// ---------------------------------------------------------------------------------------------

#[test]
fn product_omitted_end_to_end() {
    // A checker that would fail the test if consulted: with no product it never runs (and so
    // nothing is spawned).
    let spec = probe().product_checker(|_, _| panic!("product check must not run without a product"));
    let doc = serde_json::to_value(schema_document(&spec)).unwrap();
    assert!(
        doc.get("product").is_none(),
        "product is omitted from the published document"
    );
    assert_eq!(doc["version"], json!(env!("CARGO_PKG_VERSION")));
    let (code, out) = run(&spec, &["lookup", "a"], &[]);
    assert_eq!((code, out["status"].clone()), (0, json!("ok")));
    let (_, printed) = run(&spec, &["--schema"], &[]);
    assert!(printed.get("product").is_none());
    assert_eq!(printed["version"], json!(env!("CARGO_PKG_VERSION")));
    assert!(lmrust::parse_published_schema(&printed).is_ok());
}

#[test]
fn maybe_run_cli_against_the_process_stdout() {
    // Native argv: nothing runs and nothing is printed.
    assert_eq!(lmrust::server::maybe_run_cli(&probe(), ["--help"]), None);
    // lmapi-shaped argv runs against the real stdout (prints one small envelope here).
    assert_eq!(lmrust::server::maybe_run_cli(&probe(), ["--explain", "nope"]), Some(2));
}
