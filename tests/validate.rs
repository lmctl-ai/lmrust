//! Ports of lmts `tests/validate.test.ts` (aggregating `validate()` and the throwing
//! `validateParams()` wrapper) and `tests/schema.test.ts` (the three input forms against
//! one schema).

use lmrust::js::deep_eq;
use lmrust::{
    parse_argv_form, parse_graphql_input, resolve_input, validate, validate_params, InputForm, IssueCode,
    OperationSchema, Record, ResolveArgs, ValidationIssue, ValidationOptions,
};
use serde_json::{json, Value};

fn op(v: Value) -> OperationSchema {
    serde_json::from_value(v).expect("valid operation schema")
}

fn rec(v: Value) -> Record {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn run(schema: &OperationSchema, raw: Value) -> Result<Record, Vec<ValidationIssue>> {
    validate(schema, &rec(raw), ValidationOptions::default())
}

fn run_form(schema: &OperationSchema, raw: Value, form: InputForm) -> Result<Record, Vec<ValidationIssue>> {
    validate(schema, &rec(raw), ValidationOptions::form(form))
}

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// validate.test.ts — describe('validate: T3b aggregating validator')
// ---------------------------------------------------------------------------

// 1. two bad params -> two issues
#[test]
fn two_bad_params_two_issues() {
    let schema = op(json!({
        "name": "two_bad_demo",
        "params": [
            {"name": "count", "type": "number", "occurs": "?"},
            {"name": "flag", "type": "boolean", "occurs": "?"},
        ],
    }));
    let issues = run(&schema, json!({"count": "not-a-number", "flag": "not-a-bool"})).unwrap_err();
    assert_eq!(issues.len(), 2);
    let codes: Vec<IssueCode> = issues.iter().map(|i| i.code).collect();
    assert_eq!(codes, vec![IssueCode::TypeMismatch, IssueCode::TypeMismatch]);
    let params: Vec<Option<&str>> = issues.iter().map(|i| i.param.as_deref()).collect();
    assert_eq!(params, vec![Some("count"), Some("flag")]);
}

// 2. suggestion (--pth -> path)
#[test]
fn suggestion_pth_to_path() {
    let schema = op(json!({
        "name": "suggest_demo",
        "params": [
            {"name": "path", "type": "string", "occurs": "?"},
            {"name": "verbose", "type": "boolean", "occurs": "?"},
        ],
    }));
    let issues = run(&schema, json!({"--pth": "/tmp"})).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::UnknownParam);
    assert_eq!(issues[0].suggestion.as_deref(), Some("path"));

    let issues = run(&schema, json!({"pth": "/tmp"})).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::UnknownParam);
    assert_eq!(issues[0].suggestion.as_deref(), Some("path"));
}

// 3. numeric enum violation
#[test]
fn numeric_enum_violation() {
    let schema = op(json!({
        "name": "num_enum_demo",
        "params": [{"name": "level", "type": "number", "enum": [1, 2, 3]}],
    }));
    let issues = run(&schema, json!({"level": 5})).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::EnumViolation);
    assert_eq!(issues[0].allowed, Some(vec![json!(1), json!(2), json!(3)]));
    assert!(deep_eq(issues[0].received.as_ref().unwrap(), &json!(5)));
}

// 4. '+' empty -> MISSING_REQUIRED
#[test]
fn plus_with_empty_array_missing_required() {
    let schema = op(json!({
        "name": "plus_empty_demo",
        "params": [{"name": "items", "type": "string", "occurs": "+"}],
    }));
    let issues = run(&schema, json!({"items": []})).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::MissingRequired);
    assert_eq!(issues[0].param.as_deref(), Some("items"));
}

// 5. '*' accepts empty
#[test]
fn star_accepts_empty_array() {
    let schema = op(json!({
        "name": "star_empty_demo",
        "params": [{"name": "tags", "type": "string", "occurs": "*"}],
    }));
    let out = run(&schema, json!({"tags": []})).unwrap();
    assert_eq!(out["tags"], json!([]));
}

// 6. '?' null treated absent
#[test]
fn optional_null_treated_absent() {
    let schema = op(json!({
        "name": "opt_null_demo",
        "params": [{"name": "maybe", "type": "string", "occurs": "?"}],
    }));
    let out = run(&schema, json!({"maybe": null})).unwrap();
    assert!(!out.contains_key("maybe"));
}

// 7. required null mismatch -> TYPE_MISMATCH
#[test]
fn required_null_mismatch_type_mismatch() {
    let schema = op(json!({
        "name": "req_null_demo",
        "params": [{"name": "mustHave", "type": "string", "occurs": "1"}],
    }));
    let issues = run(&schema, json!({"mustHave": null})).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::TypeMismatch);
    assert_eq!(issues[0].received, Some(Value::Null));
    assert_eq!(issues[0].param.as_deref(), Some("mustHave"));
}

// 8. cliOnly/docOnly FORM_NOT_ALLOWED
#[test]
fn cli_only_doc_only_form_not_allowed_when_form_is_specified() {
    let schema = op(json!({
        "name": "form_constraint_demo",
        "params": [
            {"name": "cliParam", "type": "string", "occurs": "?", "cliOnly": true},
            {"name": "docParam", "type": "string", "occurs": "?", "docOnly": true},
        ],
    }));

    let issues = run_form(&schema, json!({"cliParam": "hello"}), InputForm::Json).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::FormNotAllowed);
    assert_eq!(issues[0].param.as_deref(), Some("cliParam"));

    let issues = run_form(&schema, json!({"cliParam": "hello"}), InputForm::Graphql).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::FormNotAllowed);
    assert_eq!(issues[0].param.as_deref(), Some("cliParam"));

    let issues = run_form(&schema, json!({"docParam": "hello"}), InputForm::Flags).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::FormNotAllowed);
    assert_eq!(issues[0].param.as_deref(), Some("docParam"));

    // Allowed forms pass without issue
    assert!(run_form(&schema, json!({"cliParam": "hello"}), InputForm::Flags).is_ok());
    assert!(run_form(&schema, json!({"docParam": "hello"}), InputForm::Json).is_ok());
}

// 9. every issue has non-empty synopsis and usage
#[test]
fn every_issue_has_non_empty_synopsis_and_usage() {
    let schema = op(json!({
        "name": "multi_issues_demo",
        "params": [
            {"name": "file", "type": "string", "occurs": "1"},
            {"name": "limit", "type": "number", "occurs": "?"},
        ],
    }));
    let issues = run(&schema, json!({"limit": "bad", "bogus": true})).unwrap_err();
    assert!(issues.len() >= 3, "unknown bogus, bad limit, missing file: {issues:?}");
    for issue in &issues {
        assert!(!issue.code.as_str().is_empty());
        assert!(!issue.message.is_empty());
        assert!(!issue.synopsis.is_empty());
        assert!(!issue.usage.is_empty());
    }
}

// 10. defaults applied
#[test]
fn defaults_applied_when_parameter_omitted() {
    let schema = op(json!({
        "name": "defaults_demo",
        "params": [
            {"name": "host", "type": "string", "occurs": "?", "default": "localhost"},
            {"name": "port", "type": "number", "occurs": "?", "default": 8080},
        ],
    }));
    let out = run(&schema, json!({})).unwrap();
    assert!(deep_eq(
        &Value::Object(out),
        &json!({"host": "localhost", "port": 8080})
    ));
}

// 11. additionalParams allows unknown
#[test]
fn additional_params_allows_unknown_keys() {
    let schema = op(json!({
        "name": "additional_params_demo",
        "additionalParams": true,
        "params": [{"name": "known", "type": "string", "occurs": "?"}],
    }));
    let out = run(
        &schema,
        json!({"known": "explicit", "customA": 42, "customB": {"nested": true}}),
    )
    .unwrap();
    assert_eq!(out["known"], json!("explicit"));
    assert!(deep_eq(&out["customA"], &json!(42)));
    assert_eq!(out["customB"], json!({"nested": true}));
}

// 12. wrapper still throws with details.synopsis
#[test]
fn validate_params_wrapper_throws_with_details_synopsis_and_issues() {
    let schema = op(json!({
        "name": "wrapper_demo",
        "params": [{"name": "file", "type": "string", "occurs": "1"}],
    }));
    let err = validate_params(&schema, &Record::new()).unwrap_err();
    assert_eq!(err.operation, "wrapper_demo");
    assert_eq!(err.details.get("synopsis"), Some(&json!("--file <string>")));
    let issues = err
        .details
        .get("issues")
        .and_then(Value::as_array)
        .expect("issues array");
    assert!(!issues.is_empty());
    assert!(!err.issues().is_empty());
}

// 13. S1: reject --prefixed keys in validate()
#[test]
fn s1_validate_rejects_double_dash_prefixed_keys_as_unknown_param() {
    let schema = op(json!({
        "name": "prefix_demo",
        "params": [{"name": "path", "type": "string", "occurs": "?"}],
    }));
    let issues = run(&schema, json!({"--path": "/nonexistent-dir-xyz"})).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::UnknownParam);
    assert_eq!(issues[0].param.as_deref(), Some("--path"));
}

// 14. S6: number coercion requires at least one digit
#[test]
fn s6_number_coercion_rejects_empty_dash_dot_as_type_mismatch() {
    let schema = op(json!({
        "name": "number_coercion_demo",
        "params": [{"name": "count", "type": "number", "occurs": "?"}],
    }));
    for invalid in ["", "-", ".", "-."] {
        let issues = run(&schema, json!({"count": invalid})).unwrap_err();
        assert_eq!(issues[0].code, IssueCode::TypeMismatch, "{invalid:?}");
        assert_eq!(issues[0].param.as_deref(), Some("count"));
    }

    // Valid numbers with at least one digit succeed
    for valid in ["0", "42", "-42", "3.14", ".5", "-.5", "1e5", "-1.2e-3"] {
        let out = run(&schema, json!({"count": valid})).unwrap_or_else(|e| panic!("{valid}: {e:?}"));
        let n = out["count"].as_f64().unwrap_or_else(|| panic!("{valid} not a number"));
        assert!(n.is_finite());
        assert_eq!(n, valid.parse::<f64>().unwrap(), "{valid}");
    }
}

// 15. validate rejects schema with reserved operation name
#[test]
fn validate_rejects_schema_with_reserved_operation_name() {
    for reserved in ["__proto__", "constructor", "prototype"] {
        let schema = op(json!({
            "name": reserved,
            "params": [{"name": "arg", "type": "string", "occurs": "?"}],
        }));
        let issues = run(&schema, json!({"arg": "val"})).unwrap_err();
        assert!(issues.iter().any(|i| i.code == IssueCode::ReservedName), "{reserved}");
    }
}

// 16. validate rejects schema with reserved parameter name or alias
#[test]
fn validate_rejects_schema_with_reserved_parameter_name_or_alias() {
    for reserved in ["__proto__", "constructor", "prototype"] {
        let schema_param = op(json!({
            "name": "test_op",
            "params": [{"name": reserved, "type": "string", "occurs": "?"}],
        }));
        let issues = run(&schema_param, json!({})).unwrap_err();
        assert!(issues
            .iter()
            .any(|i| i.code == IssueCode::ReservedName && i.param.as_deref() == Some(reserved)));

        let schema_alias = op(json!({
            "name": "test_op",
            "params": [{"name": "okParam", "type": "string", "occurs": "?", "aliases": [reserved]}],
        }));
        let issues = run(&schema_alias, json!({})).unwrap_err();
        assert!(issues
            .iter()
            .any(|i| i.code == IssueCode::ReservedName && i.param.as_deref() == Some(reserved)));
    }
}

// 17. validate returns prototype-less record. The prototype check is JS-specific; the
// values (explicit and defaulted) are still checked.
#[test]
fn validate_returns_prototype_less_record() {
    let schema = op(json!({
        "name": "proto_demo",
        "params": [
            {"name": "name", "type": "string", "occurs": "?"},
            {"name": "active", "type": "boolean", "occurs": "?", "default": true},
        ],
    }));
    let out = run(&schema, json!({"name": "alice"})).unwrap();
    assert_eq!(out["name"], json!("alice"));
    assert_eq!(out["active"], json!(true));
}

// 18. Hostile input with __proto__, constructor, prototype is rejected as UNKNOWN_PARAM.
#[test]
fn hostile_input_proto_constructor_prototype_rejected_as_unknown_param() {
    let schema = op(json!({
        "name": "safe_demo",
        "params": [{"name": "target", "type": "string", "occurs": "?"}],
    }));
    let hostile: Value = serde_json::from_str(
        r#"{"__proto__": {"polluted": true}, "constructor": {"prototype": {"polluted": true}}, "prototype": {"polluted": true}}"#,
    )
    .unwrap();
    let issues = run(&schema, hostile).unwrap_err();
    for key in ["__proto__", "constructor", "prototype"] {
        assert!(
            issues
                .iter()
                .any(|i| i.code == IssueCode::UnknownParam && i.param.as_deref() == Some(key)),
            "{key}: {issues:?}"
        );
    }
}

// 19. additionalParams: true rejects hostile reserved keys
#[test]
fn additional_params_true_rejects_proto_constructor_prototype() {
    let schema = op(json!({
        "name": "add_params_demo",
        "params": [{"name": "path", "type": "string", "occurs": "?"}],
        "additionalParams": true,
    }));
    let hostile: Value = serde_json::from_str(
        r#"{"__proto__": "danger", "constructor": "danger", "prototype": "danger", "custom": "ok"}"#,
    )
    .unwrap();
    let issues = run(&schema, hostile).unwrap_err();
    let names: Vec<&str> = issues.iter().filter_map(|i| i.param.as_deref()).collect();
    assert!(names.contains(&"__proto__"));
    assert!(names.contains(&"constructor"));
    assert!(names.contains(&"prototype"));
    assert!(!names.contains(&"custom"));
}

// ---------------------------------------------------------------------------
// schema.test.ts
// ---------------------------------------------------------------------------

fn status_schema() -> OperationSchema {
    op(json!({
        "name": "lm_status",
        "access": "read",
        "description": "Structured git status",
        "params": [
            {"name": "path", "type": "string", "occurs": "?", "description": "Working dir", "default": "."},
            {"name": "ignored", "type": "boolean", "occurs": "?", "description": "Include ignored files"},
            {"name": "maxFiles", "type": "number", "occurs": "?", "description": "Cap"},
            {"name": "paths", "type": "string", "occurs": "*", "description": "Limit to paths"},
            {"name": "query", "type": "string", "occurs": "?", "description": "JMESPath-ish filter"},
            {"name": "config", "type": "object", "occurs": "?", "description": "Nested config blob"},
            {"name": "state", "type": "string", "occurs": "?", "enum": ["open", "closed", "all"]},
            {"name": "repo", "type": "string", "occurs": "?", "positional": true, "description": "Positional repo arg"},
        ],
    }))
}

fn resolve_flags(
    schema: &OperationSchema,
    tokens: &[&str],
) -> Result<lmrust::ResolvedInput, lmrust::SchemaValidationError> {
    let a = argv(tokens);
    resolve_input(ResolveArgs::new(schema, &a))
}

fn resolve_json(schema: &OperationSchema, doc: &str) -> Result<lmrust::ResolvedInput, lmrust::SchemaValidationError> {
    let mut args = ResolveArgs::new(schema, &[]);
    args.json_document = Some(doc);
    resolve_input(args)
}

fn resolve_graphql(
    schema: &OperationSchema,
    doc: &str,
) -> Result<lmrust::ResolvedInput, lmrust::SchemaValidationError> {
    let mut args = ResolveArgs::new(schema, &[]);
    args.graphql_document = Some(doc);
    resolve_input(args)
}

// describe('validateParams')
#[test]
fn validate_params_applies_defaults_and_coerces_cli_strings() {
    let out = validate_params(
        &status_schema(),
        &rec(json!({
            "ignored": "true",
            "maxFiles": "50",
            "paths": "[\"a.txt\",\"b.txt\"]",
            "query": "Users[].Name",
            "config": "{\"debug\":true}",
        })),
    )
    .unwrap();
    assert_eq!(out["path"], json!("."));
    assert_eq!(out["ignored"], json!(true));
    assert!(deep_eq(&out["maxFiles"], &json!(50)));
    assert_eq!(out["paths"], json!(["a.txt", "b.txt"]));
    assert_eq!(out["config"], json!({"debug": true}));
}

#[test]
fn validate_params_enum_violation_reports_the_allowed_set() {
    let err = validate_params(&status_schema(), &rec(json!({"state": "purple"}))).unwrap_err();
    let issues = err.issues();
    assert_eq!(issues[0].code, IssueCode::EnumViolation);
    assert_eq!(
        issues[0].allowed,
        Some(vec![json!("open"), json!("closed"), json!("all")])
    );
}

#[test]
fn validate_params_missing_required_param() {
    let schema = op(json!({"name": "lm_blame", "params": [{"name": "file", "type": "string"}]}));
    let err = validate_params(&schema, &Record::new()).unwrap_err();
    assert!(err.message.contains("missing required param: file"), "{}", err.message);
}

#[test]
fn validate_params_unknown_params_are_rejected() {
    let err = validate_params(&status_schema(), &rec(json!({"bogus": 1}))).unwrap_err();
    assert!(err.message.contains("unknown param: bogus"), "{}", err.message);
}

#[test]
fn validate_params_array_accepts_single_string_as_one_element_array() {
    let out = validate_params(&status_schema(), &rec(json!({"paths": "a.txt"}))).unwrap();
    assert_eq!(out["paths"], json!(["a.txt"]));
}

// describe('CLI argv form (array-based)')
#[test]
fn argv_parses_flags_values_positionals_repeated_array_flags() {
    let a = argv(&[
        "/tmp/repo",
        "--path",
        "/tmp/repo",
        "--paths",
        "a.txt",
        "--paths",
        "b.txt",
        "--max-files",
        "50",
        "--ignored",
        "--query",
        "Users[].Name",
    ]);
    let r = parse_argv_form(&a, &status_schema(), &[]).unwrap_or_else(|i| panic!("{i:?}"));
    assert!(deep_eq(
        &Value::Object(r.params),
        &json!({
            "repo": "/tmp/repo",
            "path": "/tmp/repo",
            "paths": ["a.txt", "b.txt"],
            "maxFiles": 50,
            "ignored": true,
            "query": "Users[].Name",
        })
    ));
}

#[test]
fn argv_mixed_positional_flag_json_number_works_in_sequence() {
    let a = argv(&["main", "--ignored", "--max-files=3", "--config", "{\"debug\":true}"]);
    let r = parse_argv_form(&a, &status_schema(), &[]).unwrap_or_else(|i| panic!("{i:?}"));
    assert!(deep_eq(
        &Value::Object(r.params),
        &json!({"repo": "main", "ignored": true, "maxFiles": 3, "config": {"debug": true}})
    ));
}

#[test]
fn argv_stray_tokens_beyond_positional_slots_are_extra_positional() {
    let a = argv(&["one", "=bad"]);
    let issues = parse_argv_form(&a, &status_schema(), &[]).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::ExtraPositional);
}

#[test]
fn argv_flag_values_are_kept_verbatim() {
    let a = argv(&["--path", "/tmp", "--query", "\"Users[].Name\""]);
    let r = parse_argv_form(&a, &status_schema(), &[]).unwrap_or_else(|i| panic!("{i:?}"));
    assert_eq!(
        Value::Object(r.params),
        json!({"path": "/tmp", "query": "\"Users[].Name\""})
    );
}

// describe('resolveInput: flags form')
#[test]
fn resolve_flags_positional_binding_binds_schema_declared_positional_param() {
    let r = resolve_flags(&status_schema(), &["/tmp/lmdemo", "--ignored", "--max-files", "10"]).unwrap();
    assert_eq!(r.kind, InputForm::Flags);
    assert_eq!(r.params["repo"], json!("/tmp/lmdemo"));
    assert_eq!(r.params["ignored"], json!(true));
    assert!(deep_eq(&r.params["maxFiles"], &json!(10)));
    assert_eq!(r.params["path"], json!("."));
}

#[test]
fn resolve_flags_form_validates_enums() {
    let err = resolve_flags(&status_schema(), &["--state", "purple"]).unwrap_err();
    assert_eq!(err.issues()[0].code, IssueCode::EnumViolation);
}

// describe('resolveInput: json form')
#[test]
fn resolve_json_envelope_with_matching_op_validates() {
    let doc = json!({
        "op": "lm_status",
        "params": {"path": "/x", "ignored": true, "paths": ["a"], "state": "open"},
    })
    .to_string();
    let r = resolve_json(&status_schema(), &doc).unwrap();
    assert_eq!(r.kind, InputForm::Json);
    assert_eq!(r.params["path"], json!("/x"));
    assert_eq!(r.params["ignored"], json!(true));
    assert_eq!(r.params["state"], json!("open"));
}

#[test]
fn resolve_json_reject_op_mismatch() {
    let doc = json!({"op": "other_op", "params": {"path": "/x"}}).to_string();
    let err = resolve_json(&status_schema(), &doc).unwrap_err();
    assert!(err.message.contains("does not match invoked op"), "{}", err.message);
}

#[test]
fn resolve_json_bare_params_form_works() {
    let r = resolve_json(&status_schema(), &json!({"ignored": false}).to_string()).unwrap();
    assert_eq!(r.kind, InputForm::Json);
    assert_eq!(r.params["ignored"], json!(false));
}

#[test]
fn resolve_json_cannot_mix_input_json_with_flags() {
    let schema = status_schema();
    let a = argv(&["--path", "/x"]);
    let mut args = ResolveArgs::new(&schema, &a);
    args.json_document = Some("{\"params\":{\"path\":\"/y\"}}");
    let err = resolve_input(args).unwrap_err();
    assert!(err.message.contains("cannot mix"), "{}", err.message);
}

// describe('resolveInput: graphql form')
#[test]
fn resolve_graphql_simple_query() {
    let r = resolve_graphql(
        &status_schema(),
        "query { lm_status(path: \"/r\", ignored: true, maxFiles: 7) { __typename } }",
    )
    .unwrap();
    assert_eq!(r.kind, InputForm::Graphql);
    assert_eq!(r.params["path"], json!("/r"));
    assert_eq!(r.params["ignored"], json!(true));
    assert!(deep_eq(&r.params["maxFiles"], &json!(7)));
}

#[test]
fn resolve_graphql_args_with_arrays_and_objects() {
    let r = resolve_graphql(
        &status_schema(),
        "{ lm_status(paths: [\"a.txt\", \"b.txt\"], config: {debug: true, level: 2}) { __typename } }",
    )
    .unwrap();
    assert_eq!(r.params["paths"], json!(["a.txt", "b.txt"]));
    assert!(deep_eq(&r.params["config"], &json!({"debug": true, "level": 2})));
}

#[test]
fn resolve_graphql_selection_without_explicit_query_keyword() {
    let r = resolve_graphql(&status_schema(), "{ lm_status { __typename } }").unwrap();
    assert_eq!(r.params["path"], json!("."));
}

#[test]
fn resolve_graphql_reject_extra_content_after_selection_set() {
    let err = parse_graphql_input("{ lm_status { __typename } } trailing").unwrap_err();
    assert!(err.message.contains("trailing content"), "{}", err.message);
}

#[test]
fn resolve_graphql_reject_schema_mismatch_via_resolver() {
    let err = resolve_graphql(&status_schema(), "{ lm_other { __typename } }").unwrap_err();
    assert!(err.message.contains("does not match invoked op"), "{}", err.message);
}

// describe('resolveInput: flags errors')
#[test]
fn resolve_flags_argv_problems_surface_with_issues_and_synopsis() {
    let err = resolve_flags(&status_schema(), &["--bogus", "--max-files", "ten"]).unwrap_err();
    let codes: Vec<IssueCode> = err.issues().iter().map(|i| i.code).collect();
    assert_eq!(codes, vec![IssueCode::UnknownParam, IssueCode::TypeMismatch]);
    assert!(err.details.get("synopsis").is_some_and(Value::is_string));
}

#[test]
fn resolve_flags_value_with_spaces_is_not_split_b1() {
    let r = resolve_flags(&status_schema(), &["--path", "/tmp/my repo"]).unwrap();
    assert_eq!(r.params["path"], json!("/tmp/my repo"));
}
