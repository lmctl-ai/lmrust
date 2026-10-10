//! Port of lmts `tests/publish.test.ts` (T10 published schema validation and round
//! trips), `tests/publish-edge.test.ts` (warnings, structural errors, synopsis helpers)
//! and `tests/publish-help.test.ts` (published document and derived help).

use lmrust::{
    find_operation, is_effectful, parse_published_schema, parse_published_schema_or_err, publish_schema,
    render_op_help, render_tool_help, schema_warnings, synopsis_for_param, to_camel, usage_line, validate,
    validate_params, InputForm, IssueCode, OperationSchema, ParamSpec, PolicyFlags, PublishOptions, PublishedCliSchema,
    Record, ReturnType, ValidationIssue, ValidationOptions,
};
use serde_json::{json, Value};

fn op(v: Value) -> OperationSchema {
    serde_json::from_value(v).expect("valid OperationSchema literal")
}

fn spec(v: Value) -> ParamSpec {
    serde_json::from_value(v).expect("valid ParamSpec literal")
}

fn specs(v: Value) -> Vec<ParamSpec> {
    serde_json::from_value(v).expect("valid ParamSpec list literal")
}

fn rec(v: Value) -> Record {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn to_json(doc: &PublishedCliSchema) -> Value {
    serde_json::to_value(doc).unwrap()
}

/// JSON.parse(JSON.stringify(doc))
fn round_trip(doc: &PublishedCliSchema) -> Value {
    serde_json::from_str(&serde_json::to_string(doc).unwrap()).unwrap()
}

fn issues_of(v: &Value) -> Vec<ValidationIssue> {
    match parse_published_schema(v) {
        Ok(doc) => panic!("expected failure, got {doc:?}"),
        Err(issues) => issues,
    }
}

fn has(issues: &[ValidationIssue], code: Option<IssueCode>, needle: &str) -> bool {
    issues
        .iter()
        .any(|i| code.map_or(true, |c| i.code == c) && i.message.contains(needle))
}

/// `{ ...base, key: value }`
fn with(base: &Value, key: &str, value: Value) -> Value {
    let mut m = base.as_object().unwrap().clone();
    m.insert(key.into(), value);
    Value::Object(m)
}

// ===========================================================================
// publish.test.ts — T10 PublishedCliSchema validation and round-trip
// ===========================================================================

fn sample_op() -> OperationSchema {
    op(json!({
        "name": "lm_status",
        "access": "read",
        "returns": "object",
        "description": "Show git status",
        "params": [
            { "name": "path", "type": "string", "occurs": "?", "default": ".", "description": "Directory" },
            { "name": "ignored", "type": "boolean", "occurs": "?", "default": false, "description": "Show ignored" },
            { "name": "limit", "type": "number", "occurs": "?", "description": "Limit items" },
            { "name": "state", "type": "string", "occurs": "?", "enum": ["open", "closed"], "description": "State" }
        ]
    }))
}

fn version(v: &str) -> PublishOptions {
    PublishOptions {
        version: Some(v.into()),
        ..Default::default()
    }
}

// 1. Full Consumer Round-Trip
#[test]
fn t10_round_trip_publish_stringify_parse_find_operation_validate() {
    let published = publish_schema("git2", vec![sample_op()], version("1.0.0"));
    let parsed_obj = round_trip(&published);

    let doc = parse_published_schema(&parsed_obj).expect("round-tripped document parses");
    assert_eq!(doc.tool, "git2");
    assert_eq!(doc.version.as_deref(), Some("1.0.0"));

    let op = find_operation(&doc, "lm_status").expect("lm_status");

    // Validate accepts good input
    let good = validate(
        op,
        &rec(json!({ "path": "/home/workspace", "ignored": true, "limit": 10 })),
        ValidationOptions::default(),
    )
    .expect("good input validates");
    assert_eq!(good["path"], json!("/home/workspace"));
    assert_eq!(good["ignored"], json!(true));
    assert_eq!(good["limit"], json!(10));

    // Validate rejects bad input with synopsis
    let bad = validate(
        op,
        &rec(json!({ "limit": "not-a-number" })),
        ValidationOptions::default(),
    )
    .expect_err("bad input rejected");
    assert!(!bad.is_empty());
    assert_eq!(bad[0].code, IssueCode::TypeMismatch);
    assert!(!bad[0].synopsis.is_empty());
    assert!(!bad[0].usage.is_empty());
}

// 2. Version passthrough & default
#[test]
fn t10_version_passthrough_and_fallback_to_unknown() {
    let custom = publish_schema("git2", vec![sample_op()], version("2.4.0"));
    assert_eq!(custom.version.as_deref(), Some("2.4.0"));

    let fallback = publish_schema("git2", vec![sample_op()], PublishOptions::default());
    assert_eq!(fallback.version.as_deref(), Some("unknown"));

    let undefined_version = publish_schema(
        "git2",
        vec![sample_op()],
        PublishOptions {
            version: None,
            ..Default::default()
        },
    );
    assert_eq!(undefined_version.version.as_deref(), Some("unknown"));
}

// 3. Globals round-trip
#[test]
fn t10_globals_round_trip() {
    let doc = publish_schema(
        "git2",
        vec![sample_op()],
        PublishOptions {
            globals: Some(specs(json!([
                { "name": "dry-run", "type": "boolean", "aliases": ["dryrun"], "description": "Simulate" },
                { "name": "confirm", "type": "boolean", "description": "Confirm action" }
            ]))),
            ..Default::default()
        },
    );
    let parsed = parse_published_schema(&round_trip(&doc)).expect("parses");
    let globals = parsed.globals.expect("globals");
    assert_eq!(globals.len(), 2);
    assert_eq!(globals[0].name, "dry-run");
}

// 4. Returns round-trip
#[test]
fn t10_returns_field_round_trip_on_operation_schema() {
    let op_with_returns = op(json!({
        "name": "list_tags",
        "access": "read",
        "returns": "array",
        "description": "List git tags",
        "params": []
    }));
    let doc = publish_schema("git2", vec![op_with_returns], PublishOptions::default());
    let parsed = parse_published_schema(&round_trip(&doc)).expect("parses");
    let op = find_operation(&parsed, "list_tags").expect("list_tags");
    assert_eq!(op.returns, Some(ReturnType::Array));
}

// 5. Non-object document rejected (`undefined` has no JSON counterpart and is not ported)
#[test]
fn t10_rejects_non_object_documents() {
    for val in [json!(null), json!("string"), json!(123), json!(true), json!([])] {
        let issues = issues_of(&val);
        assert_eq!(issues[0].code, IssueCode::DocumentParse, "{val}");
    }
}

// 6. Unsupported schema tag rejected
#[test]
fn t10_rejects_unsupported_schema_tag() {
    let issues = issues_of(&json!({ "schema": "cli.schema.v2", "tool": "git2", "operations": [] }));
    assert!(has(&issues, None, "unsupported schema tag"), "{issues:#?}");
}

// 7. Invalid tool name rejected
#[test]
fn t10_rejects_invalid_tool_names() {
    for bad_tool in ["Git2", "123tool", "tool-kebab", "tool.dot", ""] {
        let issues = issues_of(&json!({ "schema": "cli.schema.v1", "tool": bad_tool, "operations": [] }));
        assert!(
            issues
                .iter()
                .any(|i| i.param.is_none() && i.message.contains("tool must be a string matching")),
            "{bad_tool:?}: {issues:#?}"
        );
    }
}

// 8. Duplicate operations rejected
#[test]
fn t10_rejects_duplicate_operation_names() {
    let issues = issues_of(&json!({
        "schema": "cli.schema.v1",
        "tool": "git2",
        "operations": [{ "name": "status", "params": [] }, { "name": "status", "params": [] }]
    }));
    assert!(has(&issues, None, "duplicate operation name \"status\""), "{issues:#?}");
}

// 9. Invalid operation name rejected
#[test]
fn t10_rejects_invalid_operation_names() {
    let issues = issues_of(&json!({
        "schema": "cli.schema.v1",
        "tool": "git2",
        "operations": [{ "name": "Status-Op", "params": [] }]
    }));
    assert!(
        has(&issues, None, "operation name \"Status-Op\" must match"),
        "{issues:#?}"
    );
}

fn one_op_doc(op: Value) -> Value {
    json!({ "schema": "cli.schema.v1", "tool": "git2", "operations": [op] })
}

// 10. Invalid access rejected
#[test]
fn t10_rejects_invalid_operation_access() {
    let issues = issues_of(&one_op_doc(
        json!({ "name": "status", "access": "unsafe", "params": [] }),
    ));
    assert!(
        has(&issues, Some(IssueCode::EnumViolation), "invalid access"),
        "{issues:#?}"
    );
}

// 11. Invalid returns rejected
#[test]
fn t10_rejects_invalid_returns_type() {
    let issues = issues_of(&one_op_doc(
        json!({ "name": "status", "returns": "custom_class", "params": [] }),
    ));
    assert!(
        has(&issues, Some(IssueCode::EnumViolation), "invalid returns"),
        "{issues:#?}"
    );
}

// 12. Param named dryrun rejected
#[test]
fn t10_rejects_param_named_dryrun() {
    let issues = issues_of(&one_op_doc(
        json!({ "name": "commit", "params": [{ "name": "dryrun", "type": "boolean" }] }),
    ));
    assert!(has(&issues, None, "cannot be named \"dryrun\""), "{issues:#?}");
}

// 13. Invalid param name rejected
#[test]
fn t10_rejects_invalid_param_names_in_operations() {
    // kebab-case not allowed in op params
    let issues = issues_of(&one_op_doc(
        json!({ "name": "commit", "params": [{ "name": "dry-run", "type": "boolean" }] }),
    ));
    assert!(has(&issues, None, "name must match ^[A-Za-z_]\\w*$"), "{issues:#?}");
}

// 14. Invalid param type rejected
#[test]
fn t10_rejects_invalid_param_type() {
    let issues = issues_of(&one_op_doc(
        json!({ "name": "status", "params": [{ "name": "path", "type": "filepath" }] }),
    ));
    assert!(
        has(&issues, Some(IssueCode::EnumViolation), "invalid type"),
        "{issues:#?}"
    );
}

// 15. Invalid occurs rejected
#[test]
fn t10_rejects_invalid_occurs() {
    let issues = issues_of(&one_op_doc(
        json!({ "name": "status", "params": [{ "name": "path", "type": "string", "occurs": "many" }] }),
    ));
    assert!(
        has(&issues, Some(IssueCode::EnumViolation), "invalid occurs"),
        "{issues:#?}"
    );
}

// 16. Invalid default value rejected
#[test]
fn t10_rejects_default_value_not_matching_param_type() {
    let issues = issues_of(&one_op_doc(
        json!({ "name": "status", "params": [{ "name": "count", "type": "number", "default": "ten" }] }),
    ));
    assert!(
        has(&issues, Some(IssueCode::TypeMismatch), "default value does not match"),
        "{issues:#?}"
    );
}

// 17. Global param names allow kebab-case
#[test]
fn t10_global_param_names_allow_kebab_case() {
    let doc = json!({
        "schema": "cli.schema.v1",
        "tool": "git2",
        "policy": { "dryrun": true, "readonly": true },
        "inputs": ["flags", "json", "graphql"],
        "globals": [
            { "name": "dry-run", "type": "boolean", "aliases": ["dryrun"] },
            { "name": "read-only", "type": "boolean" }
        ],
        "operations": []
    });
    assert!(
        parse_published_schema(&doc).is_ok(),
        "{:#?}",
        parse_published_schema(&doc)
    );
}

// 18. Schema warnings helper detects missing descriptions without failing
#[test]
fn t10_schema_warnings_identifies_missing_descriptions_and_attaches_doc_warnings() {
    let mut doc = publish_schema(
        "git2",
        vec![op(json!({
            "name": "undocumented_op",
            "params": [
                { "name": "undocumented_param", "type": "string" },
                { "name": "documented_param", "type": "string", "description": "Has description" }
            ]
        }))],
        PublishOptions::default(),
    );

    // Validation passes despite missing descriptions
    assert!(parse_published_schema(&to_json(&doc)).is_ok());

    // schemaWarnings helper populates warnings
    let warnings = schema_warnings(&mut doc);
    assert_eq!(warnings.len(), 2);
    assert!(warnings
        .iter()
        .any(|w| w.contains("operation \"undocumented_op\" is missing a description")));
    assert!(warnings
        .iter()
        .any(|w| w.contains("param \"undocumented_param\" of operation \"undocumented_op\" is missing a description")));
    assert_eq!(doc.warnings.as_ref(), Some(&warnings));
}

// 19. parsePublishedSchemaOrThrow compatibility wrapper
#[test]
fn t10_parse_published_schema_or_throw_returns_value_or_throws() {
    let valid_doc = publish_schema("git2", vec![sample_op()], PublishOptions::default());
    assert_eq!(parse_published_schema_or_err(&to_json(&valid_doc)).unwrap(), valid_doc);
    assert!(parse_published_schema_or_err(&json!({ "schema": "bad", "tool": "x", "operations": [] })).is_err());
}

// 20. isEffectful helper (the TS partial objects `{access: undefined}` / `{access: 'read'}`
// are ported as full ops with the same access).
#[test]
fn t10_is_effectful_false_only_for_read_access() {
    assert!(!is_effectful(&op(
        json!({ "name": "read_op", "access": "read", "params": [] })
    )));
    assert!(is_effectful(&op(
        json!({ "name": "write_op", "access": "write", "params": [] })
    )));
    assert!(is_effectful(&op(
        json!({ "name": "dyn_op", "access": "dynamic", "params": [] })
    )));
    assert!(is_effectful(&op(json!({ "name": "unspecified_op", "params": [] }))));
    assert!(is_effectful(&op(json!({ "name": "x", "params": [] }))));
    assert!(!is_effectful(&op(
        json!({ "name": "x", "access": "read", "params": [] })
    )));
}

const RESERVED: [&str; 3] = ["__proto__", "constructor", "prototype"];

// 21. Rejects tool named __proto__, constructor, prototype
#[test]
fn t10_rejects_reserved_tool_names() {
    for reserved in RESERVED {
        let issues = issues_of(&json!({ "schema": "cli.schema.v1", "tool": reserved, "operations": [] }));
        assert!(
            issues
                .iter()
                .any(|i| i.code == IssueCode::ReservedName || i.code == IssueCode::DocumentParse),
            "{reserved}: {issues:#?}"
        );
    }
}

// 22. Rejects operation named __proto__, constructor, prototype
#[test]
fn t10_rejects_reserved_operation_names() {
    for reserved in RESERVED {
        let issues = issues_of(&json!({
            "schema": "cli.schema.v1",
            "tool": "mytool",
            "operations": [{ "name": reserved, "params": [] }]
        }));
        assert!(
            issues
                .iter()
                .any(|i| i.code == IssueCode::ReservedName || i.code == IssueCode::DocumentParse),
            "{reserved}: {issues:#?}"
        );
    }
}

fn assert_reserved(doc: Value, what: &str) {
    let issues = issues_of(&doc);
    assert!(
        issues.iter().any(|i| i.code == IssueCode::ReservedName),
        "{what}: {issues:#?}"
    );
    assert!(
        issues[0].message.contains("reserved name"),
        "{what}: {}",
        issues[0].message
    );
}

// 23. Rejects param named __proto__, constructor, prototype
#[test]
fn t10_rejects_reserved_param_names() {
    for reserved in RESERVED {
        assert_reserved(
            json!({
                "schema": "cli.schema.v1",
                "tool": "mytool",
                "operations": [{ "name": "op1", "params": [{ "name": reserved, "type": "string" }] }]
            }),
            reserved,
        );
    }
}

// 24. Rejects param alias named __proto__, constructor, prototype
#[test]
fn t10_rejects_reserved_param_aliases() {
    for reserved in RESERVED {
        assert_reserved(
            json!({
                "schema": "cli.schema.v1",
                "tool": "mytool",
                "operations": [{
                    "name": "op1",
                    "params": [{ "name": "validParam", "type": "string", "aliases": [reserved] }]
                }]
            }),
            reserved,
        );
    }
}

// 25. Rejects global param named __proto__, constructor, prototype
#[test]
fn t10_rejects_reserved_global_names() {
    for reserved in RESERVED {
        assert_reserved(
            json!({
                "schema": "cli.schema.v1",
                "tool": "mytool",
                "globals": [{ "name": reserved, "type": "string" }],
                "operations": []
            }),
            reserved,
        );
    }
}

// 26. Rejects global param alias named __proto__, constructor, prototype
#[test]
fn t10_rejects_reserved_global_aliases() {
    for reserved in RESERVED {
        assert_reserved(
            json!({
                "schema": "cli.schema.v1",
                "tool": "mytool",
                "globals": [{ "name": "validGlobal", "type": "string", "aliases": [reserved] }],
                "operations": []
            }),
            reserved,
        );
    }
}

// ===========================================================================
// publish-edge.test.ts
// ===========================================================================

#[test]
fn edge_schema_warnings_detects_globals_without_description() {
    let mut doc: PublishedCliSchema = serde_json::from_value(json!({
        "schema": "cli.schema.v1",
        "tool": "mytool",
        "policy": { "dryrun": true, "readonly": true },
        "inputs": ["flags"],
        "globals": [
            { "name": "dryrun", "type": "boolean", "description": "" },
            { "name": "verbose", "type": "boolean" }
        ],
        "operations": []
    }))
    .unwrap();
    let warnings = schema_warnings(&mut doc);
    assert_eq!(warnings.len(), 2);
    assert!(warnings[0].contains("global param \"dryrun\" is missing a description"));
    assert!(warnings[1].contains("global param \"verbose\" is missing a description"));
}

#[test]
fn edge_publish_schema_with_default_custom_options() {
    let ops = vec![op(
        json!({ "name": "op1", "access": "read", "description": "First op", "params": [] }),
    )];
    // With version undefined -> defaults to 'unknown'
    let doc1 = publish_schema(
        "mytool",
        ops.clone(),
        PublishOptions {
            version: None,
            ..Default::default()
        },
    );
    assert_eq!(doc1.version.as_deref(), Some("unknown"));

    // With explicit version, globals, inputs, policy
    let doc2 = publish_schema(
        "mytool",
        ops,
        PublishOptions {
            version: Some("2.0.0".into()),
            globals: Some(specs(json!([{ "name": "flag", "type": "boolean", "occurs": "?" }]))),
            inputs: Some(vec![InputForm::Flags, InputForm::Json]),
            policy: Some(PolicyFlags {
                dryrun: false,
                readonly: true,
            }),
            ..Default::default()
        },
    );
    assert_eq!(doc2.version.as_deref(), Some("2.0.0"));
    assert_eq!(doc2.globals.as_ref().map(Vec::len), Some(1));
    assert_eq!(doc2.inputs, Some(vec![InputForm::Flags, InputForm::Json]));
    assert!(!doc2.policy.unwrap().dryrun);
}

fn base_doc() -> Value {
    json!({
        "schema": "cli.schema.v1",
        "tool": "mytool",
        "policy": { "dryrun": true, "readonly": true },
        "inputs": ["flags"],
        "operations": [{ "name": "testop", "params": [] }]
    })
}

fn with_params(params: Value) -> Value {
    with(
        &base_doc(),
        "operations",
        json!([{ "name": "testop", "params": params }]),
    )
}

#[test]
fn edge_version_not_a_string() {
    let issues = issues_of(&with(&base_doc(), "version", json!(123)));
    assert!(
        has(&issues, Some(IssueCode::TypeMismatch), "version must be a string"),
        "{issues:#?}"
    );
}

#[test]
fn edge_policy_not_an_object_or_missing_boolean_properties() {
    let issues = issues_of(&with(&base_doc(), "policy", json!("invalid")));
    assert!(
        has(&issues, Some(IssueCode::TypeMismatch), "policy must be an object"),
        "{issues:#?}"
    );

    let issues = issues_of(&with(
        &base_doc(),
        "policy",
        json!({ "dryrun": "yes", "readonly": true }),
    ));
    assert!(
        has(&issues, Some(IssueCode::TypeMismatch), "boolean dryrun and readonly"),
        "{issues:#?}"
    );
}

#[test]
fn edge_inputs_not_an_array_or_containing_invalid_input_forms() {
    let issues = issues_of(&with(&base_doc(), "inputs", json!("flags")));
    assert!(
        has(&issues, Some(IssueCode::TypeMismatch), "inputs must be an array"),
        "{issues:#?}"
    );

    let issues = issues_of(&with(&base_doc(), "inputs", json!(["flags", "rest-api"])));
    assert!(
        has(&issues, Some(IssueCode::EnumViolation), "unsupported form"),
        "{issues:#?}"
    );
}

#[test]
fn edge_param_not_an_object_or_missing_string_name() {
    let issues = issues_of(&with_params(json!(["string_not_obj"])));
    assert!(
        has(&issues, Some(IssueCode::TypeMismatch), "must be an object"),
        "{issues:#?}"
    );

    let issues = issues_of(&with_params(json!([{ "type": "string" }])));
    assert!(
        has(&issues, Some(IssueCode::TypeMismatch), "needs a string name"),
        "{issues:#?}"
    );
}

#[test]
fn edge_param_enum_not_an_array_of_strings_or_numbers() {
    let issues = issues_of(&with_params(
        json!([{ "name": "choice", "type": "string", "enum": [true, false] }]),
    ));
    assert!(
        has(&issues, Some(IssueCode::TypeMismatch), "enum must be an array"),
        "{issues:#?}"
    );
}

#[test]
fn edge_param_items_invalid() {
    let issues = issues_of(&with_params(
        json!([{ "name": "items", "type": "array", "items": "function" }]),
    ));
    assert!(
        has(&issues, Some(IssueCode::EnumViolation), "invalid items type"),
        "{issues:#?}"
    );
}

#[test]
fn edge_default_value_type_mismatches_enum_array_object_json() {
    // Enum mismatch
    let issues = issues_of(&with_params(json!([
        { "name": "mode", "type": "string", "enum": ["fast", "slow"], "default": "turbo" }
    ])));
    assert!(
        has(&issues, Some(IssueCode::TypeMismatch), "default value"),
        "{issues:#?}"
    );

    // Array mismatch
    assert!(parse_published_schema(&with_params(json!([
        { "name": "tags", "type": "array", "default": "not-an-array" }
    ])))
    .is_err());

    // Object / json / boolean defaults valid
    let ok_doc = with_params(json!([
        { "name": "cfg", "type": "object", "default": { "key": "val" } },
        { "name": "anydata", "type": "json", "default": 123 },
        { "name": "flag", "type": "boolean", "default": true }
    ]));
    assert!(
        parse_published_schema(&ok_doc).is_ok(),
        "{:#?}",
        parse_published_schema(&ok_doc)
    );
}

#[test]
fn edge_globals_not_an_array() {
    let issues = issues_of(&with(&base_doc(), "globals", json!("not-array")));
    assert!(
        has(&issues, Some(IssueCode::TypeMismatch), "globals must be an array"),
        "{issues:#?}"
    );
}

#[test]
fn edge_operation_not_an_object_or_without_string_name_or_missing_params() {
    let issues = issues_of(&with(&base_doc(), "operations", json!(["bad_op"])));
    assert!(
        has(&issues, Some(IssueCode::DocumentParse), "must be an object"),
        "{issues:#?}"
    );

    let issues = issues_of(&with(&base_doc(), "operations", json!([{ "access": "read" }])));
    assert!(
        has(&issues, Some(IssueCode::DocumentParse), "needs a string name"),
        "{issues:#?}"
    );

    let issues = issues_of(&with(&base_doc(), "operations", json!([{ "name": "testop" }])));
    assert!(
        has(&issues, Some(IssueCode::DocumentParse), "needs a params array"),
        "{issues:#?}"
    );
}

// synopsis.ts coverage helpers

#[test]
fn edge_usage_line_with_tool_name_and_positional_params() {
    let schema = op(json!({
        "name": "clone",
        "access": "write",
        "params": [
            { "name": "url", "type": "string", "positional": true },
            { "name": "dir", "type": "string", "positional": true },
            { "name": "depth", "type": "number", "occurs": "?" }
        ]
    }));
    assert_eq!(usage_line(&schema, Some("git2")), "git2 clone [<url>] [<dir>] [flags]");
    assert_eq!(usage_line(&schema, None), "clone [<url>] [<dir>] [flags]");
}

#[test]
fn edge_to_camel_converts_kebab_case_to_camel_case() {
    assert_eq!(to_camel("max-files"), "maxFiles");
    assert_eq!(to_camel("branch-name-target"), "branchNameTarget");
}

#[test]
fn edge_synopsis_for_param_for_array_with_items_enum_and_required() {
    let p_req = spec(json!({ "name": "target", "type": "string", "occurs": "1" }));
    assert_eq!(synopsis_for_param(&p_req), "--target <string>");

    let p_array = spec(json!({ "name": "files", "type": "array", "items": "string", "occurs": "*" }));
    assert!(synopsis_for_param(&p_array).contains("--files <string of string>"));
    assert!(synopsis_for_param(&p_array).contains("[optional]"));

    let p_enum = spec(json!({ "name": "level", "type": "string", "occurs": "?", "enum": ["debug", "info"] }));
    assert!(synopsis_for_param(&p_enum).contains("one of: debug, info"));
}

// ===========================================================================
// publish-help.test.ts
// ===========================================================================

fn lm_status() -> OperationSchema {
    op(json!({
        "name": "lm_status",
        "access": "read",
        "description": "Structured git status",
        "params": [
            { "name": "path", "type": "string", "occurs": "?", "default": "." },
            { "name": "ignored", "type": "boolean", "occurs": "?", "default": false }
        ]
    }))
}

fn lm_blame() -> OperationSchema {
    op(json!({
        "name": "lm_blame",
        "access": "read",
        "description": "Per-line blame",
        "params": [
            { "name": "path", "type": "string", "occurs": "?", "default": "." },
            { "name": "file", "type": "string" },
            { "name": "revision", "type": "string", "occurs": "?", "default": "HEAD" }
        ]
    }))
}

#[test]
fn help_documents_carry_v1_tag_and_advertise_all_three_input_forms() {
    let doc = publish_schema("git2", vec![lm_status(), lm_blame()], PublishOptions::default());
    assert_eq!(doc.schema, "cli.schema.v1");
    assert_eq!(doc.tool, "git2");
    assert_eq!(
        doc.inputs,
        Some(vec![InputForm::Flags, InputForm::Json, InputForm::Graphql])
    );
    assert_eq!(
        doc.policy,
        Some(PolicyFlags {
            dryrun: true,
            readonly: true
        })
    );
    let names: Vec<&str> = doc.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, vec!["lm_status", "lm_blame"]);
}

#[test]
fn help_find_operation_locates_by_name() {
    let doc = publish_schema("git2", vec![lm_status(), lm_blame()], PublishOptions::default());
    assert_eq!(find_operation(&doc, "lm_blame").map(|o| o.params.len()), Some(3));
    assert!(find_operation(&doc, "nope").is_none());
}

#[test]
fn help_parse_accepts_a_well_formed_document() {
    let doc = publish_schema("git2", vec![lm_status()], PublishOptions::default());
    assert_eq!(parse_published_schema(&to_json(&doc)).unwrap(), doc);
    assert_eq!(parse_published_schema_or_err(&to_json(&doc)).unwrap(), doc);
}

#[test]
fn help_parse_rejects_wrong_schema_tag() {
    let bad = json!({ "schema": "v2", "tool": "x", "operations": [] });
    let err = parse_published_schema_or_err(&bad).expect_err("throws");
    assert!(err.message.contains("unsupported schema tag"), "{}", err.message);
    assert!(parse_published_schema(&bad).is_err());
}

#[test]
fn help_parse_rejects_non_array_operations() {
    let bad = json!({ "schema": "cli.schema.v1", "tool": "x", "operations": {} });
    let err = parse_published_schema_or_err(&bad).expect_err("throws");
    assert!(err.message.contains("operations must be an array"), "{}", err.message);
    assert!(parse_published_schema(&bad).is_err());
}

#[test]
fn help_render_op_help_usage_line_brackets_for_optional_bare_for_required() {
    let help = render_op_help("git2", &lm_blame());
    for needle in [
        "git2 lm_blame",
        "--path <string>",
        "--file <string>",
        "[--revision <string>]",
        "[required]",
    ] {
        assert!(help.contains(needle), "missing {needle:?} in:\n{help}");
    }
}

#[test]
fn help_enums_are_rendered_with_one_of() {
    let help = render_op_help(
        "gh2",
        &op(json!({
            "name": "list_pull_requests",
            "access": "read",
            "params": [{
                "name": "state",
                "type": "string",
                "occurs": "?",
                "enum": ["open", "closed", "all"],
                "default": "open"
            }]
        })),
    );
    assert!(help.contains("one of: open, closed, all"), "{help}");
    assert!(help.contains("(default: \"open\")"), "{help}");
}

#[test]
fn help_cardinality_marks_and_aliases_on_positional_and_flag_parameters() {
    let schema = op(json!({
        "name": "test_cardinality",
        "access": "read",
        "description": "Op to test all 4 cardinality marks and aliases",
        "params": [
            { "name": "reqPos", "type": "string", "positional": true, "occurs": "1", "description": "required positional" },
            { "name": "optPos", "type": "string", "positional": true, "occurs": "?", "description": "optional positional" },
            { "name": "plusPos", "type": "string", "positional": true, "occurs": "+", "description": "one-or-more positional" },
            { "name": "starPos", "type": "string", "positional": true, "occurs": "*", "description": "zero-or-more positional" },
            { "name": "reqFlag", "type": "string", "occurs": "1", "aliases": ["rf", "req-alias"], "description": "required flag" },
            { "name": "optFlag", "type": "string", "occurs": "?", "default": "def_val", "description": "optional flag" },
            { "name": "plusFlag", "type": "string", "occurs": "+", "description": "one-or-more flag" },
            { "name": "starFlag", "type": "string", "occurs": "*", "aliases": ["sf"], "description": "zero-or-more flag" }
        ]
    }));
    let help = render_op_help("tool", &schema);
    for needle in [
        // positional cardinality marks
        "<reqPos>",
        "[optPos]",
        "<plusPos>...",
        "[starPos]...",
        // flag cardinality marks in usage line
        "--req-flag <string>",
        "[--opt-flag <string>]",
        "--plus-flag <string>...",
        "[--star-flag <string>]...",
        // options section marks, aliases, defaults
        "--star-flag <string>...",
        "(aliases: --rf, --req-alias)",
        "(aliases: --sf)",
        "(default: \"def_val\")",
    ] {
        assert!(help.contains(needle), "missing {needle:?} in:\n{help}");
    }
}

#[test]
fn help_render_tool_help_lists_first_line_description_and_global_options() {
    let schemas = vec![op(json!({
        "name": "multi_line_op",
        "access": "write",
        "description": "First line summary\nSecond line details that should be omitted\nThird line",
        "params": []
    }))];
    let globals = specs(json!([
        { "name": "dryrun", "type": "boolean", "occurs": "?", "aliases": ["dry-run"], "description": "Simulate" },
        { "name": "confirm", "type": "boolean", "occurs": "?", "description": "Confirm dangerous ops" }
    ]));
    let help = render_tool_help("mytool", Some("My tool desc"), &schemas, &globals);
    assert!(
        help.contains("multi_line_op            [write]  First line summary"),
        "{help}"
    );
    assert!(!help.contains("Second line details"), "{help}");
    assert!(help.contains("--dryrun (--dry-run)"), "{help}");
    assert!(help.contains("--confirm"), "{help}");
    assert!(help.contains("Simulate"), "{help}");
}

#[test]
fn help_synopsis_for_param_used_in_error_messages() {
    let p = spec(json!({ "name": "state", "type": "string", "enum": ["open", "closed"] }));
    assert!(synopsis_for_param(&p).contains("--state <string>"));
    assert!(synopsis_for_param(&p).contains("one of: open, closed"));
}

#[test]
fn help_missing_required_includes_synopsis() {
    let err = validate_params(&lm_blame(), &Record::new()).expect_err("throws");
    assert!(err.message.contains("missing required param"), "{}", err.message);
    assert_eq!(err.details.get("synopsis"), Some(&json!("--file <string>")));
}

#[test]
fn help_enum_violation_includes_synopsis_and_allowed_set() {
    let schema = op(json!({
        "name": "list_pull_requests",
        "params": [{ "name": "state", "type": "string", "occurs": "?", "enum": ["open", "closed", "all"], "default": "open" }]
    }));
    let err = validate_params(&schema, &rec(json!({ "state": "purple" }))).expect_err("throws");
    assert!(err.message.contains("allowed: open, closed, all"), "{}", err.message);
    assert_eq!(
        err.details.get("synopsis"),
        Some(&json!("--state <string> one of: open, closed, all"))
    );
}

#[test]
fn help_type_mismatch_includes_synopsis() {
    let err = validate_params(&lm_status(), &rec(json!({ "ignored": "maybe" }))).expect_err("throws");
    assert_eq!(err.details.get("synopsis"), Some(&json!("--ignored")));
}

#[test]
fn help_consumer_can_validate_input_against_published_document() {
    // The producer publishes its schema:
    let doc = publish_schema("git2", vec![lm_status()], PublishOptions::default());

    // A consumer finds the op and validates a candidate input against the published
    // schema (not the producer's in-memory copy):
    let op = find_operation(&doc, "lm_status").expect("lm_status");
    let ok = validate_params(op, &rec(json!({ "ignored": true, "path": "/x" }))).unwrap();
    assert_eq!(Value::Object(ok), json!({ "ignored": true, "path": "/x" }));

    let err = validate_params(op, &rec(json!({ "bogus": 1 }))).expect_err("throws");
    assert!(err.message.contains("unknown param: bogus"), "{}", err.message);
}

#[test]
fn s5_parse_published_schema_rejects_unknown_param_keys_min_max_conflicts() {
    let issues = issues_of(&json!({
        "schema": "cli.schema.v1",
        "tool": "git2",
        "operations": [{
            "name": "op_with_ranges",
            "params": [{ "name": "count", "type": "number", "min": 1, "max": 10, "conflicts": ["other"] }]
        }]
    }));
    assert!(issues.iter().any(|i| i.code == IssueCode::UnknownParam), "{issues:#?}");
    for key in ["min", "max", "conflicts"] {
        assert!(issues.iter().any(|i| i.message.contains(key)), "{key}: {issues:#?}");
    }
}
