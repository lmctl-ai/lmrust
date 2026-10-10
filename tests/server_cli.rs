//! Port of lmts `tests/server-cli.test.ts` (in-process runner tests) and
//! `tests/run-cli.test.ts` (end-to-end tests of the `tests/fixtures/test-cli.ts` tool).
//!
//! lmts spawns the fixture CLI with `bun`; here the equivalent `testtool` [`CliSpec`] is
//! driven in-process through `run_cli_io` with an explicit env map. Env floors are passed
//! through that map instead of mutating `process.env`.

use lmrust::server::{
    check_policy, dry_run_from_env, read_only_from_env, run_cli_io, synthesize_schema, CheckContext, CliError, CliSpec,
    ErrorCode, OpDef, Policy,
};
use lmrust::{parse_published_schema, Access, OperationSchema, ParamSpec, ParamType};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

struct Run {
    code: i32,
    stdout: String,
}

impl Run {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", self.stdout))
    }
}

fn run_env<C>(spec: &CliSpec<C>, argv: &[&str], env: &HashMap<String, String>) -> Run {
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let mut out: Vec<u8> = Vec::new();
    let code = run_cli_io(spec, &argv, &mut out, env);
    Run {
        code,
        stdout: String::from_utf8(out).expect("utf-8 stdout"),
    }
}

fn run<C>(spec: &CliSpec<C>, argv: &[&str]) -> Run {
    run_env(spec, argv, &HashMap::new())
}

fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A unique temp dir (removed on drop).
struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "{prefix}{}-{}-{nanos}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn path_str(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// server-cli.test.ts: the `mytool` config
// ---------------------------------------------------------------------------

struct TestContext {
    policy: Policy,
}

fn test_op_schema() -> OperationSchema {
    OperationSchema::new("greet")
        .access(Access::Read)
        .describe("Say hello")
        .param(ParamSpec::new("name", ParamType::String).describe("Name to greet"))
        .param(
            ParamSpec::new("shout", ParamType::Boolean)
                .optional()
                .describe("Shout greeting"),
        )
}

fn write_op_schema() -> OperationSchema {
    OperationSchema::new("save")
        .access(Access::Write)
        .describe("Save data")
        .param(ParamSpec::new("item", ParamType::String).describe("Item name"))
}

fn create_test_config() -> CliSpec<TestContext> {
    CliSpec::new("mytool", |policy: &Policy| TestContext { policy: *policy })
        .version("1.2.3")
        .description("My test tool CLI")
        .op(
            OpDef::new("greet", Access::Read, "Say hello", |params, _ctx: &TestContext| {
                let name = params["name"].as_str().unwrap_or_default();
                let shout = params.get("shout") == Some(&Value::Bool(true));
                let msg = format!("Hello, {name}!");
                Ok(json!({ "message": if shout { msg.to_uppercase() } else { msg } }))
            })
            .schema(test_op_schema()),
        )
        .op(
            OpDef::new("save", Access::Write, "Save data", |params, ctx: &TestContext| {
                if ctx.policy.read_only {
                    return Err(CliError::new(
                        ErrorCode::ReadOnlyViolation,
                        "Cannot save in read-only mode",
                    ));
                }
                if ctx.policy.dry_run {
                    return Ok(json!({ "success": true, "dryRun": true, "simulated": "save", "item": params["item"] }));
                }
                Ok(json!({ "success": true, "saved": params["item"] }))
            })
            .schema(write_op_schema()),
        )
        .op(OpDef::new(
            "fail",
            Access::Read,
            "Always fails",
            |_, _: &TestContext| {
                // lmts: `throw new Error('generic execution failure')` -> toCliError
                Err(CliError::from("generic execution failure"))
            },
        ))
        .op(OpDef::new(
            "cli_fail",
            Access::Read,
            "Fails with CliError",
            |_, _: &TestContext| Err(CliError::new(ErrorCode::Conflict, "resource conflict").with("id", json!(123))),
        ))
        // No input schema -> synthesize_schema
        .op(OpDef::new(
            "legacy_raw",
            Access::Dynamic,
            "Legacy op without schema",
            |params, _: &TestContext| Ok(json!({ "received": params })),
        ))
        .op(OpDef::new("panics", Access::Read, "Panics", |_, _: &TestContext| {
            panic!("handler panicked")
        }))
}

// describe('server/cli env helpers')

#[test]
fn read_only_from_env_recognizes_true_values_and_ignores_false_values() {
    assert!(!read_only_from_env("mytool", &HashMap::new()));
    for v in ["1", "true", "yes", "on"] {
        assert!(read_only_from_env("mytool", &env_of(&[("MYTOOL_READONLY", v)])), "{v}");
    }
    for v in ["0", "false"] {
        assert!(!read_only_from_env("mytool", &env_of(&[("MYTOOL_READONLY", v)])), "{v}");
    }
}

#[test]
fn dry_run_from_env_recognizes_true_values_and_ignores_false_values() {
    assert!(!dry_run_from_env("mytool", &HashMap::new()));
    assert!(dry_run_from_env("mytool", &env_of(&[("MYTOOL_DRYRUN", "1")])));
    assert!(dry_run_from_env("mytool", &env_of(&[("MYTOOL_DRYRUN", "YES")])));
    assert!(!dry_run_from_env("mytool", &env_of(&[("MYTOOL_DRYRUN", "false")])));
}

#[test]
fn synthesize_schema_builds_schema_with_additional_params_true() {
    let op: OpDef<()> = OpDef::new("synth_op", Access::Read, "synthetic op", |_, _| Ok(json!({})));
    let schema = synthesize_schema(&op);
    assert_eq!(schema.name, "synth_op");
    assert_eq!(schema.access, Some(Access::Read));
    assert_eq!(schema.additional_params, Some(true));
    assert!(schema.params.is_empty());
}

// describe('server/cli runCli global actions')

#[test]
fn empty_argv_displays_tool_help_and_exits_0() {
    let r = run(&create_test_config(), &[]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("mytool"));
    assert!(r.stdout.contains("My test tool CLI"));
    assert!(r.stdout.contains("Global options:"));
}

#[test]
fn help_and_h_at_root_display_tool_help_and_exit_0() {
    let config = create_test_config();
    let r1 = run(&config, &["--help"]);
    assert_eq!(r1.code, 0);
    assert!(r1.stdout.contains("Global options:"));
    let r2 = run(&config, &["-h"]);
    assert_eq!(r2.code, 0);
    assert!(r2.stdout.contains("Global options:"));
}

#[test]
fn list_prints_ops_as_json_array_and_exits_0() {
    let r = run(&create_test_config(), &["--list"]);
    assert_eq!(r.code, 0);
    let parsed = r.json();
    let arr = parsed.as_array().expect("array");
    assert!(arr.iter().any(|o| o["name"] == "greet"));
}

#[test]
fn schema_prints_cli_schema_v1_json_document_and_exits_0() {
    let r = run(&create_test_config(), &["--schema"]);
    assert_eq!(r.code, 0);
    let parsed = r.json();
    assert_eq!(parsed["schema"], "cli.schema.v1");
    assert_eq!(parsed["tool"], "mytool");
    assert_eq!(parsed["version"], "1.2.3");
    assert!(!parsed["operations"].as_array().unwrap().is_empty());
}

#[test]
fn explain_handles_json_and_text_formats() {
    let config = create_test_config();
    // 1. Missing op name
    let missing = run(&config, &["--explain"]);
    assert_eq!(missing.code, 2);
    assert_eq!(missing.json()["error"]["code"], "INVALID_REQUEST");

    // 2. Unknown op name
    let unknown = run(&config, &["--explain", "nonexistent"]);
    assert_eq!(unknown.code, 2);
    assert_eq!(unknown.json()["error"]["code"], "INVALID_REQUEST");

    // 3. JSON format (default)
    let json_fmt = run(&config, &["--explain", "greet"]);
    assert_eq!(json_fmt.code, 0);
    assert_eq!(json_fmt.json()["name"], "greet");

    // 4. Text format
    let text_fmt = run(&config, &["--explain", "greet", "--format", "text"]);
    assert_eq!(text_fmt.code, 0);
    assert!(text_fmt.stdout.contains("mytool greet"));
    assert!(text_fmt.stdout.contains("--name"));
}

#[test]
fn no_operation_specified_and_no_inferred_op_exits_2() {
    let r = run(&create_test_config(), &["--confirm", "--dryrun"]);
    assert_eq!(r.code, 2);
    let parsed = r.json();
    assert_eq!(parsed["error"]["code"], "INVALID_REQUEST");
    assert!(parsed["error"]["message"]
        .as_str()
        .unwrap()
        .contains("No operation specified"));
}

// describe('server/cli runCli op dispatch and arguments')

#[test]
fn successful_flag_based_execution() {
    let r = run(&create_test_config(), &["greet", "--name", "Alice", "--shout"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json(), json!({ "message": "HELLO, ALICE!" }));
}

#[test]
fn leading_globals_before_op_name() {
    let r = run(&create_test_config(), &["--dryrun", "greet", "--name", "Bob"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json(), json!({ "message": "Hello, Bob!" }));
}

#[test]
fn negated_global_flag_before_op() {
    let r = run(&create_test_config(), &["--no-dryrun", "greet", "--name", "Carol"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json(), json!({ "message": "Hello, Carol!" }));
}

#[test]
fn synthesized_op_with_double_dash_separator() {
    let r = run(&create_test_config(), &["legacy_raw", "--", "arg1", "arg2"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
}

#[test]
fn op_level_help_displays_op_help_text() {
    let r = run(&create_test_config(), &["greet", "--help"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("mytool greet"));
    assert!(r.stdout.contains("Say hello"));
}

#[test]
fn unknown_op_name_reports_error_and_exits_2() {
    let r = run(&create_test_config(), &["unknown_command"]);
    assert_eq!(r.code, 2);
    let err = r.json();
    assert_eq!(err["error"]["code"], "INVALID_REQUEST");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Unknown operation: unknown_command"));
}

#[test]
fn synthesize_schema_op_runs_successfully() {
    let r = run(&create_test_config(), &["legacy_raw", "--foo", "bar", "--count", "10"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json()["received"]["foo"], "bar");
}

#[test]
fn op_throwing_generic_error_outputs_error_envelope_and_exits_1() {
    // lmts names this "INTERNAL_ERROR envelope" but asserts only success/message/exit 1;
    // lmts toCliError maps a plain Error to UNKNOWN_ERROR (exit 1), as lmrust does.
    let r = run(&create_test_config(), &["fail"]);
    assert_eq!(r.code, 1);
    let err = r.json();
    assert_eq!(err["success"], false);
    assert_eq!(err["error"]["code"], "UNKNOWN_ERROR");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("generic execution failure"));
}

#[test]
fn op_panicking_outputs_unknown_error_envelope_and_exits_1() {
    // Rust analogue of a handler throwing: a panic is caught and enveloped.
    let r = run(&create_test_config(), &["panics"]);
    assert_eq!(r.code, 1);
    let err = r.json();
    assert_eq!(err["success"], false);
    assert_eq!(err["error"]["code"], "UNKNOWN_ERROR");
    assert_eq!(err["error"]["message"], "handler panicked");
}

#[test]
fn op_throwing_cli_error_outputs_structured_envelope_and_mapped_exit_code() {
    let r = run(&create_test_config(), &["cli_fail"]);
    assert_eq!(r.code, 5); // Conflict -> 5
    let err = r.json();
    assert_eq!(err["error"]["code"], "CONFLICT");
    assert_eq!(err["error"]["data"], json!({ "id": 123 }));
}

// describe('server/cli document inputs and MIXED_INPUT')

#[test]
fn input_json_execution_and_envelope_inference() {
    let config = create_test_config();
    let r1 = run(&config, &["greet", "--input-json", r#"{"name":"Dave"}"#]);
    assert_eq!(r1.code, 0);
    assert_eq!(r1.json(), json!({ "message": "Hello, Dave!" }));

    let r2 = run(&config, &["--input-json", r#"{"op":"greet","params":{"name":"Eve"}}"#]);
    assert_eq!(r2.code, 0);
    assert_eq!(r2.json(), json!({ "message": "Hello, Eve!" }));
}

#[test]
fn input_graphql_execution_and_envelope_inference() {
    let config = create_test_config();
    let r1 = run(
        &config,
        &[
            "greet",
            "--input-graphql",
            r#"query { greet(name: "Frank") { __typename } }"#,
        ],
    );
    assert_eq!(r1.code, 0, "{}", r1.stdout);
    assert_eq!(r1.json(), json!({ "message": "Hello, Frank!" }));

    let r2 = run(
        &config,
        &["--input-graphql", r#"query { greet(name: "Grace") { __typename } }"#],
    );
    assert_eq!(r2.code, 0, "{}", r2.stdout);
    assert_eq!(r2.json(), json!({ "message": "Hello, Grace!" }));
}

#[test]
fn input_file_execution_and_errors() {
    let config = create_test_config();
    let tmp = TempDir::new("lmrust-cli-doc-");
    let valid = tmp.0.join("doc.json");
    let invalid = tmp.0.join("nonexistent.json");
    std::fs::write(&valid, r#"{"name":"Heidi"}"#).unwrap();
    let valid = valid.to_string_lossy().into_owned();
    let invalid = invalid.to_string_lossy().into_owned();

    let r = run(&config, &["greet", "--input-file", &valid]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json(), json!({ "message": "Hello, Heidi!" }));

    let r_err = run(&config, &["greet", "--input-file", &invalid]);
    assert_eq!(r_err.code, 2);
    assert_eq!(r_err.json()["error"]["code"], "INVALID_REQUEST");

    let r_root = run(&config, &["--input-file", &invalid]);
    assert_eq!(r_root.code, 2);
}

#[test]
fn mixed_input_prevention() {
    let config = create_test_config();
    let r_flag = run(&config, &["greet", "--input-json", r#"{"name":"Ivan"}"#, "--shout"]);
    assert_eq!(r_flag.code, 2);
    let err = r_flag.json();
    assert_eq!(err["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(err["error"]["data"]["issues"][0]["code"], "MIXED_INPUT");

    let r_bad = run(
        &config,
        &["greet", "--input-json", r#"{"name":"Ivan"}"#, "--invalid-flag"],
    );
    assert_eq!(r_bad.code, 2);
}

#[test]
fn validation_failure_in_resolve_input_maps_to_validation_error_envelope() {
    let config = create_test_config();
    let r = run(&config, &["greet", "--shout"]);
    assert_eq!(r.code, 2);
    let err = r.json();
    assert_eq!(err["error"]["code"], "VALIDATION_ERROR");
    assert!(err["error"]["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["code"] == "MISSING_REQUIRED"));

    let r_unknown = run(&config, &["greet", "--name", "Judy", "--nonexistent"]);
    assert_eq!(r_unknown.code, 2);
    assert!(r_unknown.json()["error"]["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["code"] == "UNKNOWN_PARAM"));
}

#[test]
fn policy_floors_from_environment_variables_cannot_be_bypassed() {
    let config = create_test_config();
    let r = run_env(
        &config,
        &["save", "--item", "file.txt"],
        &env_of(&[("MYTOOL_READONLY", "1")]),
    );
    assert_eq!(r.code, 6);
    assert_eq!(r.json()["error"]["code"], "READONLY_VIOLATION");

    let r_dry = run_env(
        &config,
        &["save", "--item", "file.txt"],
        &env_of(&[("MYTOOL_DRYRUN", "1")]),
    );
    assert_eq!(r_dry.code, 0);
    assert_eq!(
        r_dry.json(),
        json!({ "success": true, "dryRun": true, "simulated": "save", "item": "file.txt" })
    );
}

// ---------------------------------------------------------------------------
// run-cli.test.ts: the `testtool` fixture (tests/fixtures/test-cli.ts)
// ---------------------------------------------------------------------------

const TESTTOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

fn testtool() -> CliSpec<Policy> {
    let status_schema = OperationSchema::new("lm_status")
        .access(Access::Read)
        .describe("Structured status")
        .param(
            ParamSpec::new("path", ParamType::String)
                .optional()
                .with_default(json!(".")),
        )
        .param(ParamSpec::new("ignored", ParamType::Boolean).optional());
    let commit_schema = OperationSchema::new("commit")
        .access(Access::Write)
        .describe("Record changes to the repository")
        .param(ParamSpec::new("message", ParamType::String).describe("Commit message"))
        .param(
            ParamSpec::new("path", ParamType::String)
                .optional()
                .with_default(json!("."))
                .describe("Working dir"),
        );
    let checkout_schema = OperationSchema::new("checkout")
        .access(Access::Write)
        .describe("Switch branches or restore files")
        .param(
            ParamSpec::new("branch", ParamType::String)
                .optional()
                .describe("Branch to checkout"),
        )
        .additional_params(true);
    let log_schema = OperationSchema::new("log")
        .access(Access::Read)
        .describe("Show commit logs")
        .param(
            ParamSpec::new("maxCount", ParamType::Number)
                .optional()
                .aliases(["max-count"])
                .describe("Max commits"),
        );

    CliSpec::with_policy("testtool")
        .version(TESTTOOL_VERSION)
        .description("Test CLI tool for runCli integration tests")
        .op(OpDef::new("lm_status", Access::Read, "Structured status", |p, _: &Policy| {
            Ok(json!({ "clean": true, "workdir": p.get("path").cloned().unwrap_or(json!(".")) }))
        })
        .schema(status_schema))
        .op(OpDef::new(
            "commit",
            Access::Write,
            "Record changes to the repository",
            |p, policy: &Policy| {
                if let Some(guarded) = check_policy(Access::Write, policy, &CheckContext::new("commit"))? {
                    return Ok(Value::Object(guarded));
                }
                Ok(json!({ "success": true, "message": p.get("message") }))
            },
        )
        .schema(commit_schema))
        .op(OpDef::new(
            "checkout",
            Access::Write,
            "Switch branches or restore files",
            |p, policy: &Policy| {
                if let Some(guarded) = check_policy(Access::Write, policy, &CheckContext::new("checkout"))? {
                    return Ok(Value::Object(guarded));
                }
                Ok(json!({ "success": true, "branch": p.get("branch") }))
            },
        )
        .schema(checkout_schema))
        .op(OpDef::new("log", Access::Read, "Show commit logs", |p, _: &Policy| {
            Ok(json!({ "commits": [{ "author": "Test User", "count": p.get("maxCount").cloned().unwrap_or(json!(10)) }] }))
        })
        .schema(log_schema))
}

fn space_dir() -> TempDir {
    let dir = TempDir::new("cli test space ");
    std::fs::write(dir.0.join("README.md"), "# Test\n").unwrap();
    dir
}

#[test]
fn run_cli_1_lm_status_path_with_space_succeeds() {
    let space = space_dir();
    let r = run(&testtool(), &["lm_status", "--path", &space.path_str()]);
    assert_eq!(r.code, 0);
    let j = r.json();
    assert_eq!(j["workdir"], space.path_str());
    assert_eq!(j["clean"], true);
}

#[test]
fn run_cli_2_lm_status_confirm_succeeds() {
    let space = space_dir();
    let r = run(&testtool(), &["lm_status", "--confirm", "--path", &space.path_str()]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json()["clean"], true);
}

#[test]
fn run_cli_3_lm_status_bogus_unknown_param_with_synopsis() {
    let space = space_dir();
    let r = run(&testtool(), &["lm_status", "--bogus", "--path", &space.path_str()]);
    assert_eq!(r.code, 2);
    let j = r.json();
    assert_eq!(j["success"], false);
    assert_eq!(j["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(j["error"]["data"]["op"], "lm_status");
    let issues = j["error"]["data"]["issues"].as_array().expect("issues array");
    assert!(!issues.is_empty());
    assert_eq!(issues[0]["code"], "UNKNOWN_PARAM");
    assert!(issues[0]["synopsis"].as_str().unwrap().contains("--path"));
}

#[test]
fn run_cli_4_input_json_envelope_works() {
    let space = space_dir();
    let doc = json!({ "op": "lm_status", "params": { "path": space.path_str(), "ignored": true } }).to_string();
    let r = run(&testtool(), &["--input-json", &doc]);
    assert_eq!(r.code, 0);
    let j = r.json();
    assert_eq!(j["workdir"], space.path_str());
    assert_eq!(j["clean"], true);
}

#[test]
fn run_cli_5_input_json_plus_path_is_mixed_input() {
    let space = space_dir();
    let doc = json!({ "op": "lm_status", "params": { "ignored": true } }).to_string();
    let r = run(
        &testtool(),
        &["lm_status", "--input-json", &doc, "--path", &space.path_str()],
    );
    assert_eq!(r.code, 2);
    let j = r.json();
    assert_eq!(j["success"], false);
    assert_eq!(j["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(j["error"]["data"]["issues"][0]["code"], "MIXED_INPUT");
}

#[test]
fn run_cli_6_input_json_with_dryrun_accepted() {
    let space = space_dir();
    let doc = json!({ "op": "lm_status", "params": { "path": space.path_str(), "ignored": true } }).to_string();
    let r = run(&testtool(), &["--input-json", &doc, "--dryrun"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json()["workdir"], space.path_str());
}

#[test]
fn run_cli_7_checkout_list_is_not_list_mode() {
    let r = run(&testtool(), &["checkout", "list", "--dryrun"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let j = r.json();
    assert_eq!(j["success"], true);
    assert_eq!(j["dryRun"], true);
}

#[test]
fn run_cli_8_log_max_count_receives_number() {
    let r = run(&testtool(), &["log", "--max-count", "2"]);
    assert_eq!(r.code, 0);
    let j = r.json();
    let commits = j["commits"].as_array().expect("commits array");
    assert!(!commits.is_empty());
    assert_eq!(commits[0]["author"], "Test User");
    assert_eq!(commits[0]["count"], 2);
}

#[test]
fn run_cli_9_schema_publishes_valid_document_with_version_and_globals() {
    let r = run(&testtool(), &["--schema"]);
    assert_eq!(r.code, 0);
    let j = r.json();
    assert_eq!(j["schema"], "cli.schema.v1");
    assert_eq!(j["version"], TESTTOOL_VERSION);
    assert!(!j["globals"].as_array().expect("globals array").is_empty());
    assert!(parse_published_schema(&j).is_ok());
}

#[test]
fn run_cli_10_help_lists_every_op() {
    let r = run(&testtool(), &["--help"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("testtool — structured CLI wrapper"), "{}", r.stdout);
    assert!(r.stdout.contains("Operations (name [access] — description):"));
    for op in ["lm_status", "commit", "checkout", "log"] {
        assert!(r.stdout.contains(op), "{op}");
    }
}

#[test]
fn run_cli_11_s1_rejects_prefixed_keys_in_json_input_as_unknown_param() {
    let doc = json!({ "op": "lm_status", "params": { "--path": "/nonexistent-dir-xyz" } }).to_string();
    let r = run(&testtool(), &["lm_status", "--input-json", &doc]);
    assert_eq!(r.code, 2);
    let j = r.json();
    assert_eq!(j["success"], false);
    assert_eq!(j["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(j["error"]["data"]["issues"][0]["code"], "UNKNOWN_PARAM");
    assert_eq!(j["error"]["data"]["issues"][0]["param"], "--path");
}

#[test]
fn run_cli_12_s2_globals_before_op_name_work() {
    let space = space_dir();
    let tool = testtool();
    let r_ro = run(&tool, &["--readonly", "lm_status", "--path", &space.path_str()]);
    assert_eq!(r_ro.code, 0);
    assert_eq!(r_ro.json()["clean"], true);

    let r_dry = run(&tool, &["--dryrun", "commit", "--message", "test commit"]);
    assert_eq!(r_dry.code, 0);
    let j = r_dry.json();
    assert_eq!(j["success"], true);
    assert_eq!(j["dryRun"], true);
}

#[test]
fn run_cli_13_n4_unrelated_flag_parse_failure_with_input_json_is_validation_error() {
    let space = space_dir();
    let doc = json!({ "op": "lm_status", "params": { "path": space.path_str() } }).to_string();
    let r = run(&testtool(), &["lm_status", "--input-json", &doc, "--dryrun=maybe"]);
    assert_eq!(r.code, 2);
    let j = r.json();
    assert_eq!(j["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(j["error"]["data"]["issues"][0]["code"], "TYPE_MISMATCH");
    assert_ne!(j["error"]["data"]["issues"][0]["code"], "MIXED_INPUT");
}
