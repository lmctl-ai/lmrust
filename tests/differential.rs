//! Differential tests: comparing lmrust behavior with the reference TypeScript SDK (`@lmctl-ai/lmts`).
//!
//! Generates >= 2000 deterministic pseudo-random cases across:
//! 1. `validate`
//! 2. `parse_argv_form`
//! 3. `json_form` and `graphql_form`
//! 4. `encode_argv`, `encode_json`, `encode_graphql` round-trips
//! 5. `interpret_command`
//!
//! Compares canonical JSON results (for issues: compares code and param only).
//! Any divergence is captured as a minimal reproduction and marked `#[ignore = "divergence: ..."]`.

use lmrust::js::deep_eq;
use lmrust::{
    encode_argv, encode_graphql, encode_json, interpret_command, parse_argv_form, parse_graphql_document,
    parse_json_document, parse_published_schema, to_kebab, validate, EncodeArgvOptions, InputForm, OperationSchema,
    ParamSpec, ParamType, PublishedCliSchema, Record, ValidationIssue, ValidationOptions,
};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, LineWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

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
        let len = 1 + self.next_usize(10);
        let mut s = String::with_capacity(len);
        s.push(*self.choose(first) as char);
        for _ in 1..len {
            s.push(*self.choose(rest) as char);
        }
        s
    }

    pub fn gen_word(&mut self, max_len: usize) -> String {
        let chars = b"abcdefghijklmnopqrstuvwxyz0123456789_-";
        let len = 1 + self.next_usize(max_len.max(1));
        (0..len).map(|_| *self.choose(chars) as char).collect()
    }
}

/// Node oracle child process wrapper.
struct Oracle {
    child: Child,
    reader: BufReader<ChildStdout>,
    writer: LineWriter<ChildStdin>,
    next_id: u64,
}

impl Oracle {
    fn spawn(lmts_dir: &Path) -> Result<Self, String> {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/lmts-oracle.mjs");
        if !script.is_file() {
            return Err(format!("Oracle script not found at {}", script.display()));
        }

        let mut child = Command::new("node")
            .arg(&script)
            .env("LMTS_DIR", lmts_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Failed to spawn node: {e}"))?;

        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let stdin = child.stdin.take().ok_or("Failed to capture stdin")?;

        let mut oracle = Self {
            child,
            reader: BufReader::new(stdout),
            writer: LineWriter::new(stdin),
            next_id: 1,
        };

        // Handshake
        let pong = oracle.call("ping", json!({}))?;
        if pong.get("pong") != Some(&Value::Bool(true)) {
            return Err("Oracle ping failed".into());
        }

        Ok(oracle)
    }

    fn call(&mut self, op: &str, payload: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({ "id": id, "op": op, "payload": payload });
        let req_str = serde_json::to_string(&req).map_err(|e| e.to_string())?;

        self.writer
            .write_all(req_str.as_bytes())
            .map_err(|e| format!("Write to oracle failed: {e}"))?;
        self.writer
            .write_all(b"\n")
            .map_err(|e| format!("Write newline failed: {e}"))?;
        self.writer.flush().map_err(|e| format!("Flush failed: {e}"))?;

        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .map_err(|e| format!("Read from oracle failed: {e}"))?;
        if line.trim().is_empty() {
            return Err("Oracle process closed stdout unexpectedly".into());
        }

        let resp: Value =
            serde_json::from_str(&line).map_err(|e| format!("Bad JSON response from oracle: {e} ({line})"))?;

        if let Some(panic_msg) = resp.get("panic").and_then(Value::as_str) {
            return Err(format!("Oracle panicked: {panic_msg}"));
        }

        Ok(resp)
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn locate_lmts_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("LMTS_DIR") {
        let p = PathBuf::from(dir);
        if p.join("dist").exists() {
            return Some(p);
        } else {
            return None;
        }
    }
    let sibling = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("lmts");
    if sibling.join("dist").exists() {
        return Some(sibling);
    }
    None
}

fn node_is_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn setup_oracle_or_skip() -> Option<Oracle> {
    let lmts_dir = locate_lmts_dir();
    let node_ok = node_is_available();

    if lmts_dir.is_none() || !node_ok {
        if std::env::var_os("LMRUST_REQUIRE_DIFFERENTIAL").is_some() {
            panic!(
                "Differential tests FAIL in CI: LMTS_DIR={:?}, node_available={}",
                lmts_dir, node_ok
            );
        }
        eprintln!(
            "SKIPPED: differential tests skipped because LMTS_DIR or node is missing (LMTS_DIR={:?}, node={})",
            lmts_dir, node_ok
        );
        return None;
    }

    match Oracle::spawn(&lmts_dir.unwrap()) {
        Ok(oracle) => Some(oracle),
        Err(e) => {
            if std::env::var_os("LMRUST_REQUIRE_DIFFERENTIAL").is_some() {
                panic!("Differential tests FAIL in CI: {e}");
            }
            eprintln!("SKIPPED: differential tests skipped due to oracle initialization failure: {e}");
            None
        }
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

/// Normalize issue lists for differential comparison: sort by (param, code).
fn extract_issues(val: &Value) -> Vec<(String, Option<String>)> {
    let mut list = Vec::new();
    if let Some(issues) = val.get("issues").and_then(Value::as_array) {
        for issue in issues {
            let code = issue["code"].as_str().unwrap_or_default().to_string();
            let param = issue.get("param").and_then(Value::as_str).map(String::from);
            list.push((code, param));
        }
    }
    list.sort();
    list
}

fn extract_rust_issues(issues: &[ValidationIssue]) -> Vec<(String, Option<String>)> {
    let mut list: Vec<(String, Option<String>)> = issues
        .iter()
        .map(|i| (i.code.as_str().to_string(), i.param.clone()))
        .collect();
    list.sort();
    list
}

// ---------------------------------------------------------------------------
// 1. Differential: validate
// ---------------------------------------------------------------------------
#[test]
fn diff_validate_suite() {
    let Some(mut oracle) = setup_oracle_or_skip() else {
        return;
    };
    let fixtures = load_fixtures();
    assert!(!fixtures.is_empty());
    let mut rng = Rng::new(0x11112222_33334444);

    let mut case_count = 0;

    for doc in &fixtures {
        for op in &doc.operations {
            // Generate multiple validation test cases per op (both valid and invalid)
            for _ in 0..20 {
                let mut raw = Record::new();
                for param in &op.params {
                    // Random strategy: 0: correct, 1: wrong type, 2: missing, 3: enum violation, 4: null
                    match rng.next_usize(5) {
                        0 => {
                            // Valid value
                            let val = match param.kind {
                                ParamType::String => Value::String(rng.gen_word(8)),
                                ParamType::Number => {
                                    Value::Number(serde_json::Number::from((rng.next_u32() % 200) as i64))
                                }
                                ParamType::Boolean => Value::Bool(rng.next_bool()),
                                ParamType::Object => {
                                    let mut m = Record::new();
                                    m.insert("k".into(), Value::String("v".into()));
                                    Value::Object(m)
                                }
                                ParamType::Array => Value::Array(vec![Value::String("item".into())]),
                                ParamType::Json => json!({"nested": 123}),
                            };
                            raw.insert(param.name.clone(), val);
                        }
                        1 => {
                            // Wrong type: send object where string expected, or string where array expected
                            let wrong = match param.kind {
                                ParamType::String => json!({"not": "a string"}),
                                ParamType::Number => json!({"not": "a number"}),
                                ParamType::Boolean => json!({"not": "a bool"}),
                                ParamType::Array => json!({"not": "an array"}),
                                ParamType::Object => Value::String("not an object".into()),
                                ParamType::Json => json!(true),
                            };
                            raw.insert(param.name.clone(), wrong);
                        }
                        2 => {
                            // Intentionally omitted (tests MISSING_REQUIRED if required)
                        }
                        3 => {
                            // Enum violation if param has enum
                            if let Some(ref ev) = param.enum_values {
                                if !ev.is_empty() {
                                    raw.insert(param.name.clone(), Value::String("INVALID_ENUM_VALUE_XYZ".into()));
                                }
                            }
                        }
                        _ => {
                            // Explicit null
                            raw.insert(param.name.clone(), Value::Null);
                        }
                    }
                }

                // Maybe add an undeclared extra parameter
                if rng.next_bool() {
                    raw.insert("extra_unknown_param_999".into(), Value::String("extra".into()));
                }

                let form = match rng.next_usize(4) {
                    0 => None,
                    1 => Some(InputForm::Flags),
                    2 => Some(InputForm::Json),
                    _ => Some(InputForm::Graphql),
                };
                let opts = ValidationOptions { form };
                let form_str = form.map(|f| f.as_str());

                let r_res = validate(op, &raw, opts);
                let t_res = oracle
                    .call(
                        "validate",
                        json!({
                            "schema": serde_json::to_value(op).unwrap(),
                            "raw": raw,
                            "opts": { "form": form_str }
                        }),
                    )
                    .unwrap();

                let t_ok = t_res["ok"].as_bool().unwrap();

                assert_eq!(
                    r_res.is_ok(),
                    t_ok,
                    "validate ok divergence on op {}: rust_ok={}, ts_ok={}. Raw: {:?}, TS: {:?}",
                    op.name,
                    r_res.is_ok(),
                    t_ok,
                    raw,
                    t_res
                );

                if t_ok {
                    let r_val = Value::Object(r_res.unwrap());
                    let t_val = &t_res["value"];
                    assert!(
                        deep_eq(&r_val, t_val),
                        "validate value divergence on op {}:\nRust: {}\nTS: {}",
                        op.name,
                        r_val,
                        t_val
                    );
                } else {
                    let r_issues = extract_rust_issues(&r_res.unwrap_err());
                    let t_issues = extract_issues(&t_res);
                    assert_eq!(
                        r_issues, t_issues,
                        "validate issues divergence on op {}:\nRust: {:?}\nTS: {:?}",
                        op.name, r_issues, t_issues
                    );
                }

                case_count += 1;
            }
        }
    }

    assert!(case_count >= 600, "validate cases run: {case_count}");
}

// ---------------------------------------------------------------------------
// 2. Differential: parse_argv_form
// ---------------------------------------------------------------------------
#[test]
fn diff_parse_argv_form_suite() {
    let Some(mut oracle) = setup_oracle_or_skip() else {
        return;
    };
    let fixtures = load_fixtures();
    assert!(!fixtures.is_empty());
    let mut rng = Rng::new(0x22223333_44445555);

    let globals = vec![
        ParamSpec::new("dryRun", ParamType::Boolean).optional(),
        ParamSpec::new("readOnly", ParamType::Boolean).optional(),
    ];

    let mut case_count = 0;

    for doc in &fixtures {
        for op in &doc.operations {
            for _ in 0..15 {
                let mut argv = Vec::new();

                // Sometimes inject global flags before op args
                if rng.next_bool() {
                    argv.push(if rng.next_bool() {
                        "--dry-run".into()
                    } else {
                        "--dryrun".into()
                    });
                }

                for param in &op.params {
                    if param.doc_only == Some(true) || param.cli_only == Some(true) {
                        continue;
                    }
                    if rng.next_bool() {
                        let kebab = to_kebab(&param.name);
                        match rng.next_usize(6) {
                            0 => {
                                // --flag value
                                argv.push(format!("--{kebab}"));
                                argv.push(rng.gen_word(6));
                            }
                            1 => {
                                // --flag=value
                                argv.push(format!("--{kebab}={}", rng.gen_word(6)));
                            }
                            2 => {
                                // boolean flag: --flag
                                argv.push(format!("--{kebab}"));
                            }
                            3 => {
                                // boolean negation: --no-flag
                                argv.push(format!("--no-{kebab}"));
                            }
                            4 => {
                                // repeated flag
                                argv.push(format!("--{kebab}=val1"));
                                argv.push(format!("--{kebab}=val2"));
                            }
                            _ => {
                                // short alias if any
                                if let Some(ref aliases) = param.aliases {
                                    if let Some(alias) = aliases.first() {
                                        argv.push(format!("-{alias}"));
                                        argv.push("alias_val".into());
                                    }
                                }
                            }
                        }
                    }
                }

                // Maybe include -- separator and trailing positionals
                if rng.next_bool() {
                    argv.push("--".into());
                    argv.push(rng.gen_word(6));
                    argv.push(rng.gen_word(6));
                }

                let r_res = parse_argv_form(&argv, op, &globals);
                let t_res = oracle
                    .call(
                        "parse_argv_form",
                        json!({
                            "argv": argv,
                            "schema": serde_json::to_value(op).unwrap(),
                            "globals": serde_json::to_value(&globals).unwrap(),
                        }),
                    )
                    .unwrap();

                let t_ok = t_res["ok"].as_bool().unwrap();

                assert_eq!(
                    r_res.is_ok(),
                    t_ok,
                    "parse_argv_form ok divergence on op {}: argv={:?}, rust_ok={}, ts_ok={}. TS: {:?}",
                    op.name,
                    argv,
                    r_res.is_ok(),
                    t_ok,
                    t_res
                );

                if t_ok {
                    let r_val = r_res.unwrap();
                    let t_val = &t_res["value"];
                    assert!(
                        deep_eq(&Value::Object(r_val.params.clone()), &t_val["params"])
                            && deep_eq(
                                &serde_json::to_value(&r_val.positionals).unwrap(),
                                &t_val["positionals"]
                            )
                            && deep_eq(&Value::Object(r_val.globals.clone()), &t_val["globals"]),
                        "parse_argv_form value divergence on op {}: argv={:?}\nRust: {:?}\nTS: {:?}",
                        op.name,
                        argv,
                        r_val,
                        t_val
                    );
                } else {
                    let r_issues = extract_rust_issues(&r_res.unwrap_err());
                    let t_issues = extract_issues(&t_res);
                    assert_eq!(
                        r_issues, t_issues,
                        "parse_argv_form issues divergence on op {}: argv={:?}\nRust: {:?}\nTS: {:?}",
                        op.name, argv, r_issues, t_issues
                    );
                }

                case_count += 1;
            }
        }
    }

    assert!(case_count >= 500, "argv cases run: {case_count}");
}

// ---------------------------------------------------------------------------
// 3. Differential: json_form & graphql_form documents
// ---------------------------------------------------------------------------
#[test]
fn diff_json_and_graphql_form_suite() {
    let Some(mut oracle) = setup_oracle_or_skip() else {
        return;
    };
    let mut rng = Rng::new(0x33334444_55556666);

    let mut json_cases = 0;
    let mut graphql_cases = 0;

    // JSON forms (valid envelopes, bare objects, arrays, malformed, non-string ops)
    for _ in 0..250 {
        let text = match rng.next_usize(6) {
            0 => {
                // Envelope
                json!({ "op": "status", "params": { "path": rng.gen_word(6) } }).to_string()
            }
            1 => {
                // Bare params object
                json!({ "limit": rng.next_u32() % 50, "author": rng.gen_word(5) }).to_string()
            }
            2 => {
                // Envelope with non-string op (treated as bare params)
                json!({ "op": 12345, "params": { "foo": "bar" } }).to_string()
            }
            3 => {
                // Non-object root (error)
                json!([1, 2, 3]).to_string()
            }
            4 => {
                // Malformed JSON (syntax error)
                format!("{{ unquoted_key: \"{}\", bad_json ", rng.gen_word(5))
            }
            _ => {
                // Empty string or whitespace
                "".into()
            }
        };

        let r_res = parse_json_document(&text);
        let t_res = oracle.call("parse_json_document", json!({ "text": text })).unwrap();

        let t_ok = t_res["ok"].as_bool().unwrap();
        assert_eq!(
            r_res.is_ok(),
            t_ok,
            "parse_json_document ok divergence on text: {:?}",
            text
        );

        if t_ok {
            let r_val = r_res.unwrap();
            let t_val = &t_res["value"];
            let mut r_map = Record::new();
            if let Some(ref op) = r_val.op {
                r_map.insert("op".into(), Value::String(op.clone()));
            }
            r_map.insert("params".into(), Value::Object(r_val.params));
            let r_json = Value::Object(r_map);
            assert!(
                deep_eq(&r_json, t_val),
                "parse_json_document value divergence:\nRust: {}\nTS: {}",
                r_json,
                t_val
            );
        } else {
            let r_issues = extract_rust_issues(&r_res.unwrap_err());
            let t_issues = extract_issues(&t_res);
            assert_eq!(
                r_issues, t_issues,
                "parse_json_document issues divergence on text: {:?}\nRust: {:?}\nTS: {:?}",
                text, r_issues, t_issues
            );
        }

        json_cases += 1;
    }

    // GraphQL forms (queries, mutations, shorthand, comments, invalid syntax, variables)
    for _ in 0..250 {
        let text = match rng.next_usize(6) {
            0 => {
                // Valid query
                format!(
                    "query {{ commit(message: \"{}\", limit: {}) {{ __typename }} }}",
                    rng.gen_word(6),
                    rng.next_u32() % 100
                )
            }
            1 => {
                // Valid mutation
                format!(
                    "mutation {{ branch_create(name: \"{}\", force: true) {{ __typename }} }}",
                    rng.gen_word(6)
                )
            }
            2 => {
                // Bare selection set
                format!("{{ status(path: \"{}\") {{ __typename }} }}", rng.gen_word(6))
            }
            3 => {
                // With comments
                format!(
                    "# comment\n{{ log(maxCount: {}, author: \"{}\") {{ __typename }} }}",
                    rng.next_u32() % 50,
                    rng.gen_word(6)
                )
            }
            4 => {
                // Variables or named operations (must be rejected)
                "query MyQuery($v: String) { status { __typename } }".into()
            }
            _ => {
                // Malformed GraphQL
                format!("mutation {{ unclosed(foo: \"{}\"", rng.gen_word(5))
            }
        };

        let r_res = parse_graphql_document(&text);
        let t_res = oracle.call("parse_graphql_document", json!({ "text": text })).unwrap();

        let t_ok = t_res["ok"].as_bool().unwrap();
        assert_eq!(
            r_res.is_ok(),
            t_ok,
            "parse_graphql_document ok divergence on text: {:?}",
            text
        );

        if t_ok {
            let r_val = r_res.unwrap();
            let t_val = &t_res["value"];
            let mut r_map = Record::new();
            if let Some(ref op) = r_val.op {
                r_map.insert("op".into(), Value::String(op.clone()));
            }
            r_map.insert("params".into(), Value::Object(r_val.params));
            let r_json = Value::Object(r_map);
            assert!(
                deep_eq(&r_json, t_val),
                "parse_graphql_document value divergence:\nRust: {}\nTS: {}",
                r_json,
                t_val
            );
        } else {
            let r_issues = extract_rust_issues(&r_res.unwrap_err());
            let t_issues = extract_issues(&t_res);
            assert_eq!(
                r_issues, t_issues,
                "parse_graphql_document issues divergence on text: {:?}\nRust: {:?}\nTS: {:?}",
                text, r_issues, t_issues
            );
        }

        graphql_cases += 1;
    }

    assert!(json_cases >= 250);
    assert!(graphql_cases >= 250);
}

// ---------------------------------------------------------------------------
// 4. Differential: encode round-trips
// ---------------------------------------------------------------------------
#[test]
fn diff_encode_round_trips_suite() {
    let Some(mut oracle) = setup_oracle_or_skip() else {
        return;
    };
    let fixtures = load_fixtures();
    assert!(!fixtures.is_empty());
    let mut rng = Rng::new(0x44445555_66667777);

    let mut case_count = 0;

    for doc in &fixtures {
        for op in &doc.operations {
            for _ in 0..10 {
                let mut params = Record::new();
                for param in &op.params {
                    if param.doc_only == Some(true) || param.cli_only == Some(true) {
                        continue;
                    }
                    if rng.next_bool() {
                        let val = match param.kind {
                            ParamType::String => Value::String(rng.gen_word(6)),
                            ParamType::Number => Value::Number(serde_json::Number::from((rng.next_u32() % 100) as i64)),
                            ParamType::Boolean => Value::Bool(rng.next_bool()),
                            ParamType::Object => {
                                let mut m = Record::new();
                                m.insert(rng.gen_ident(), Value::String(rng.gen_word(4)));
                                Value::Object(m)
                            }
                            ParamType::Array => Value::Array(vec![Value::String(rng.gen_word(5))]),
                            ParamType::Json => Value::String(format!("prefix_{}", rng.gen_word(4))),
                        };
                        params.insert(param.name.clone(), val);
                    }
                }

                let schema_val = serde_json::to_value(op).unwrap();

                // 4a. encode_argv
                let r_argv = encode_argv(op, &params, EncodeArgvOptions::default());
                let t_argv = oracle
                    .call(
                        "encode_argv",
                        json!({
                            "schema": schema_val.clone(),
                            "params": params.clone(),
                            "opts": {}
                        }),
                    )
                    .unwrap();
                let t_argv_vec: Vec<String> = serde_json::from_value(t_argv["value"].clone()).unwrap();
                assert_eq!(
                    r_argv, t_argv_vec,
                    "encode_argv divergence on op {}: params={:?}",
                    op.name, params
                );

                // 4b. encode_json
                let r_json = encode_json(op, &params);
                let t_json = oracle
                    .call(
                        "encode_json",
                        json!({
                            "schema": schema_val.clone(),
                            "params": params.clone(),
                        }),
                    )
                    .unwrap();
                let t_json_str = t_json["value"].as_str().unwrap();
                // Compare parsed JSON representations
                let r_parsed: Value = serde_json::from_str(&r_json).unwrap();
                let t_parsed: Value = serde_json::from_str(t_json_str).unwrap();
                assert!(
                    deep_eq(&r_parsed, &t_parsed),
                    "encode_json divergence on op {}:\nRust: {}\nTS: {}",
                    op.name,
                    r_json,
                    t_json_str
                );

                // 4c. encode_graphql (when op and param names are valid identifiers)
                let gql_ok = op.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && params
                        .keys()
                        .all(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));

                if gql_ok {
                    let r_gql = encode_graphql(op, &params);
                    let t_gql = oracle
                        .call(
                            "encode_graphql",
                            json!({
                                "schema": schema_val.clone(),
                                "params": params.clone(),
                            }),
                        )
                        .unwrap();

                    assert_eq!(
                        r_gql.is_ok(),
                        t_gql["ok"].as_bool().unwrap(),
                        "encode_graphql ok divergence on op {}",
                        op.name
                    );

                    if let Ok(gql_str) = r_gql {
                        let r_doc = parse_graphql_document(&gql_str).unwrap();
                        let t_doc = parse_graphql_document(t_gql["value"].as_str().unwrap()).unwrap();
                        assert!(
                            deep_eq(&Value::Object(r_doc.params), &Value::Object(t_doc.params)),
                            "encode_graphql parsed divergence on op {}",
                            op.name
                        );
                    }
                }

                case_count += 1;
            }
        }
    }

    assert!(case_count >= 400, "encode cases run: {case_count}");
}

// ---------------------------------------------------------------------------
// 5. Differential: interpret_command
// ---------------------------------------------------------------------------
#[test]
fn diff_interpret_command_suite() {
    let Some(mut oracle) = setup_oracle_or_skip() else {
        return;
    };
    let fixtures = load_fixtures();
    assert!(!fixtures.is_empty());
    let mut rng = Rng::new(0x55556666_77778888);

    let globals = vec![ParamSpec::new("dryRun", ParamType::Boolean).optional()];
    let schemas_val = serde_json::to_value(&fixtures).unwrap();

    let mut case_count = 0;

    for _ in 0..500 {
        let doc = rng.choose(&fixtures);
        let op = rng.choose(&doc.operations);

        let line = match rng.next_usize(10) {
            0 => {
                // Simple plain invocation
                format!("{} {}", doc.tool, op.name)
            }
            1 => {
                // Quoted values (single and double)
                format!(
                    "{} {} --path 'hello world' --message \"fix: commit\"",
                    doc.tool, op.name
                )
            }
            2 => {
                // Escaped spaces
                format!("{} {} --path hello\\ world", doc.tool, op.name)
            }
            3 => {
                // Disallowed shell syntax: pipe
                format!("{} {} | grep something", doc.tool, op.name)
            }
            4 => {
                // Disallowed shell syntax: redirection
                format!("{} {} > /tmp/out.txt", doc.tool, op.name)
            }
            5 => {
                // Disallowed shell syntax: env assignment at start
                format!("VAR=val {} {}", doc.tool, op.name)
            }
            6 => {
                // Disallowed shell syntax: unquoted glob or tilde
                format!("{} {} ~/folder", doc.tool, op.name)
            }
            7 => {
                // Unterminated quote
                format!("{} {} \"unclosed string", doc.tool, op.name)
            }
            8 => {
                // Unicode argument
                format!("{} {} --author \"Snowman \u{2603}\"", doc.tool, op.name)
            }
            _ => {
                // Global flag before op
                format!("{} --dryrun {}", doc.tool, op.name)
            }
        };

        let r_res = interpret_command(&fixtures, &line, &globals);
        let t_res = oracle
            .call(
                "interpret_command",
                json!({
                    "schemas": schemas_val.clone(),
                    "line": line,
                    "globals": serde_json::to_value(&globals).unwrap(),
                }),
            )
            .unwrap();

        let t_ok = t_res["ok"].as_bool().unwrap();

        assert_eq!(
            r_res.is_ok(),
            t_ok,
            "interpret_command ok divergence on line {:?}: rust_ok={}, ts_ok={}. TS: {:?}",
            line,
            r_res.is_ok(),
            t_ok,
            t_res
        );

        if t_ok {
            let r_val = r_res.unwrap();
            let t_val = &t_res["value"];
            assert_eq!(r_val.tool, t_val["tool"].as_str().unwrap());
            assert_eq!(r_val.op, t_val["op"].as_str().unwrap());
            assert_eq!(r_val.effect.as_str(), t_val["effect"].as_str().unwrap());
            assert!(deep_eq(&Value::Object(r_val.params), &t_val["params"]));
            assert!(deep_eq(&Value::Object(r_val.globals), &t_val["globals"]));
            assert!(deep_eq(&serde_json::to_value(&r_val.argv).unwrap(), &t_val["argv"]));
        } else {
            let r_issues = extract_rust_issues(&r_res.unwrap_err());
            let t_issues = extract_issues(&t_res);
            assert_eq!(
                r_issues, t_issues,
                "interpret_command issues divergence on line {:?}\nRust: {:?}\nTS: {:?}",
                line, r_issues, t_issues
            );
        }

        case_count += 1;
    }

    assert!(case_count >= 500, "interpret cases run: {case_count}");
}

// ---------------------------------------------------------------------------
// Divergence regression tests: marked #[ignore = "divergence: ..."]
// ---------------------------------------------------------------------------

#[test]
#[ignore = "divergence: lmts parses 1e309 in GraphQL as Infinity; lmrust rejects non-finite numbers as DOCUMENT_PARSE"]
fn divergence_graphql_overflow_infinity() {
    let Some(mut oracle) = setup_oracle_or_skip() else {
        return;
    };
    let text = "{ op(num: 1e309) { __typename } }";
    let r_res = parse_graphql_document(text);
    let t_res = oracle.call("parse_graphql_document", json!({ "text": text })).unwrap();
    // In lmts: ok is true, params has { num: null } (serialized from JS Infinity)
    // In lmrust: Err(DOCUMENT_PARSE)
    assert_eq!(r_res.is_ok(), t_res["ok"].as_bool().unwrap());
}

#[test]
#[ignore = "divergence: lmts parses --num Infinity as numeric Infinity; lmrust keeps text for validate() to reject"]
fn divergence_argv_infinity_number() {
    let Some(mut oracle) = setup_oracle_or_skip() else {
        return;
    };
    let op = OperationSchema::new("test").param(ParamSpec::new("num", ParamType::Number).optional());
    let argv = vec!["--num".to_string(), "Infinity".to_string()];
    let r_res = parse_argv_form(&argv, &op, &[]).unwrap();
    let t_res = oracle
        .call(
            "parse_argv_form",
            json!({
                "argv": argv,
                "schema": serde_json::to_value(&op).unwrap(),
                "globals": []
            }),
        )
        .unwrap();
    // In lmts: params has { num: null } (serialized from JS Infinity)
    // In lmrust: params has { num: "Infinity" } (keeps uncoerced string)
    assert!(deep_eq(&Value::Object(r_res.params), &t_res["value"]["params"]));
}
