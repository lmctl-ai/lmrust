//! Port of lmts `tests/argv.test.ts` (array-based argv form, B1–B8 regressions and rules)
//! and `tests/inputs-argv-edge.test.ts` (resolve_input, argv-form, tokenizer and
//! describe_for_prompt edge cases).

use lmrust::{
    describe_for_prompt, parse_argv_form, resolve_input, tokenize_simple_command, ArgvFormValue, IssueCode,
    OperationSchema, Outcome, ParamSpec, PublishedCliSchema, ResolveArgs, ValidationIssue,
};
use serde_json::{json, Value};

fn op(v: Value) -> OperationSchema {
    serde_json::from_value(v).expect("valid OperationSchema literal")
}

fn specs(v: Value) -> Vec<ParamSpec> {
    serde_json::from_value(v).expect("valid ParamSpec list literal")
}

fn schema() -> OperationSchema {
    op(json!({
        "name": "lm_commit",
        "params": [
            { "name": "repo", "type": "string", "positional": true, "occurs": "?" },
            { "name": "path", "type": "string", "occurs": "?" },
            { "name": "message", "type": "string", "occurs": "?" },
            { "name": "rev", "type": "string", "occurs": "?" },
            { "name": "limit", "type": "number", "occurs": "?" },
            { "name": "wip", "type": "boolean", "occurs": "?" },
            { "name": "ignored", "type": "boolean", "occurs": "?" },
            { "name": "config", "type": "object", "occurs": "?" },
            { "name": "paths", "type": "array", "items": "string", "occurs": "*" },
            { "name": "tag", "type": "string", "occurs": "+" },
            { "name": "dryRunLocal", "type": "boolean", "occurs": "?", "aliases": ["dr"] }
        ]
    }))
}

fn globals() -> Vec<ParamSpec> {
    specs(json!([
        { "name": "dryrun", "type": "boolean", "occurs": "?", "aliases": ["dry-run"] },
        { "name": "format", "type": "string", "occurs": "?" },
        { "name": "help", "type": "boolean", "occurs": "?", "aliases": ["h"] }
    ]))
}

fn argv(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|s| s.to_string()).collect()
}

fn parse_with(tokens: &[&str], schema: &OperationSchema) -> Outcome<ArgvFormValue> {
    parse_argv_form(&argv(tokens), schema, &globals())
}

fn parse(tokens: &[&str]) -> Outcome<ArgvFormValue> {
    parse_with(tokens, &schema())
}

fn ok<T: std::fmt::Debug>(r: Outcome<T>) -> T {
    match r {
        Ok(v) => v,
        Err(issues) => panic!("expected ok, got {issues:#?}"),
    }
}

fn fail<T: std::fmt::Debug>(r: Outcome<T>) -> Vec<ValidationIssue> {
    match r {
        Ok(v) => panic!("expected failure, got {v:?}"),
        Err(issues) => issues,
    }
}

fn params(tokens: &[&str]) -> Value {
    Value::Object(ok(parse(tokens)).params)
}

// ---------------------------------------------------------------------------
// parseArgvForm: values are taken verbatim
// ---------------------------------------------------------------------------

#[test]
fn b1_space_in_value() {
    assert_eq!(params(&["--path", "/tmp/my repo"]), json!({ "path": "/tmp/my repo" }));
}

#[test]
fn b2_eq_in_value_separate_and_inline() {
    assert_eq!(params(&["--message", "a=b"])["message"], json!("a=b"));
    assert_eq!(params(&["--message=a=b=c"])["message"], json!("a=b=c"));
}

#[test]
fn b3_nested_json_object() {
    assert_eq!(
        params(&["--config", "{\"a\":{\"b\":1}}"])["config"],
        json!({ "a": { "b": 1 } })
    );
}

#[test]
fn b3_bad_json_is_type_mismatch_with_argv_index_of_the_value() {
    let issues = fail(parse(&["--config", "{\"a\":"]));
    let i = &issues[0];
    assert_eq!(i.code, IssueCode::TypeMismatch);
    assert_eq!(i.param.as_deref(), Some("config"));
    assert_eq!(i.argv_index, Some(1));
}

#[test]
fn b4_string_param_keeps_value() {
    for v in ["trueish", "123abc", "1.2.3", "2024-01-01", "false", "true"] {
        assert_eq!(params(&["--rev", v])["rev"], json!(v), "value {v:?}");
    }
}

#[test]
fn b5_message_false_is_string_wip_false_is_boolean() {
    assert_eq!(params(&["--message", "false"])["message"], json!("false"));
    assert_eq!(params(&["--wip", "false"])["wip"], json!(false));
    assert_eq!(params(&["--wip=true"])["wip"], json!(true));
}

#[test]
fn value_may_start_with_dash_via_inline_and_as_next_token() {
    assert_eq!(params(&["--message=--not-a-flag"])["message"], json!("--not-a-flag"));
    assert_eq!(params(&["--message", "-x"])["message"], json!("-x"));
    assert_eq!(params(&["--limit", "-5"])["limit"], json!(-5));
}

#[test]
fn number_param_parses_and_rejects_nan() {
    assert_eq!(params(&["--limit", "1e3"])["limit"], json!(1000));
    let issues = fail(parse(&["--limit", "ten"]));
    assert_eq!(issues[0].code, IssueCode::TypeMismatch);
}

#[test]
fn boolean_param_rejects_non_boolean_inline_value() {
    assert_eq!(fail(parse(&["--wip=yes"]))[0].code, IssueCode::TypeMismatch);
}

#[test]
fn boolean_flag_followed_by_non_boolean_word_leaves_it_positional() {
    let v = ok(parse(&["--wip", "myrepo"]));
    assert_eq!(Value::Object(v.params), json!({ "wip": true, "repo": "myrepo" }));
}

// ---------------------------------------------------------------------------
// parseArgvForm: cardinality
// ---------------------------------------------------------------------------

#[test]
fn b6_repeated_flag_accumulates_for_array_and_occurs_plus() {
    let p = params(&["--paths", "a", "--paths", "b", "--tag", "x", "--tag=y"]);
    assert_eq!(p["paths"], json!(["a", "b"]));
    assert_eq!(p["tag"], json!(["x", "y"]));
}

#[test]
fn single_occurrence_of_a_multi_param_is_still_an_array() {
    assert_eq!(params(&["--tag", "x"])["tag"], json!(["x"]));
}

#[test]
fn b6_repeated_scalar_flag_is_duplicate_flag() {
    let issues = fail(parse(&["--path", "a", "--path", "b"]));
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].code, IssueCode::DuplicateFlag);
    assert_eq!(issues[0].argv_index, Some(2));
}

// ---------------------------------------------------------------------------
// parseArgvForm: flag syntax
// ---------------------------------------------------------------------------

#[test]
fn b8_message_then_flag_is_missing_value() {
    let issues = fail(parse(&["--message", "--wip"]));
    let i = &issues[0];
    assert_eq!(i.code, IssueCode::MissingValue);
    assert_eq!(i.param.as_deref(), Some("message"));
    assert_eq!(i.argv_index, Some(0));
}

#[test]
fn trailing_non_boolean_flag_is_missing_value() {
    assert_eq!(fail(parse(&["--message"]))[0].code, IssueCode::MissingValue);
}

#[test]
fn double_dash_terminates_flag_parsing() {
    let v = ok(parse(&["--wip", "--", "--message"]));
    assert_eq!(Value::Object(v.params), json!({ "wip": true, "repo": "--message" }));
    assert_eq!(v.positionals, vec!["--message".to_string()]);
}

#[test]
fn no_flag_sets_false() {
    assert_eq!(params(&["--no-ignored"])["ignored"], json!(false));
}

#[test]
fn kebab_case_maps_to_camel_case_aliases_honored() {
    assert_eq!(params(&["--dry-run-local"])["dryRunLocal"], json!(true));
    assert_eq!(params(&["--dr"])["dryRunLocal"], json!(true));
}

#[test]
fn unknown_param_with_suggestion_all_issues_collected() {
    let issues = fail(parse(&["--mesage", "hi", "--limit", "x", "--bogus"]));
    let codes: Vec<IssueCode> = issues.iter().map(|i| i.code).collect();
    assert_eq!(
        codes,
        vec![
            IssueCode::UnknownParam,
            IssueCode::TypeMismatch,
            IssueCode::UnknownParam
        ]
    );
    assert_eq!(issues[0].suggestion.as_deref(), Some("--message"));
    assert_eq!(issues[0].argv_index, Some(0));
    assert_eq!(issues[2].argv_index, Some(4));
}

// ---------------------------------------------------------------------------
// parseArgvForm: positionals and globals
// ---------------------------------------------------------------------------

#[test]
fn positional_binds_in_order() {
    let v = ok(parse(&["myrepo", "--wip"]));
    assert_eq!(Value::Object(v.params), json!({ "repo": "myrepo", "wip": true }));
    assert_eq!(v.positionals, vec!["myrepo".to_string()]);
}

#[test]
fn positional_slot_already_set_by_flag_is_skipped_extra_positional() {
    let issues = fail(parse(&["--repo", "a", "b"]));
    assert_eq!(issues[0].code, IssueCode::ExtraPositional);
    assert_eq!(issues[0].argv_index, Some(2));
}

#[test]
fn extra_positional_for_surplus_positionals() {
    let issues = fail(parse(&["a", "b"]));
    let i = &issues[0];
    assert_eq!(i.code, IssueCode::ExtraPositional);
    assert_eq!(i.received, Some(json!("b")));
    assert_eq!(i.argv_index, Some(1));
}

#[test]
fn globals_extracted_and_never_unknown() {
    let v = ok(parse(&["--dry-run", "--format", "json", "-h", "--wip"]));
    assert_eq!(
        Value::Object(v.globals),
        json!({ "dryrun": true, "format": "json", "help": true })
    );
    assert_eq!(Value::Object(v.params), json!({ "wip": true }));
}

#[test]
fn additional_params_keeps_unknown_flags_with_loose_coercion() {
    let lenient = op(json!({ "name": "run", "params": [], "additionalParams": true }));
    let v = ok(parse_with(
        &[
            "--a",
            "true",
            "--b=12",
            "--c",
            "{\"x\":[1]}",
            "--d",
            "hello world",
            "--e",
            "--f",
            "x",
            "--f",
            "y",
            "extra",
        ],
        &lenient,
    ));
    assert_eq!(
        Value::Object(v.params),
        json!({ "a": true, "b": 12, "c": { "x": [1] }, "d": "hello world", "e": true, "f": ["x", "y"] })
    );
    assert_eq!(v.positionals, vec!["extra".to_string()]);
}

#[test]
fn json_typed_param_uses_loose_literal_coercion() {
    let s = op(json!({ "name": "x", "params": [{ "name": "v", "type": "json", "occurs": "?" }] }));
    assert_eq!(ok(parse_with(&["--v", "42"], &s)).params["v"], json!(42));
    assert_eq!(ok(parse_with(&["--v", "abc"], &s)).params["v"], json!("abc"));
    assert_eq!(ok(parse_with(&["--v"], &s)).params["v"], json!(true));
}

// ---------------------------------------------------------------------------
// parseArgvForm: issue shape
// ---------------------------------------------------------------------------

#[test]
fn every_issue_has_code_message_synopsis_usage_argv_index() {
    let issues = fail(parse(&[
        "--path",
        "a",
        "--path",
        "b",
        "--message",
        "--zzz",
        "p1",
        "p2",
        "--limit=NaN",
    ]));
    assert!(issues.len() >= 4, "{issues:#?}");
    for i in &issues {
        assert!(!i.code.as_str().is_empty());
        assert!(!i.message.is_empty());
        assert!(!i.synopsis.is_empty());
        assert!(!i.usage.is_empty());
        assert!(i.argv_index.is_some());
    }
    let dup = issues
        .iter()
        .find(|i| i.code == IssueCode::DuplicateFlag)
        .expect("DUPLICATE_FLAG");
    assert_eq!(dup.synopsis, "--path <string> [optional]");
    assert_eq!(dup.usage, "lm_commit [<repo>] [flags]");
}

#[test]
fn n1_unknown_flag_issue_message_mentions_swallowed_following_value() {
    let issues = fail(parse(&["--bogus", "swallowed_value"]));
    let unknown = issues
        .iter()
        .find(|i| i.code == IssueCode::UnknownParam)
        .expect("UNKNOWN_PARAM issue");
    assert!(
        unknown.message.contains("swallowed value 'swallowed_value'"),
        "{}",
        unknown.message
    );
}

// TS: "parseArgvForm returns null-prototype params and globals" checks
// Object.getPrototypeOf(...) === null, which has no Rust equivalent (Record is a plain map).
// The behaviour behind it (plain records holding exactly the parsed keys) is checked here.
#[test]
fn parse_argv_form_returns_plain_params_and_globals() {
    let v = ok(parse(&["--path", "/tmp", "--dryrun"]));
    assert_eq!(Value::Object(v.params), json!({ "path": "/tmp" }));
    assert_eq!(Value::Object(v.globals), json!({ "dryrun": true }));
}

#[test]
fn hostile_flags_proto_constructor_prototype_rejected_as_unknown_param_without_polluting() {
    for flag in ["--__proto__=evil", "--constructor=evil", "--prototype=evil"] {
        let issues = fail(parse(&[flag]));
        assert!(
            issues.iter().any(|i| i.code == IssueCode::UnknownParam),
            "{flag}: {issues:#?}"
        );
    }
    // No pollution: a later, clean parse carries no "evil" keys.
    let v = ok(parse(&["--path", "/tmp"]));
    assert!(!v.params.contains_key("evil"));
    assert!(!v.params.values().any(|x| x == &json!("evil")));
}

#[test]
fn schema_with_additional_params_rejects_proto_and_preserves_prototype_clean() {
    let add_schema = op(json!({ "name": "add_op", "params": [], "additionalParams": true }));
    let issues = fail(parse_argv_form(
        &argv(&["--__proto__=danger", "--constructor=danger", "--prototype=danger"]),
        &add_schema,
        &[],
    ));
    assert!(
        issues
            .iter()
            .any(|i| i.code == IssueCode::UnknownParam && i.param.as_deref() == Some("__proto__")),
        "{issues:#?}"
    );
    let v = ok(parse_argv_form(&argv(&["--x=1"]), &add_schema, &[]));
    assert_eq!(Value::Object(v.params), json!({ "x": 1 }));
}

// ===========================================================================
// inputs-argv-edge.test.ts
// ===========================================================================

fn sync_schema() -> OperationSchema {
    op(json!({
        "name": "sync",
        "access": "write",
        "params": [
            { "name": "remote", "type": "string", "occurs": "1" },
            { "name": "branch", "type": "string", "occurs": "?" }
        ]
    }))
}

#[test]
fn inputs_mixing_input_json_and_input_graphql_throws() {
    let schema = sync_schema();
    let r = resolve_input(ResolveArgs {
        json_document: Some("{\"remote\":\"origin\"}"),
        graphql_document: Some("mutation { sync(remote: \"origin\") { __typename } }"),
        ..ResolveArgs::new(&schema, &[])
    });
    assert!(r.is_err(), "{r:?}");
}

#[test]
fn inputs_document_input_mixed_with_op_flags_throws() {
    let schema = sync_schema();
    let flags = argv(&["--branch", "main"]);
    let r = resolve_input(ResolveArgs {
        json_document: Some("{\"remote\":\"origin\"}"),
        ..ResolveArgs::new(&schema, &flags)
    });
    assert!(r.is_err(), "{r:?}");

    let r = resolve_input(ResolveArgs {
        graphql_document: Some("mutation { sync(remote: \"origin\") { __typename } }"),
        ..ResolveArgs::new(&schema, &flags)
    });
    assert!(r.is_err(), "{r:?}");
}

#[test]
fn inputs_input_document_op_mismatch_throws() {
    let schema = sync_schema();
    // JSON op mismatch
    let doc = json!({ "op": "other_op", "params": { "remote": "origin" } }).to_string();
    let r = resolve_input(ResolveArgs {
        json_document: Some(&doc),
        ..ResolveArgs::new(&schema, &[])
    });
    assert!(r.is_err(), "{r:?}");

    // GraphQL op mismatch
    let r = resolve_input(ResolveArgs {
        graphql_document: Some("mutation { other_op(remote: \"origin\") { __typename } }"),
        ..ResolveArgs::new(&schema, &[])
    });
    assert!(r.is_err(), "{r:?}");
}

// TS "encodeGraphql with unsupported value type throws TypeError" passes a BigInt, which
// has no serde_json::Value counterpart; skipped as JavaScript-specific.

fn config_schema() -> OperationSchema {
    op(json!({
        "name": "config",
        "access": "write",
        "params": [
            { "name": "flag", "type": "boolean", "occurs": "?" },
            { "name": "data", "type": "json", "occurs": "?" },
            { "name": "count", "type": "number", "positional": true, "occurs": "1" }
        ]
    }))
}

#[test]
fn argv_negated_boolean_with_inline_value_is_type_mismatch() {
    let issues = fail(parse_argv_form(&argv(&["--no-flag=true", "42"]), &config_schema(), &[]));
    assert!(
        issues
            .iter()
            .any(|i| i.code == IssueCode::TypeMismatch && i.message.contains("does not take a value")),
        "{issues:#?}"
    );
}

#[test]
fn argv_malformed_json_literal_falls_back_to_raw_string() {
    let v = ok(parse_argv_form(
        &argv(&["--data", "{not valid json}", "42"]),
        &config_schema(),
        &[],
    ));
    assert_eq!(v.params["data"], json!("{not valid json}"));
}

#[test]
fn argv_positional_argument_type_mismatch() {
    let issues = fail(parse_argv_form(&argv(&["not_a_number"]), &config_schema(), &[]));
    assert!(issues.iter().any(|i| i.code == IssueCode::TypeMismatch), "{issues:#?}");
}

fn assert_tokenize_fails_with(line: &str, needle: &str) {
    let issues = fail(tokenize_simple_command(line));
    assert!(
        issues.iter().any(|i| i.message.contains(needle)),
        "{line:?}: no issue contains {needle:?}: {issues:#?}"
    );
}

#[test]
fn interpret_unterminated_escape_in_double_quotes() {
    assert_tokenize_fails_with(
        "git2 commit --message \"hello\\",
        "unterminated escape in double quotes",
    );
}

#[test]
fn interpret_escaped_variable_and_backtick_inside_double_quotes() {
    assert_tokenize_fails_with("git2 commit --message \"\\$FOO\"", "variable expansion");
    assert_tokenize_fails_with("git2 commit --message \"\\`cmd\\`\"", "backtick command substitution");
}

#[test]
fn interpret_literal_preserved_backslash_in_double_quotes_for_non_special_character() {
    let tokens = ok(tokenize_simple_command("git2 commit --message \"\\n\\a\""));
    assert_eq!(tokens[3], "\\n\\a");
}

#[test]
fn interpret_unterminated_backslash_escape_outside_quotes_at_end_of_line() {
    assert_tokenize_fails_with("git2 status \\", "unterminated backslash escape");
}

#[test]
fn interpret_backslash_followed_by_refused_character_outside_quotes() {
    assert_tokenize_fails_with("git2 status \\;", "shell character \";\"");
}

#[test]
fn describe_for_prompt_handles_single_published_cli_schema() {
    let single: PublishedCliSchema = serde_json::from_value(json!({
        "schema": "cli.schema.v1",
        "tool": "solo",
        "policy": { "dryrun": true, "readonly": true },
        "inputs": ["flags"],
        "operations": [
            { "name": "ping", "access": "read", "description": "Ping service", "params": [] }
        ]
    }))
    .unwrap();
    let rendered = describe_for_prompt(std::slice::from_ref(&single));
    assert!(rendered.contains("# solo"), "{rendered}");
    assert!(rendered.contains("solo ping (read) — Ping service"), "{rendered}");
}
