//! Port of lmts `tests/encode.test.ts`: encoder round trips (T10b), encode -> server-side
//! parser. Each `test.each(CASES)` row becomes one Rust test per encoder.

use lmrust::js::deep_eq;
use lmrust::{
    encode_argv, encode_graphql, encode_json, parse_argv_form, parse_graphql_input, parse_json_input, Access,
    EncodeArgvOptions, OperationSchema, Record,
};
use serde_json::{json, Value};

fn schema() -> OperationSchema {
    serde_json::from_value(json!({
        "name": "lm_commit",
        "access": "write",
        "params": [
            {"name": "repo", "type": "string", "positional": true, "occurs": "?"},
            {"name": "message", "type": "string", "occurs": "?"},
            {"name": "rev", "type": "string", "occurs": "?"},
            {"name": "limit", "type": "number", "occurs": "?"},
            {"name": "dryRun", "type": "boolean", "occurs": "?"},
            {"name": "wip", "type": "boolean", "occurs": "?"},
            {"name": "config", "type": "object", "occurs": "?"},
            {"name": "paths", "type": "array", "items": "string", "occurs": "*"},
            {"name": "ids", "type": "array", "items": "number", "occurs": "*"},
            {"name": "tag", "type": "string", "occurs": "+"},
        ],
    }))
    .unwrap()
}

fn rec(v: Value) -> Record {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn case(name: &str) -> Record {
    rec(match name {
        "plain" => json!({"message": "hello", "limit": 5, "wip": true, "dryRun": false}),
        "space" => json!({"message": "hello world", "repo": "/tmp/my repo"}),
        "equals" => json!({"message": "a=b=c"}),
        "leading dash" => json!({"message": "-x", "rev": "--not-a-flag", "limit": -3.5}),
        "string-like literals" => json!({"message": "true", "rev": "123"}),
        "more string-like" => json!({"message": "false", "rev": "2024-01-01"}),
        "arrays" => json!({"paths": ["a b", "-c", "d=e"], "ids": [1, 2.5, -3], "tag": ["x"]}),
        "nested object" => json!({"config": {"a": {"b": [1, "two", null, true]}, "s": "q\"uote\\n"}}),
        "unicode/newline" => json!({"message": "line1\nline2\t\u{2603} \"q\""}),
        other => panic!("unknown case {other}"),
    })
}

fn argv_round_trip(name: &str) {
    let s = schema();
    let params = case(name);
    let argv = encode_argv(&s, &params, EncodeArgvOptions::default());
    let r = parse_argv_form(&argv, &s, &[]).unwrap_or_else(|i| panic!("{name}: {argv:?}: {i:?}"));
    assert!(
        deep_eq(&Value::Object(r.params.clone()), &Value::Object(params.clone())),
        "{name}: {argv:?} -> {:?} != {params:?}",
        r.params
    );
}

fn json_round_trip(name: &str) {
    let s = schema();
    let params = case(name);
    let text = encode_json(&s, &params);
    let doc = parse_json_input(&text).unwrap_or_else(|e| panic!("{name}: {text}: {e}"));
    assert_eq!(doc.op.as_deref(), Some("lm_commit"));
    assert!(
        deep_eq(&Value::Object(doc.params.clone()), &Value::Object(params.clone())),
        "{name}: {text} -> {:?}",
        doc.params
    );
}

fn graphql_round_trip(name: &str) {
    let s = schema();
    let params = case(name);
    let text = encode_graphql(&s, &params).unwrap_or_else(|e| panic!("{name}: {e}"));
    let doc = parse_graphql_input(&text).unwrap_or_else(|e| panic!("{name}: {text}: {e}"));
    assert_eq!(doc.op.as_deref(), Some("lm_commit"));
    assert!(
        deep_eq(&Value::Object(doc.params.clone()), &Value::Object(params.clone())),
        "{name}: {text} -> {:?}",
        doc.params
    );
}

macro_rules! round_trips {
    ($($id:ident => $name:literal),* $(,)?) => {
        // describe('encodeArgv -> parseArgvForm') test.each(CASES)
        mod encode_argv_to_parse_argv_form {
            $(#[test] fn $id() { super::argv_round_trip($name); })*
        }
        // describe('encodeJson -> parseJsonInput') test.each(CASES)
        mod encode_json_to_parse_json_input {
            $(#[test] fn $id() { super::json_round_trip($name); })*
        }
        // describe('encodeGraphql -> parseGraphqlInput') test.each(CASES)
        mod encode_graphql_to_parse_graphql_input {
            $(#[test] fn $id() { super::graphql_round_trip($name); })*
        }
    };
}

round_trips! {
    plain => "plain",
    space => "space",
    equals => "equals",
    leading_dash => "leading dash",
    string_like_literals => "string-like literals",
    more_string_like => "more string-like",
    arrays => "arrays",
    nested_object => "nested object",
    unicode_newline => "unicode/newline",
}

// describe('encodeArgv -> parseArgvForm')
#[test]
fn positional_option_emits_bare_words_after_double_dash() {
    let s = schema();
    let params = rec(json!({"repo": "--weird", "wip": true}));
    let argv = encode_argv(&s, &params, EncodeArgvOptions { positional: true });
    assert_eq!(argv, vec!["--wip", "--", "--weird"]);
    let r = parse_argv_form(&argv, &s, &[]).unwrap_or_else(|i| panic!("{i:?}"));
    assert_eq!(Value::Object(r.params), json!({"repo": "--weird", "wip": true}));
}

// 'undefined params are omitted': Rust records have no `undefined`; an absent key is the
// equivalent, so only the present param is encoded.
#[test]
fn undefined_params_are_omitted() {
    let argv = encode_argv(&schema(), &rec(json!({"wip": true})), EncodeArgvOptions::default());
    assert_eq!(argv, vec!["--wip"]);
}

// Asserts insertion-ordered output: needs the `preserve_order` feature (on by default).
#[cfg(feature = "preserve_order")]
#[test]
fn booleans_use_k_and_no_k() {
    let argv = encode_argv(
        &schema(),
        &rec(json!({"wip": true, "dryRun": false})),
        EncodeArgvOptions::default(),
    );
    assert_eq!(argv, vec!["--wip", "--no-dry-run"]);
}

#[test]
fn additional_params_round_trip_via_json_coercion() {
    let lenient: OperationSchema = serde_json::from_value(json!({
        "name": "run",
        "params": [],
        "additionalParams": true,
    }))
    .unwrap();
    let params = rec(json!({"a": 1, "b": "hello world", "c": {"x": [1]}, "d": false}));
    let argv = encode_argv(&lenient, &params, EncodeArgvOptions::default());
    let r = parse_argv_form(&argv, &lenient, &[]).unwrap_or_else(|i| panic!("{argv:?}: {i:?}"));
    assert!(
        deep_eq(&Value::Object(r.params.clone()), &Value::Object(params)),
        "{argv:?} -> {:?}",
        r.params
    );
}

// describe('encodeJson -> parseJsonInput')
// 'envelope shape, undefined omitted': `rev: undefined` has no Rust equivalent; the
// envelope shape is checked with the key absent.
#[test]
fn envelope_shape_undefined_omitted() {
    let text = encode_json(&schema(), &rec(json!({"message": "m"})));
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, json!({"op": "lm_commit", "params": {"message": "m"}}));
}

// describe('encodeGraphql -> parseGraphqlInput')
#[test]
fn uses_mutation_for_write_ops_query_otherwise() {
    let s = schema();
    assert_eq!(
        encode_graphql(&s, &Record::new()).unwrap(),
        "mutation { lm_commit { __typename } }"
    );
    let mut read = s.clone();
    read.access = Some(Access::Read);
    assert_eq!(
        encode_graphql(&read, &rec(json!({"limit": 1}))).unwrap(),
        "query { lm_commit(limit: 1) { __typename } }"
    );
}

// The `limit: NaN` half is not portable: serde_json values cannot hold NaN.
#[test]
fn rejects_values_the_subset_cannot_express() {
    assert!(encode_graphql(&schema(), &rec(json!({"config": {"bad-key": 1}}))).is_err());
}
