//! Ports of lmts `tests/json.test.ts` (JSON input form, T4) and `tests/graphql.test.ts`
//! (GraphQL input form, T5).

use lmrust::js::deep_eq;
use lmrust::{
    parse_graphql_document, parse_graphql_input, parse_json_document, parse_json_input, resolve_input, validate,
    IssueCode, OperationSchema, Record, ResolveArgs, ValidationOptions,
};
use serde_json::{json, Value};

fn op(v: Value) -> OperationSchema {
    serde_json::from_value(v).expect("valid operation schema")
}

fn obj(r: &Record) -> Value {
    Value::Object(r.clone())
}

const VARS_MSG: &str = "variables and operation names are not supported; inline literal values";

// ---------------------------------------------------------------------------
// json.test.ts — describe('json-form: parseJsonDocument (T4)')
// ---------------------------------------------------------------------------

fn demo_schema() -> OperationSchema {
    op(json!({
        "name": "lm_status",
        "access": "read",
        "params": [
            {"name": "path", "type": "string", "occurs": "?", "default": "."},
            {"name": "ignored", "type": "boolean", "occurs": "?"},
        ],
    }))
}

fn resolve_json(schema: &OperationSchema, doc: &str) -> Result<lmrust::ResolvedInput, lmrust::SchemaValidationError> {
    let mut args = ResolveArgs::new(schema, &[]);
    args.json_document = Some(doc);
    resolve_input(args)
}

// 1. Bare params
#[test]
fn json_bare_params_form() {
    let doc = parse_json_document(r#"{"path": "/tmp", "limit": 10}"#).unwrap();
    assert_eq!(doc.op, None);
    assert!(deep_eq(&obj(&doc.params), &json!({"path": "/tmp", "limit": 10})));
}

// 2. {"params":{...}} without op treated as bare params
#[test]
fn json_params_without_op_treated_as_bare_params() {
    let text = json!({"params": {"Bucket": "my-bucket", "Key": "data.txt"}}).to_string();
    let doc = parse_json_document(&text).unwrap();
    assert_eq!(doc.op, None);
    assert_eq!(
        obj(&doc.params),
        json!({"params": {"Bucket": "my-bucket", "Key": "data.txt"}})
    );
}

// 3. Envelope with op
#[test]
fn json_envelope_with_op() {
    let text = json!({"op": "lm_status", "params": {"path": "/workspace", "ignored": true}}).to_string();
    let doc = parse_json_document(&text).unwrap();
    assert_eq!(doc.op.as_deref(), Some("lm_status"));
    assert_eq!(obj(&doc.params), json!({"path": "/workspace", "ignored": true}));
}

// 4. Envelope with op and missing params defaults to empty object
#[test]
fn json_envelope_with_op_but_no_params_key_defaults_to_empty_object() {
    let doc = parse_json_document(r#"{"op": "lm_status"}"#).unwrap();
    assert_eq!(doc.op.as_deref(), Some("lm_status"));
    assert!(doc.params.is_empty());
}

// 5. Envelope with op and non-object params returns DOCUMENT_PARSE with issue op
#[test]
fn json_envelope_with_non_object_params_returns_document_parse() {
    let issues = parse_json_document(r#"{"op": "lm_status", "params": "string-not-object"}"#).unwrap_err();
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(issue.code, IssueCode::DocumentParse);
    assert_eq!(issue.op.as_deref(), Some("lm_status"));
    assert!(
        issue.message.contains("\"params\" in envelope must be an object"),
        "{}",
        issue.message
    );
    assert!(!issue.synopsis.is_empty());
    assert!(!issue.usage.is_empty());
}

// 6. Op mismatch via resolveInput
#[test]
fn json_op_mismatch_checked_by_resolve_input() {
    let text = json!({"op": "lm_other", "params": {"path": "/tmp"}}).to_string();
    let err = resolve_json(&demo_schema(), &text).unwrap_err();
    assert!(err.message.contains("does not match invoked op"), "{}", err.message);
}

// 7. Invalid JSON -> DOCUMENT_PARSE issue with parse error
#[test]
fn json_invalid_json_document_parse_issue_with_parse_error() {
    let issues = parse_json_document("{ invalid json content:").unwrap_err();
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(issue.code, IssueCode::DocumentParse);
    assert!(issue.message.contains("Invalid JSON"), "{}", issue.message);
    assert!(!issue.synopsis.is_empty());
    assert!(!issue.usage.is_empty());
}

// 8. Array and scalar documents -> DOCUMENT_PARSE
#[test]
fn json_array_or_scalar_documents_document_parse() {
    for input in ["[1, 2, 3]", "\"plain string\"", "42", "null", "true"] {
        let issues = parse_json_document(input).unwrap_err();
        assert_eq!(issues[0].code, IssueCode::DocumentParse, "{input}");
        assert!(
            issues[0].message.contains("JSON input must be an object"),
            "{}",
            issues[0].message
        );
        assert!(!issues[0].synopsis.is_empty());
        assert!(!issues[0].usage.is_empty());
    }
}

// 9. Nested objects and arrays pass through untouched
#[test]
fn json_nested_objects_arrays_pass_through_untouched() {
    let nested = json!({
        "config": {
            "retries": 3,
            "headers": {"X-Custom": "header-val"},
            "tags": ["alpha", "beta", {"id": 1}],
        },
    });
    let doc = parse_json_document(&nested.to_string()).unwrap();
    assert_eq!(obj(&doc.params), nested);
}

// 10. parseJsonInput compat wrapper throws SchemaValidationError with details
#[test]
fn json_parse_json_input_compat_wrapper_throws_on_invalid_input() {
    let err = parse_json_input("bad json").unwrap_err();
    assert!(err.details.get("synopsis").is_some());
    assert!(err.details.get("issues").is_some_and(Value::is_array));
}

// 11. parseJsonDocument returns null-prototype params record: JS-specific (prototype
// check); the Rust records are plain maps. Only the parse succeeding is checked.
#[test]
fn json_parse_json_document_returns_null_prototype_params_record() {
    assert!(parse_json_document(r#"{"path": "/tmp", "limit": 10}"#).is_ok());
    let env = parse_json_document(r#"{"op": "lm_status", "params": {"path": "/tmp"}}"#).unwrap();
    assert_eq!(obj(&env.params), json!({"path": "/tmp"}));
}

// 12. hostile JSON with __proto__ key yields UNKNOWN_PARAM
#[test]
fn json_hostile_proto_key_yields_unknown_param() {
    let text = r#"{"op": "lm_status", "params": {"__proto__": {"polluted": true}, "path": "/safe"}}"#;
    let doc = parse_json_document(text).unwrap();
    // The key is kept as plain data (JS: own property on a null-prototype record).
    assert_eq!(doc.params.get("__proto__"), Some(&json!({"polluted": true})));
    let err = resolve_json(&demo_schema(), text).unwrap_err();
    let issues = err.issues();
    assert!(issues
        .iter()
        .any(|i| i.code == IssueCode::UnknownParam && i.param.as_deref() == Some("__proto__")));
}

// ---------------------------------------------------------------------------
// graphql.test.ts — describe('graphql-form: parseGraphqlDocument (T5)')
// ---------------------------------------------------------------------------

fn status_schema() -> OperationSchema {
    op(json!({
        "name": "lm_status",
        "access": "read",
        "params": [
            {"name": "path", "type": "string", "occurs": "?", "default": "."},
            {"name": "ignored", "type": "boolean", "occurs": "?"},
            {"name": "maxFiles", "type": "number", "occurs": "?"},
            {"name": "state", "type": "string", "occurs": "?", "enum": ["open", "closed"]},
        ],
    }))
}

// 1. Happy path: query, mutation, bare form with arguments and selection set
#[test]
fn graphql_happy_path_parses_query_mutation_and_bare_forms() {
    let q1 = parse_graphql_document("query { lm_status(path: \"/tmp\", ignored: true) { __typename } }").unwrap();
    assert_eq!(q1.op.as_deref(), Some("lm_status"));
    assert_eq!(obj(&q1.params), json!({"path": "/tmp", "ignored": true}));

    let q2 = parse_graphql_document("mutation { lm_commit(message: \"feat: add stuff\") }").unwrap();
    assert_eq!(q2.op.as_deref(), Some("lm_commit"));
    assert_eq!(obj(&q2.params), json!({"message": "feat: add stuff"}));

    let q3 = parse_graphql_document("{ lm_status }").unwrap();
    assert_eq!(q3.op.as_deref(), Some("lm_status"));
    assert!(q3.params.is_empty());
}

// 2. Reject variables ($x)
#[test]
fn graphql_reject_variables_with_clear_document_parse_message() {
    let issues = parse_graphql_document("{ lm_status(path: $myPath) }").unwrap_err();
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(issue.code, IssueCode::DocumentParse);
    assert!(issue.message.contains(VARS_MSG), "{}", issue.message);
    assert_eq!(issue.op.as_deref(), Some("lm_status"));
    assert!(!issue.synopsis.is_empty());
    assert!(!issue.usage.is_empty());

    let issues = parse_graphql_document("query ($var: String) { lm_status(path: $var) }").unwrap_err();
    assert_eq!(issues[0].code, IssueCode::DocumentParse);
    assert!(issues[0].message.contains(VARS_MSG), "{}", issues[0].message);

    let issues = parse_graphql_document("{ lm_status(items: [$var]) }").unwrap_err();
    assert_eq!(issues[0].code, IssueCode::DocumentParse);
    assert!(issues[0].message.contains(VARS_MSG), "{}", issues[0].message);
}

// 3. Reject named operations
#[test]
fn graphql_reject_named_operations_with_document_parse_message() {
    let issues = parse_graphql_document("query GetStatus { lm_status { __typename } }").unwrap_err();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].code, IssueCode::DocumentParse);
    assert!(issues[0].message.contains(VARS_MSG), "{}", issues[0].message);

    let issues = parse_graphql_document("mutation CreateRepo { lm_init }").unwrap_err();
    assert_eq!(issues[0].code, IssueCode::DocumentParse);
    assert!(issues[0].message.contains(VARS_MSG), "{}", issues[0].message);
}

// 4. Reject more than one top-level field
#[test]
fn graphql_reject_more_than_one_top_level_field() {
    let issues = parse_graphql_document(
        r#"
      {
        lm_status { __typename }
        lm_commit(message: "hello") { __typename }
      }
    "#,
    )
    .unwrap_err();
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(issue.code, IssueCode::DocumentParse);
    assert_eq!(issue.op.as_deref(), Some("lm_status"));
    assert!(
        issue.message.contains("more than one top-level field is not supported"),
        "{}",
        issue.message
    );
    assert!(!issue.synopsis.is_empty());
    assert!(!issue.usage.is_empty());
}

// 5. Issue op is set to the parsed field name when known
#[test]
fn graphql_issue_op_is_set_to_parsed_field_name_when_error_happens_after_op_name() {
    let issues = parse_graphql_document("{ lm_status(unclosed: \"val\" }").unwrap_err();
    assert_eq!(issues[0].op.as_deref(), Some("lm_status"));
    assert_eq!(issues[0].code, IssueCode::DocumentParse);
}

// 6. null argument -> key omitted (so defaults apply)
#[test]
fn graphql_null_argument_key_omitted_so_schema_defaults_apply() {
    let doc = parse_graphql_document("{ lm_status(path: null, ignored: true) }").unwrap();
    assert!(!doc.params.contains_key("path"));
    assert_eq!(obj(&doc.params), json!({"ignored": true}));

    let validated = validate(&status_schema(), &doc.params, ValidationOptions::default()).unwrap();
    assert_eq!(validated["path"], json!("."));
    assert_eq!(validated["ignored"], json!(true));
}

// 7. Keep # comments
#[test]
fn graphql_keep_comments_throughout_document() {
    let text = r#"
      # Header comment
      query {
        # Field comment
        lm_status(
          # Arg comment
          path: "/workspace",
          ignored: false # inline comment
        ) {
          # Selection comment
          __typename
        }
      }
      # Trailing comment
    "#;
    let doc = parse_graphql_document(text).unwrap();
    assert_eq!(doc.op.as_deref(), Some("lm_status"));
    assert_eq!(obj(&doc.params), json!({"path": "/workspace", "ignored": false}));
}

// 8. Throwing compat wrapper parseGraphqlInput
#[test]
fn graphql_parse_graphql_input_compat_wrapper_succeeds_or_throws() {
    let ok = parse_graphql_input("{ lm_status(path: \"/foo\") }").unwrap();
    assert_eq!(ok.op.as_deref(), Some("lm_status"));
    assert_eq!(obj(&ok.params), json!({"path": "/foo"}));

    let err = parse_graphql_input("{ lm_status(bad: $var) }").unwrap_err();
    assert_eq!(err.operation, "lm_status");
    assert!(err.details.get("synopsis").is_some());
    assert!(err.details.get("issues").is_some_and(Value::is_array));
}

// 9. Nested data types (arrays, objects, numbers, booleans, enums)
#[test]
fn graphql_nested_arrays_objects_booleans_numbers_and_enums_parse_accurately() {
    let text = r#"
      {
        custom_op(
          str: "hello",
          num: 42.5,
          neg: -10,
          bool: true,
          enumVal: OPEN,
          arr: [1, 2, "three"],
          obj: { key: "value", innerArr: [true, false] }
        )
      }
    "#;
    let doc = parse_graphql_document(text).unwrap();
    assert_eq!(doc.op.as_deref(), Some("custom_op"));
    let expected = json!({
        "str": "hello",
        "num": 42.5,
        "neg": -10,
        "bool": true,
        "enumVal": "OPEN",
        "arr": [1, 2, "three"],
        "obj": {"key": "value", "innerArr": [true, false]},
    });
    assert!(deep_eq(&obj(&doc.params), &expected), "{:?}", doc.params);
}

// 10. Reject trailing content
#[test]
fn graphql_reject_unexpected_trailing_content() {
    let issues = parse_graphql_document("{ lm_status } extra").unwrap_err();
    assert_eq!(issues[0].code, IssueCode::DocumentParse);
    assert!(
        issues[0].message.contains("unexpected trailing content"),
        "{}",
        issues[0].message
    );
    assert_eq!(issues[0].op.as_deref(), Some("lm_status"));
}

// 11. parseGraphqlDocument returns null-prototype params: JS-specific (prototype check);
// the nested object value is still checked.
#[test]
fn graphql_parse_graphql_document_returns_null_prototype_params_and_nested_objects() {
    let doc = parse_graphql_document("{ lm_status(path: \"/tmp\", meta: { tag: \"test\" }) }").unwrap();
    assert_eq!(obj(&doc.params), json!({"path": "/tmp", "meta": {"tag": "test"}}));
}

// 12. hostile GraphQL with __proto__ fails validation
#[test]
fn graphql_hostile_proto_input_fails_validation() {
    let doc = parse_graphql_document("{ lm_status(__proto__: \"danger\", path: \"/safe\") }").unwrap();
    assert_eq!(doc.params.get("__proto__"), Some(&json!("danger")));
    let issues = validate(&status_schema(), &doc.params, ValidationOptions::default()).unwrap_err();
    assert!(issues
        .iter()
        .any(|i| i.code == IssueCode::UnknownParam && i.param.as_deref() == Some("__proto__")));
}
