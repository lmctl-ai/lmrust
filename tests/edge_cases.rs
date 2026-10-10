//! lmrust-specific edge cases: error branches and API surface the ported lmts tests do not
//! reach (GraphQL parse errors, validator list branches, publish product checks, runner
//! document errors, `CliSpec::from_document`).

use lmrust::server::{
    check_policy, global_flags, run_cli_io, CheckContext, CliError, CliSpec, ErrorCode, OpDef, Policy,
};
use lmrust::*;
use serde_json::{json, Value};
use std::collections::HashMap;

fn rec(v: Value) -> Record {
    v.as_object().cloned().expect("object")
}

fn gql_err(text: &str) -> ValidationIssue {
    match parse_graphql_document(text) {
        Ok(doc) => panic!("expected an error for {text:?}, got {doc:?}"),
        Err(mut issues) => issues.remove(0),
    }
}

#[test]
fn graphql_string_escapes() {
    let doc = parse_graphql_document(r#"{ op(a: "q\"b\\s\/n\nt\tr\rb\bf\f") }"#).unwrap();
    assert_eq!(doc.params["a"], json!("q\"b\\s/n\nt\tr\rb\u{8}f\u{c}"));
    let doc = parse_graphql_document(r#"{ op(a: "Aé😀") }"#).unwrap();
    assert_eq!(doc.params["a"], json!("Aé😀"));
    // parseInt-style lenient hex: leading hex digits of the four characters, all six consumed.
    let doc = parse_graphql_document(r#"{ op(a: "\u41zz") }"#).unwrap();
    assert_eq!(doc.params["a"], json!("A"));
    // A lone surrogate has no UTF-8 form: replacement character.
    let doc = parse_graphql_document(r#"{ op(a: "\ud800x") }"#).unwrap();
    assert_eq!(doc.params["a"], json!("\u{FFFD}x"));
    let doc = parse_graphql_document(r#"{ op(a: "\ud800A") }"#).unwrap();
    assert_eq!(doc.params["a"], json!("\u{FFFD}A"));
    assert!(gql_err(r#"{ op(a: "\uzz") }"#).message.contains("invalid \\u escape"));
    assert!(gql_err(r#"{ op(a: "\q") }"#).message.contains("unknown escape \\q"));
    assert!(gql_err(r#"{ op(a: "abc"#)
        .message
        .contains("unterminated string literal"));
}

#[test]
fn graphql_numbers_arrays_objects() {
    let doc = parse_graphql_document("{ op(a: -1.5e+2, b: 3e, c: [], d: {}, e: [1, 2], f: {x: 1, y: [true]}) }");
    // `3e` lexes as 3 then `e` is an enum-like bare word that is not a separator: error.
    assert!(doc.is_err());
    let doc = parse_graphql_document("{ op(a: -1.5e+2, c: [], d: {}, e: [1, 2], f: {x: 1, y: [true]}, g: 2.) }");
    assert!(doc.is_err());
    let doc =
        parse_graphql_document("{ op(a: -1.5e+2, c: [], d: {}, e: [1, 2], f: {x: 1, y: [true]}, g: 7) }").unwrap();
    assert_eq!(doc.params["a"], json!(-150));
    assert_eq!(doc.params["c"], json!([]));
    assert_eq!(doc.params["d"], json!({}));
    assert_eq!(doc.params["e"], json!([1, 2]));
    assert_eq!(doc.params["f"], json!({"x": 1, "y": [true]}));
    assert!(gql_err("{ op(a: -x) }").message.contains("invalid number"));
    assert!(gql_err("{ op(a: 1e999) }").message.contains("invalid number"));
    assert!(gql_err("{ op(a: [1 2]) }").message.contains("expected , or ] in array"));
    assert!(gql_err("{ op(a: [1,").message.contains("unterminated array"));
    assert!(gql_err("{ op(a: {$v: 1}) }").message.contains("variables"));
    assert!(gql_err("{ op(a: {1: 1}) }").message.contains("expected object key"));
    assert!(gql_err("{ op(a: {k 1}) }")
        .message
        .contains("expected : after object key"));
    assert!(gql_err("{ op(a: {k: 1 j: 2}) }")
        .message
        .contains("expected , or } in object"));
    assert!(gql_err("{ op(a: {k: 1,").message.contains("unterminated object"));
    assert!(gql_err("{ op(a: @) }").message.contains("unexpected value at offset"));
}

#[test]
fn graphql_document_structure_errors() {
    let e = gql_err("op(a: 1)");
    assert_eq!(e.code, IssueCode::DocumentParse);
    assert!(e.message.contains("variables and operation names"));
    assert!(gql_err("$x").message.contains("variables"));
    assert!(gql_err("(").message.contains("expected { at offset 0"));
    assert!(gql_err("{ $x }").message.contains("variables"));
    assert!(gql_err("{ 1 }").message.contains("expected operation field name"));
    assert!(gql_err("{ op($a: 1) }").message.contains("variables"));
    assert!(gql_err("{ op(1: 1) }").message.contains("expected argument name"));
    let e = gql_err("{ op(a 1) }");
    assert!(e.message.contains("expected : after argument name \"a\""));
    assert_eq!(e.op.as_deref(), Some("op"));
    assert!(gql_err("{ op(a: 1").message.contains("expected ) after arguments"));
    assert!(gql_err("{ op { x { y }").message.contains("unbalanced braces"));
    assert!(gql_err("{ op").message.contains("unclosed operation"));
    // Selection sets skip strings, escapes and comments.
    let doc = parse_graphql_document("{ op(a: 1) { x(s: \"}\\\"\") # }\n y } }").unwrap();
    assert_eq!(doc.op.as_deref(), Some("op"));
    // query keyword followed by a word character is not the keyword.
    assert!(gql_err("queryx { op }").message.contains("variables"));
    let err = parse_graphql_input("{ op").unwrap_err();
    assert_eq!(err.details["code"], json!("DOCUMENT_PARSE"));
    assert_eq!(err.details["offset"], json!(4));
}

#[test]
fn validate_list_branches() {
    let op = OperationSchema::new("op")
        .param(
            ParamSpec::new("tags", ParamType::Array)
                .items(ItemType::Number)
                .optional(),
        )
        .param(
            ParamSpec::new("modes", ParamType::String)
                .occurs(Occurs::ZeroOrMore)
                .with_enum([json!("a"), json!("b")]),
        )
        .param(ParamSpec::new("objs", ParamType::Object).occurs(Occurs::ZeroOrMore))
        .param(
            ParamSpec::new("flags", ParamType::Array)
                .items(ItemType::Boolean)
                .optional(),
        );
    let ok = validate(
        &op,
        &rec(json!({"tags": "[1, 2]", "modes": ["a"], "objs": [{"k": 1}], "flags": "true"})),
        ValidationOptions::default(),
    )
    .unwrap();
    assert_eq!(ok["tags"], json!([1, 2]));
    assert_eq!(ok["flags"], json!([true]));
    let err = validate(&op, &rec(json!({"tags": {"x": 1}})), ValidationOptions::default()).unwrap_err();
    assert_eq!(err[0].code, IssueCode::TypeMismatch);
    assert!(err[0].message.contains("expected array"));
    let err = validate(&op, &rec(json!({"tags": ["x"]})), ValidationOptions::default()).unwrap_err();
    assert!(err[0].message.contains("expected number item"));
    let err = validate(&op, &rec(json!({"modes": ["a", "c"]})), ValidationOptions::default()).unwrap_err();
    assert_eq!(err[0].code, IssueCode::EnumViolation);
    assert_eq!(err[0].allowed, Some(vec![json!("a"), json!("b")]));
    let err = validate(&op, &rec(json!({"objs": [[1]]})), ValidationOptions::default()).unwrap_err();
    assert_eq!(err[0].code, IssueCode::TypeMismatch);
    let err = validate(
        &op,
        &rec(json!({"objs": ["{\"a\":1}", "nope"]})),
        ValidationOptions::default(),
    )
    .unwrap_err();
    assert_eq!(err[0].received, Some(json!("nope")));
    assert_eq!(to_array_value(&json!(null)), None);
    assert_eq!(to_array_value(&json!(3)), Some(vec![json!(3)]));
    assert_eq!(to_array_value(&json!("[1,")), Some(vec![json!("[1,")]));
}

#[test]
fn validate_scalar_coercion_failures() {
    let op = OperationSchema::new("op")
        .param(ParamSpec::new("b", ParamType::Boolean).optional())
        .param(ParamSpec::new("n", ParamType::Number).optional())
        .param(ParamSpec::new("s", ParamType::String).optional())
        .param(ParamSpec::new("o", ParamType::Object).optional());
    for (k, v) in [
        ("b", json!(1)),
        ("n", json!(true)),
        ("n", json!("1e999")),
        ("s", json!([])),
        ("o", json!(5)),
        ("o", json!("[1]")),
    ] {
        let err = validate(&op, &rec(json!({ k: v })), ValidationOptions::default()).unwrap_err();
        assert_eq!(err[0].code, IssueCode::TypeMismatch, "{k}={v}");
    }
    let ok = validate(
        &op,
        &rec(json!({"s": 1.5, "n": "  -2  ".trim()})),
        ValidationOptions::default(),
    )
    .unwrap();
    assert_eq!(ok["s"], json!("1.5"));
    assert_eq!(ok["n"], json!(-2));
}

#[test]
fn schema_validation_error_surface() {
    let op = OperationSchema::new("op").param(ParamSpec::new("a", ParamType::String));
    let err = validate_params(&op, &Record::new()).unwrap_err();
    assert_eq!(err.to_string(), err.message);
    assert_eq!(err.issues().len(), 1);
    assert_eq!(err.details["spec"], json!("a"));
    assert_eq!(err.details["known"], json!(["a"]));
    let plain = SchemaValidationError::new("op", "m", Record::new());
    assert!(plain.issues().is_empty());
    let issue = ValidationIssue::new(IssueCode::MixedInput, "msg", "s", "u");
    assert_eq!(issue.to_string(), "MIXED_INPUT: msg");
    assert_eq!(IssueCode::OpMismatch.to_string(), "OP_MISMATCH");
}

#[test]
fn type_names() {
    assert_eq!(ParamType::Array.to_string(), "array");
    assert_eq!(ItemType::Boolean.to_string(), "boolean");
    assert_eq!(ItemType::Object.as_param_type(), ParamType::Object);
    assert_eq!(ItemType::Boolean.as_param_type(), ParamType::Boolean);
    assert_eq!(
        [Occurs::One, Occurs::Optional, Occurs::OneOrMore, Occurs::ZeroOrMore].map(Occurs::as_str),
        ["1", "?", "+", "*"]
    );
    assert_eq!(InputForm::Flags.as_str(), "flags");
    assert_eq!(Access::Dynamic.to_string(), "dynamic");
    assert_eq!(Effect::Read.as_str(), "read");
    let p: ParamSpec = serde_json::from_value(json!({"name": "x", "type": "json", "default": null})).unwrap();
    assert_eq!(p.default, Some(Value::Null));
}

#[test]
fn encode_edge_cases() {
    let op = OperationSchema::new("op")
        .param(
            ParamSpec::new("files", ParamType::String)
                .occurs(Occurs::ZeroOrMore)
                .positional(),
        )
        .param(ParamSpec::new("dir", ParamType::String).positional());
    let argv = encode_argv(
        &op,
        &rec(json!({"files": ["a", "b"], "dir": "d"})),
        EncodeArgvOptions { positional: true },
    );
    assert_eq!(argv, ["--", "a", "b", "d"]);
    let bad = OperationSchema::new("bad-name");
    assert_eq!(
        encode_graphql(&bad, &Record::new()).unwrap_err().to_string(),
        "op name bad-name is not a GraphQL name"
    );
    let ok = OperationSchema::new("op");
    let err = encode_graphql(&ok, &rec(json!({"bad-key": 1}))).unwrap_err();
    assert_eq!(err.0, "param bad-key is not a GraphQL name");
    assert_eq!(
        encode_graphql(&ok, &Record::new()).unwrap(),
        "query { op { __typename } }"
    );
}

#[test]
fn publish_product_and_param_edges() {
    let base = json!({"schema": "cli.schema.v1", "tool": "t", "operations": []});
    let with = |key: &str, v: Value| {
        let mut d = base.clone();
        d[key] = v;
        parse_published_schema(&d)
    };
    let codes = |r: Outcome<PublishedCliSchema>| {
        r.unwrap_err()
            .iter()
            .map(|i| i.code.as_str().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        codes(with("product", json!({"name": 1, "supported": 2}))),
        ["TYPE_MISMATCH", "TYPE_MISMATCH"]
    );
    assert_eq!(
        codes(with(
            "product",
            json!({"name": "git", "supported": ">=1.0.0", "detect": {"argv": ["git"], "pattern": 3, "x": 1}})
        )),
        ["UNKNOWN_PARAM", "TYPE_MISMATCH"]
    );
    assert_eq!(
        codes(with(
            "product",
            json!({"name": "git", "supported": ">=1.0.0", "detect": {"argv": ["git"], "pattern": "(?=x)(\\d)"}})
        )),
        ["DOCUMENT_PARSE"]
    );
    // An array default must hold elements of the declared element type.
    let op = |p: Value| json!({"schema": "cli.schema.v1", "tool": "t", "operations": [{"name": "o", "params": [p]}]});
    assert!(parse_published_schema(&op(
        json!({"name": "a", "type": "array", "items": "number", "default": [1, 2]})
    ))
    .is_ok());
    assert!(parse_published_schema(&op(json!({"name": "a", "type": "array", "default": ["x", 1]}))).is_err());
    assert!(parse_published_schema(&op(
        json!({"name": "a", "type": "string", "occurs": "*", "default": "x"})
    ))
    .is_err());
    // Globals use the wider name pattern (dashes allowed); a bad global name is DOCUMENT_PARSE.
    let mut g = base.clone();
    g["globals"] = json!([{"name": "dry-run", "type": "boolean"}, {"name": "-bad", "type": "boolean"}]);
    assert_eq!(codes(parse_published_schema(&g)), ["DOCUMENT_PARSE"]);
    // lmts does not type-check op descriptions; the typed lmrust document does (reported, not panicking).
    let loose =
        json!({"schema": "cli.schema.v1", "tool": "t", "operations": [{"name": "o", "params": [], "description": 5}]});
    let issues = parse_published_schema(&loose).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::TypeMismatch);
    let err = parse_published_schema_or_err(&json!([])).unwrap_err();
    assert_eq!(err.details["code"], json!("DOCUMENT_PARSE"));
    let mut doc = publish_schema("t", vec![OperationSchema::new("o")], PublishOptions::default());
    assert_eq!(schema_warnings(&mut doc), ["operation \"o\" is missing a description"]);
    assert!(find_operation(&doc, "o").is_some());
    assert!(find_operation(&doc, "x").is_none());
}

#[test]
fn version_edges() {
    assert!(parse_version("2").is_none());
    assert_eq!(parse_version("2.43.").unwrap().patch, 0);
    assert!(!satisfies("x", ">=1.0.0"));
    assert!(!is_valid_supported_range(">=1.0"));
    assert_eq!(count_regexp_groups(&"a".repeat(201)), None);
}

#[test]
fn interpret_edges() {
    let issues = interpret_command(&[], "git2 status", &[]).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::UnknownTool);
    assert_eq!(issues[0].usage, "<tool> <operation> [flags]");
    let doc: PublishedCliSchema = serde_json::from_value(json!({
        "schema": "cli.schema.v1", "tool": "t",
        "operations": [{"name": "o", "access": "read", "params": [
            {"name": "n", "type": "number"},
            {"name": "files", "type": "string", "occurs": "*", "positional": true}
        ]}]
    }))
    .unwrap();
    // Both an argv issue and a validation issue are reported.
    let issues = interpret_command(std::slice::from_ref(&doc), "t o --n abc", &[]).unwrap_err();
    assert!(issues.iter().any(|i| i.code == IssueCode::TypeMismatch));
    assert!(issues.iter().any(|i| i.code == IssueCode::MissingRequired));
    // An argv issue alone (validation passes) still fails.
    let issues = interpret_command(std::slice::from_ref(&doc), "t o --n 1 --n 2", &[]).unwrap_err();
    assert_eq!(issues[0].code, IssueCode::DuplicateFlag);
    let prompt = describe_for_prompt(&[doc]);
    assert!(prompt.contains("t o [files]... --n <number> (read)"), "{prompt}");
}

fn spec_from_doc() -> CliSpec<Policy> {
    let doc: PublishedCliSchema = serde_json::from_value(json!({
        "schema": "cli.schema.v1", "tool": "docs-tool", "version": "1.2.3",
        "operations": [
            {"name": "ping", "access": "read", "description": "Ping", "params": [{"name": "n", "type": "number", "occurs": "?"}]},
            {"name": "wipe", "params": []},
            {"name": "orphan", "params": []}
        ]
    }))
    .unwrap();
    CliSpec::from_document_with_policy(&doc)
        .handle("ping", |p, _| {
            Ok(json!({"pong": p.get("n").cloned().unwrap_or(Value::Null)}))
        })
        .handle("wipe", |_, policy: &Policy| {
            match check_policy(Access::Write, policy, &CheckContext::new("wipe").dangerous(true))? {
                Some(sim) => Ok(Value::Object(sim)),
                None => Ok(json!({"wiped": true})),
            }
        })
        .handle("extra", |_, _| Err(CliError::new(ErrorCode::Conflict, "nope")))
}

fn run(spec: &CliSpec<Policy>, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
    let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let env: HashMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let mut out = Vec::new();
    let code = run_cli_io(spec, &argv, &mut out, &env);
    (code, String::from_utf8(out).unwrap())
}

fn envelope(out: &str) -> Value {
    serde_json::from_str(out).unwrap()
}

#[test]
fn cli_spec_from_document() {
    let spec = spec_from_doc();
    let (code, out) = run(&spec, &["ping", "--n", "3"], &[]);
    assert_eq!((code, envelope(&out)), (0, json!({"pong": 3})));
    let (code, out) = run(&spec, &["--schema"], &[]);
    assert_eq!(code, 0);
    let schema = envelope(&out);
    assert_eq!(schema["version"], json!("1.2.3"));
    assert_eq!(schema["operations"][1]["name"], json!("wipe"));
    // Missing access defaults to write: dangerous op needs confirmation.
    let (code, out) = run(&spec, &["wipe"], &[]);
    assert_eq!(
        (code, envelope(&out)["error"]["code"].clone()),
        (9, json!("MISSING_CONFIRMATION"))
    );
    let (code, _) = run(&spec, &["wipe", "--confirm"], &[]);
    assert_eq!(code, 0);
    let (code, out) = run(&spec, &["--dry-run", "wipe"], &[]);
    assert_eq!((code, envelope(&out)["simulated"].clone()), (0, json!("wipe")));
    let (code, out) = run(&spec, &["orphan"], &[]);
    assert_eq!(
        (code, envelope(&out)["error"]["code"].clone()),
        (1, json!("INTERNAL_ERROR"))
    );
    let (code, out) = run(&spec, &["extra"], &[]);
    assert_eq!((code, envelope(&out)["error"]["message"].clone()), (5, json!("nope")));
    // The env floor uses the upper-cased tool name with non-alphanumerics as underscores.
    let (code, _) = run(&spec, &["wipe", "--confirm"], &[("DOCS_TOOL_READONLY", "yes")]);
    assert_eq!(code, 6);
}

#[test]
fn cli_document_error_paths() {
    let spec = spec_from_doc();
    // Op given on the command line, document names a different op.
    let (code, out) = run(&spec, &["ping", "--input-json", r#"{"op": "wipe", "params": {}}"#], &[]);
    let env = envelope(&out);
    assert_eq!(code, 2);
    assert_eq!(env["error"]["data"]["issues"][0]["code"], json!("DOCUMENT_PARSE"));
    assert!(env["error"]["message"]
        .as_str()
        .unwrap()
        .contains("does not match invoked op"));
    // Both document forms at once.
    let (code, out) = run(
        &spec,
        &["ping", "--input-json", "{}", "--input-graphql", "{ ping }"],
        &[],
    );
    assert_eq!(code, 2);
    assert!(envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .contains("cannot mix --input-json and --input-graphql"));
    // Mixed graphql document and op flags.
    let (code, out) = run(&spec, &["ping", "--n", "1", "--input-graphql", "{ ping }"], &[]);
    assert_eq!(code, 2);
    assert!(envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--input-graphql"));
    // An argv that does not parse also loses its globals, so the document is ignored and the
    // flags error is reported (lmts behaves the same; its "invalid arguments" branch is unreachable).
    let (code, out) = run(&spec, &["ping", "--input-json", "{}", "--dryrun=maybe"], &[]);
    assert_eq!(code, 2);
    assert_eq!(
        envelope(&out)["error"]["data"]["issues"][0]["code"],
        json!("TYPE_MISMATCH")
    );
    // `--` makes the next token the op; a trailing `--` names none.
    let (code, out) = run(&spec, &["--dryrun", "--", "ping"], &[]);
    assert_eq!((code, envelope(&out)), (0, json!({"pong": null})));
    let (code, _) = run(&spec, &["--"], &[]);
    assert_eq!(code, 2);
    // `--explain=x` is not the `--explain <op>` form: no op is found.
    let (code, out) = run(&spec, &["--explain=ping"], &[]);
    assert_eq!(
        (code, envelope(&out)["error"]["message"].clone()),
        (2, json!("No operation specified"))
    );
    let (code, out) = run(&spec, &["--explain", "ping"], &[]);
    assert_eq!((code, envelope(&out)["name"].clone()), (0, json!("ping")));
    let (code, out) = run(&spec, &["--explain", "ping", "--format", "text"], &[]);
    assert_eq!(code, 0);
    assert!(out.starts_with("docs-tool ping\n"));
    assert_eq!(global_flags().len(), 12);
}

#[test]
fn cli_handler_panic_is_unknown_error() {
    let spec = CliSpec::with_policy("boom")
        .op(OpDef::new("str", Access::Read, "", |_, _| panic!("static message")))
        .op(OpDef::new("owned", Access::Read, "", |_, _| {
            panic!("{}", String::from("owned message"))
        }))
        .op(OpDef::new("other", Access::Read, "", |_, _| {
            std::panic::panic_any(42u8)
        }));
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let results: Vec<(i32, Value)> = ["str", "owned", "other"]
        .iter()
        .map(|op| {
            let (code, out) = run(&spec, &[op], &[]);
            (code, envelope(&out)["error"].clone())
        })
        .collect();
    std::panic::set_hook(hook);
    assert_eq!(
        results[0],
        (1, json!({"code": "UNKNOWN_ERROR", "message": "static message"}))
    );
    assert_eq!(results[1].1["message"], json!("owned message"));
    assert_eq!(results[2].1["message"], json!("Unknown error"));
}

#[test]
fn cli_error_conversions() {
    let io = std::io::Error::other("disk");
    assert_eq!(CliError::from(io).message, "disk");
    let json_err = serde_json::from_str::<Value>("{").unwrap_err();
    assert_eq!(CliError::from(json_err).code, ErrorCode::UnknownError);
    assert_eq!(CliError::from("").message, "Unknown error");
    assert_eq!(CliError::from(String::from("x")).to_string(), "UNKNOWN_ERROR: x");
    assert_eq!(ErrorCode::parse("TIMEOUT"), Some(ErrorCode::Timeout));
    assert_eq!(ErrorCode::parse("NOPE"), None);
    let p = CheckContext::new("op").command(["a"]).data(rec(json!({"k": 1})));
    let err = check_policy(
        Access::Write,
        &Policy {
            read_only: true,
            ..Policy::default()
        },
        &p,
    )
    .unwrap_err();
    assert_eq!(err.data.unwrap()["command"], json!(["a"]));
    let ok = check_policy(
        Access::Write,
        &Policy {
            dry_run: true,
            ..Policy::default()
        },
        &CheckContext::new("op"),
    )
    .unwrap();
    assert!(!ok.unwrap().contains_key("command"));
}

#[test]
fn array_param_enum_applies_to_each_default_item() {
    let doc = |param: Value| {
        json!({"schema": "cli.schema.v1", "tool": "probe", "policy": {"dryrun": true, "readonly": true},
               "inputs": ["flags", "json"], "operations": [{"name": "find", "access": "read", "params": [param]}]})
    };
    let cols = json!(["name", "lastmod", "size", "type"]);
    // Valid: every item of an array default is in the enum (occurs * / + and type array).
    for ok in [
        json!({"name": "display", "type": "string", "occurs": "*", "enum": cols, "default": ["name", "size"]}),
        json!({"name": "display", "type": "string", "occurs": "+", "enum": cols, "default": ["size"]}),
        json!({"name": "display", "type": "array", "items": "string", "enum": cols, "default": []}),
    ] {
        assert!(parse_published_schema(&doc(ok.clone())).is_ok(), "{ok}");
    }
    // Invalid: an item outside the enum is TYPE_MISMATCH, like a scalar default outside its enum.
    for bad in [
        json!({"name": "display", "type": "string", "occurs": "*", "enum": cols, "default": ["name", "owner"]}),
        json!({"name": "display", "type": "array", "items": "string", "enum": cols, "default": ["color"]}),
        json!({"name": "display", "type": "string", "occurs": "?", "enum": cols, "default": "owner"}),
        json!({"name": "display", "type": "string", "occurs": "*", "enum": cols, "default": "name"}),
    ] {
        let issues = parse_published_schema(&doc(bad.clone())).unwrap_err();
        let codes: Vec<IssueCode> = issues.iter().map(|i| i.code).collect();
        assert_eq!(codes, [IssueCode::TypeMismatch], "{bad}");
    }
}
