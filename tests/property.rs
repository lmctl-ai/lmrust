//! Property tests for lmrust (no node required).
//!
//! 1. encode -> parse round-trip for every fixture op under random values
//! 2. validate idempotence (validating already-validated output yields identical output)
//! 3. tokenizer never panics on arbitrary bytes-as-strings (1e5 random inputs)
//! 4. parse_published_schema(publish(schema)) round trip
//! 5. server run_cli_io never panics on random argv (fuzz 1e4) and always returns
//!    an exit code from the documented set {0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 127}.

use lmrust::js::deep_eq;
use lmrust::server::{run_cli_io, CliSpec, OpDef};
use lmrust::{
    encode_argv, encode_graphql, encode_json, parse_argv_form, parse_graphql_document, parse_json_document,
    parse_published_schema, publish_schema, to_kebab, tokenize_simple_command, validate, Access, EncodeArgvOptions,
    InputForm, Occurs, OperationSchema, ParamSpec, ParamType, PolicyFlags, ProductDetect, ProductInfo, PublishOptions,
    PublishedCliSchema, Record, ValidationOptions,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Deterministic, zero-dependency PRNG (SplitMix64).
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0xdeadbeefcafebabe } else { seed },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    pub fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    pub fn next_usize(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }

    pub fn next_bool(&mut self) -> bool {
        (self.next_u64() & 1) == 1
    }

    pub fn choose<'a, T>(&mut self, slice: &'a [T]) -> &'a T {
        assert!(!slice.is_empty());
        &slice[self.next_usize(slice.len())]
    }

    pub fn gen_ident(&mut self) -> String {
        let first = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_";
        let rest = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";
        let len = 1 + self.next_usize(12);
        let mut s = String::with_capacity(len);
        s.push(*self.choose(first) as char);
        for _ in 1..len {
            s.push(*self.choose(rest) as char);
        }
        s
    }

    pub fn gen_tool_or_op_name(&mut self) -> String {
        let first = b"abcdefghijklmnopqrstuvwxyz";
        let rest = b"abcdefghijklmnopqrstuvwxyz0123456789_";
        let len = 3 + self.next_usize(6);
        let mut s = String::with_capacity(len);
        s.push(*self.choose(first) as char);
        for _ in 1..len {
            s.push(*self.choose(rest) as char);
        }
        if s == "constructor" || s == "prototype" {
            s.push('_');
        }
        s
    }

    pub fn gen_ascii_word(&mut self, max_len: usize) -> String {
        let chars = b"abcdefghijklmnopqrstuvwxyz0123456789_-";
        let len = 1 + self.next_usize(max_len.max(1));
        (0..len).map(|_| *self.choose(chars) as char).collect()
    }
}

fn load_fixtures() -> Vec<PublishedCliSchema> {
    let mut docs = Vec::new();
    let local = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    if local.is_dir() {
        for entry in fs::read_dir(&local).unwrap().flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = fs::read_to_string(&p) {
                    if let Ok(v) = serde_json::from_str::<Value>(&content) {
                        if let Ok(doc) = parse_published_schema(&v) {
                            docs.push(doc);
                        }
                    }
                }
            }
        }
    }
    // Also check lmapi sibling fixtures if present
    let lmapi = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("lmapi/conformance/fixtures");
    if lmapi.is_dir() {
        for entry in fs::read_dir(&lmapi).unwrap().flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = fs::read_to_string(&p) {
                    if let Ok(v) = serde_json::from_str::<Value>(&content) {
                        if let Ok(doc) = parse_published_schema(&v) {
                            if !docs.iter().any(|d| d.tool == doc.tool) {
                                docs.push(doc);
                            }
                        }
                    }
                }
            }
        }
    }
    docs
}

/// Generate a valid random value for a parameter that survives round-trips.
fn gen_param_value(rng: &mut Rng, spec: &ParamSpec, for_argv: bool, for_graphql: bool) -> Value {
    if let Some(ref enum_vals) = spec.enum_values {
        if !enum_vals.is_empty() {
            let chosen = rng.choose(enum_vals);
            if spec.kind == ParamType::Array {
                return Value::Array(vec![chosen.clone()]);
            }
            return chosen.clone();
        }
    }

    let is_array = spec.kind == ParamType::Array
        || spec.occurs == Some(Occurs::OneOrMore)
        || spec.occurs == Some(Occurs::ZeroOrMore);

    if is_array {
        // For argv roundtrip: empty arrays cannot be expressed on CLI, so produce 1..3 items
        let count = if for_argv {
            1 + rng.next_usize(3)
        } else {
            rng.next_usize(3)
        };
        let item_type = spec.items.map(|i| i.as_param_type()).unwrap_or(ParamType::String);
        let items: Vec<Value> = (0..count)
            .map(|_| gen_scalar_value(rng, item_type, for_argv, for_graphql))
            .collect();
        return Value::Array(items);
    }

    gen_scalar_value(rng, spec.kind, for_argv, for_graphql)
}

fn gen_scalar_value(rng: &mut Rng, kind: ParamType, for_argv: bool, for_graphql: bool) -> Value {
    match kind {
        ParamType::String => {
            // Pick a safe string without whitespace or characters that require quoting for clean roundtrips
            let word = rng.gen_ascii_word(10);
            Value::String(word)
        }
        ParamType::Number => {
            let n = match rng.next_usize(5) {
                0 => 0,
                1 => 1,
                2 => (rng.next_u32() % 1000) as i64,
                3 => -((rng.next_u32() % 500) as i64),
                _ => 42,
            };
            Value::Number(serde_json::Number::from(n))
        }
        ParamType::Boolean => Value::Bool(rng.next_bool()),
        ParamType::Object => {
            let mut obj = serde_json::Map::new();
            let k = if for_graphql {
                rng.gen_ident()
            } else {
                rng.gen_ascii_word(6)
            };
            obj.insert(k, Value::String(rng.gen_ascii_word(6)));
            Value::Object(obj)
        }
        ParamType::Json => {
            // For argv roundtrip: avoid strings that look like booleans or numbers
            if for_argv {
                Value::String(format!("str_{}", rng.gen_ascii_word(6)))
            } else {
                match rng.next_usize(3) {
                    0 => Value::Bool(rng.next_bool()),
                    1 => Value::Number(serde_json::Number::from(rng.next_u32() % 100)),
                    _ => Value::String(rng.gen_ascii_word(8)),
                }
            }
        }
        ParamType::Array => {
            let count = if for_argv {
                1 + rng.next_usize(2)
            } else {
                rng.next_usize(3)
            };
            Value::Array((0..count).map(|_| Value::String(rng.gen_ascii_word(5))).collect())
        }
    }
}

// ---------------------------------------------------------------------------
// 1. encode -> parse round-trip for every fixture op under random values
// ---------------------------------------------------------------------------
#[test]
fn prop_encode_parse_roundtrip_fixture_ops() {
    let fixtures = load_fixtures();
    assert!(!fixtures.is_empty(), "fixtures must be available");
    let mut rng = Rng::new(0x12345678_abcdef01);

    let mut json_trips = 0;
    let mut graphql_trips = 0;
    let mut argv_trips = 0;

    for doc in &fixtures {
        for op in &doc.operations {
            // Test 10 random parameter sets per op
            for _ in 0..10 {
                // Generate values for a subset of params
                let mut params = Record::new();
                for param in &op.params {
                    // If required or random coin flip
                    let is_required = param.occurs == Some(Occurs::One)
                        || param.occurs == Some(Occurs::OneOrMore)
                        || param.occurs.is_none();
                    if is_required || rng.next_bool() {
                        let val = gen_param_value(&mut rng, param, false, false);
                        params.insert(param.name.clone(), val);
                    }
                }

                // 1a. JSON round-trip
                let json_text = encode_json(op, &params);
                let parsed_json = parse_json_document(&json_text)
                    .unwrap_or_else(|e| panic!("Failed to parse encoded JSON for {}: {e:?}", op.name));
                assert_eq!(parsed_json.op.as_deref(), Some(op.name.as_str()));
                assert!(
                    deep_eq(&Value::Object(params.clone()), &Value::Object(parsed_json.params)),
                    "JSON mismatch for {}: params={:?}",
                    op.name,
                    params
                );
                json_trips += 1;

                // 1b. GraphQL round-trip (when op and params have GraphQL-valid names)
                let gql_valid_op = op.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                let gql_valid_params = params
                    .keys()
                    .all(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));

                if gql_valid_op && gql_valid_params {
                    // Filter params suitable for GraphQL
                    let mut gql_params = Record::new();
                    for param in &op.params {
                        if params.contains_key(&param.name) {
                            let val = gen_param_value(&mut rng, param, false, true);
                            gql_params.insert(param.name.clone(), val);
                        }
                    }
                    if let Ok(gql_text) = encode_graphql(op, &gql_params) {
                        let parsed_gql = parse_graphql_document(&gql_text)
                            .unwrap_or_else(|e| panic!("Failed to parse encoded GraphQL: {e:?} in {gql_text}"));
                        assert_eq!(parsed_gql.op.as_deref(), Some(op.name.as_str()));
                        assert!(
                            deep_eq(&Value::Object(gql_params.clone()), &Value::Object(parsed_gql.params)),
                            "GraphQL mismatch for {}",
                            op.name
                        );
                        graphql_trips += 1;
                    }
                }

                // 1c. Argv round-trip
                // Build params strictly obeying argv limitations (no empty arrays, non-literal json strings)
                let mut argv_params = Record::new();
                let mut seen_flags = std::collections::HashSet::new();
                let has_no_variant = |name: &str| -> bool {
                    let mut chars = name.chars();
                    match chars.next() {
                        Some(first) => {
                            let capitalized: String = first.to_uppercase().chain(chars).collect();
                            let no_name = format!("no{capitalized}");
                            op.params.iter().any(|p| p.name == no_name)
                        }
                        None => false,
                    }
                };

                for param in &op.params {
                    if param.doc_only == Some(true) || param.cli_only == Some(true) {
                        continue;
                    }
                    if params.contains_key(&param.name) {
                        let val = gen_param_value(&mut rng, param, true, false);
                        if val == Value::Bool(false) && has_no_variant(&param.name) {
                            continue;
                        }
                        let flag_key = match (&val, param.kind) {
                            (Value::Bool(false), ParamType::Boolean | ParamType::Json) => {
                                format!("no-{}", to_kebab(&param.name))
                            }
                            _ => to_kebab(&param.name),
                        };
                        if seen_flags.insert(flag_key) {
                            argv_params.insert(param.name.clone(), val);
                        }
                    }
                }
                let argv = encode_argv(op, &argv_params, EncodeArgvOptions::default());
                let parsed_argv = parse_argv_form(&argv, op, &[]).unwrap_or_else(|e| {
                    panic!("Failed to parse encoded argv for {}: {argv:?}, issues: {e:?}", op.name)
                });
                assert!(
                    deep_eq(
                        &Value::Object(argv_params.clone()),
                        &Value::Object(parsed_argv.params.clone())
                    ),
                    "Argv mismatch for {}: argv={:?}, expected={:?}, got={:?}",
                    op.name,
                    argv,
                    argv_params,
                    parsed_argv.params
                );
                argv_trips += 1;
            }
        }
    }

    assert!(json_trips >= 50, "json round trips: {json_trips}");
    assert!(graphql_trips >= 30, "graphql round trips: {graphql_trips}");
    assert!(argv_trips >= 50, "argv round trips: {argv_trips}");
}

// ---------------------------------------------------------------------------
// 2. validate idempotence (validating already-validated output yields same output)
// ---------------------------------------------------------------------------
#[test]
fn prop_validate_idempotence() {
    let fixtures = load_fixtures();
    assert!(!fixtures.is_empty());
    let mut rng = Rng::new(0x23456789_bcdef012);

    let mut successful_validations = 0;

    for doc in &fixtures {
        for op in &doc.operations {
            for _ in 0..15 {
                let mut raw = Record::new();
                for param in &op.params {
                    let is_required = param.occurs == Some(Occurs::One)
                        || param.occurs == Some(Occurs::OneOrMore)
                        || param.occurs.is_none();
                    if is_required || rng.next_bool() {
                        let val = gen_param_value(&mut rng, param, false, false);
                        raw.insert(param.name.clone(), val);
                    }
                }

                let form = match rng.next_usize(4) {
                    0 => None,
                    1 => Some(InputForm::Flags),
                    2 => Some(InputForm::Json),
                    _ => Some(InputForm::Graphql),
                };
                let opts = ValidationOptions { form };

                if let Ok(v1) = validate(op, &raw, opts) {
                    // Validating already-validated record must succeed
                    let v2 = validate(op, &v1, opts)
                        .unwrap_or_else(|e| panic!("Idempotence failed on second validation of {}: {e:?}", op.name));
                    assert!(
                        deep_eq(&Value::Object(v1.clone()), &Value::Object(v2.clone())),
                        "Idempotence value divergence on {}: first={:?}, second={:?}",
                        op.name,
                        v1,
                        v2
                    );
                    successful_validations += 1;
                }
            }
        }
    }

    assert!(
        successful_validations >= 100,
        "successful validations: {successful_validations}"
    );
}

// ---------------------------------------------------------------------------
// 3. Tokenizer never panics on arbitrary bytes-as-strings (1e5 random inputs)
// ---------------------------------------------------------------------------
#[test]
fn prop_tokenizer_never_panics_100k() {
    let mut rng = Rng::new(0x3456789a_cdef0123);

    // Characters of all kinds: ASCII, symbols, quotes, escapes, unicode, control, null
    let char_pool: Vec<char> = "abcdef0123-_ \t\n\r\0'\"\\$|;&<>(){}[]*?~`#@!=+/,\u{00E9}\u{2603}\u{1F600}\u{FFFD}"
        .chars()
        .collect();

    let mut buf = String::with_capacity(64);
    for _ in 0..100_000 {
        buf.clear();
        let len = rng.next_usize(32);
        for _ in 0..len {
            buf.push(*rng.choose(&char_pool));
        }

        // Must not panic on any input
        let _ = tokenize_simple_command(&buf);
    }
}

// ---------------------------------------------------------------------------
// 4. parse_published_schema(publish(schema)) round trip
// ---------------------------------------------------------------------------
#[test]
fn prop_publish_schema_round_trip() {
    let mut rng = Rng::new(0x456789ab_def01234);

    for i in 0..100 {
        let tool = rng.gen_tool_or_op_name();
        let num_ops = rng.next_usize(4);

        let mut operations = Vec::new();
        for j in 0..num_ops {
            let op_name = format!("{}_{}", rng.gen_tool_or_op_name(), j);
            let mut op = OperationSchema::new(op_name);
            op.access = Some(match rng.next_usize(3) {
                0 => Access::Read,
                1 => Access::Write,
                _ => Access::Dynamic,
            });
            op.description = Some(format!("Description for op {j}"));

            let num_params = rng.next_usize(4);
            for p in 0..num_params {
                let p_name = format!("param{p}");
                let p_type = match rng.next_usize(6) {
                    0 => ParamType::String,
                    1 => ParamType::Number,
                    2 => ParamType::Boolean,
                    3 => ParamType::Object,
                    4 => ParamType::Array,
                    _ => ParamType::Json,
                };
                let mut spec = ParamSpec::new(p_name, p_type);
                spec.description = Some(format!("Help for param {p}"));
                spec.occurs = Some(match rng.next_usize(4) {
                    0 => Occurs::One,
                    1 => Occurs::Optional,
                    2 => Occurs::OneOrMore,
                    _ => Occurs::ZeroOrMore,
                });
                op.params.push(spec);
            }
            operations.push(op);
        }

        let opts = PublishOptions {
            version: Some(format!("1.{}.0", i % 10)),
            policy: Some(PolicyFlags {
                dryrun: rng.next_bool(),
                readonly: rng.next_bool(),
            }),
            inputs: Some(vec![InputForm::Flags, InputForm::Json, InputForm::Graphql]),
            globals: Some(vec![ParamSpec::new("dryRun", ParamType::Boolean).optional()]),
            tool_description: None,
            product: if rng.next_bool() {
                Some(ProductInfo {
                    name: "test-product".into(),
                    supported: ">=1.0.0 <2.0.0".into(),
                    detect: Some(ProductDetect {
                        argv: vec!["test-product".into(), "--version".into()],
                        pattern: r"test-product v(\d+\.\d+\.\d+)".into(),
                    }),
                })
            } else {
                None
            },
        };

        let published = publish_schema(&tool, operations, opts);
        let serialized = serde_json::to_value(&published).unwrap();

        let parsed = parse_published_schema(&serialized)
            .unwrap_or_else(|issues| panic!("Failed to parse published schema for {tool}: {issues:?}"));

        let reserialized = serde_json::to_value(&parsed).unwrap();
        assert!(
            deep_eq(&serialized, &reserialized),
            "Publish round trip divergence for {tool}:\nexpected: {}\ngot: {}",
            serialized,
            reserialized
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Server run_cli_io never panics on random argv (fuzz 1e4) and returns valid code
// ---------------------------------------------------------------------------
#[test]
fn prop_server_run_cli_io_never_panics_10k() {
    let mut rng = Rng::new(0x56789abc_ef012345);

    // Build a realistic CliSpec with various ops
    let spec = CliSpec::with_policy("fuzztool")
        .op(OpDef::new("status", Access::Read, "Show status", |params, _| {
            Ok(Value::Object(params.clone()))
        }))
        .op(OpDef::new("commit", Access::Write, "Create a commit", |params, _| {
            Ok(json!({ "committed": true, "params": params }))
        }))
        .op(OpDef::new("query", Access::Dynamic, "Dynamic query", |_, _| {
            Ok(json!({ "result": 42 }))
        }));

    let allowed_exit_codes: [i32; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 127];

    let token_pool = [
        "status",
        "commit",
        "query",
        "unknown_op",
        "--help",
        "-h",
        "--version",
        "--schema",
        "--list",
        "--explain",
        "--dryrun",
        "--dry-run",
        "--readonly",
        "--read-only",
        "--confirm",
        "--no-confirm",
        "--json",
        "--format",
        "text",
        "--input-json",
        "{}",
        "{\"message\":\"hello\"}",
        "invalid json",
        "--input-graphql",
        "query { status { __typename } }",
        "--input-file",
        "/tmp/nonexistent_file",
        "--message=hello",
        "--message",
        "world",
        "-m",
        "--",
        "--unknown-flag",
        "--flag=val=extra",
        "",
        "   ",
        "\"quoted string\"",
        "123",
        "true",
        "false",
        "null",
        "-",
        "---",
        ";",
        "|",
    ];

    let env = HashMap::new();

    for _ in 0..10_000 {
        let argc = rng.next_usize(6);
        let argv: Vec<String> = (0..argc).map(|_| rng.choose(&token_pool).to_string()).collect();

        let mut sink = Vec::new();
        let code = run_cli_io(&spec, &argv, &mut sink, &env);

        assert!(
            allowed_exit_codes.contains(&code),
            "run_cli_io returned unexpected exit code {code} for argv: {argv:?}"
        );
    }
}
