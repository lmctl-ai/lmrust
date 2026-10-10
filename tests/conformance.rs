//! lmapi conformance vectors (`lmapi/conformance/**`) run against lmrust.
//!
//! The vectors live in the lmapi repository. The directory is `$LMAPI_DIR/conformance`,
//! where `LMAPI_DIR` defaults to the sibling checkout `../lmapi`. When it is missing the
//! suite is skipped locally but FAILS when `GITHUB_ACTIONS` is set, so CI can never pass
//! without running the vectors. Issues are compared by `code` and by `param` / `op` /
//! `suggestion` identity when the vector names them, never by message text.
//!
//! Run `cargo test --test conformance -- --nocapture` to see per-file counts.

use lmrust::js::deep_eq;
use lmrust::{
    describe_for_prompt, interpret_command, parse_argv_form, parse_published_schema, validate, InputForm,
    OperationSchema, ParamSpec, PublishedCliSchema, ValidationIssue, ValidationOptions,
};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn conformance_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LMAPI_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("lmapi"));
    let dir = base.join("conformance");
    if dir.is_dir() {
        return Some(dir);
    }
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        panic!(
            "{} not found: check out lmctl-ai/lmapi beside lmrust (or set LMAPI_DIR) in the workflow",
            dir.display()
        );
    }
    eprintln!("skipping conformance: {} not found (set LMAPI_DIR)", dir.display());
    None
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn cases_of(doc: &Value) -> Vec<Value> {
    match doc {
        Value::Array(list) => list.clone(),
        Value::Object(o) => match o.get("cases") {
            Some(Value::Array(list)) => list.clone(),
            _ => vec![doc.clone()],
        },
        other => vec![other.clone()],
    }
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    files.retain(|p| p.extension().is_some_and(|e| e == "json"));
    files.sort();
    files
}

/// Every expected issue must match an actual one by code and by each identity field it names.
fn check_issues(expected: &Value, actual: &[ValidationIssue], fields: &[&str]) -> Result<(), String> {
    for exp in expected.as_array().into_iter().flatten() {
        let code = exp["code"].as_str().unwrap_or_default();
        let found = actual.iter().find(|i| {
            i.code.as_str() == code
                && fields.iter().all(|f| match exp.get(*f).and_then(Value::as_str) {
                    None => true,
                    Some(want) => match *f {
                        "param" => i.param.as_deref() == Some(want),
                        "op" => i.op.as_deref() == Some(want),
                        "suggestion" => i.suggestion.as_deref() == Some(want),
                        _ => false,
                    },
                })
        });
        match found {
            None => {
                return Err(format!(
                    "missing issue {exp}; got {}",
                    serde_json::to_string(actual).unwrap()
                ))
            }
            Some(i) if i.synopsis.is_empty() || i.usage.is_empty() || i.message.is_empty() => {
                return Err(format!("issue {code} lacks message/synopsis/usage"))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

fn expect_eq(what: &str, want: &Value, got: &Value) -> Result<(), String> {
    if deep_eq(want, got) {
        Ok(())
    } else {
        Err(format!("{what}: expected {want}, got {got}"))
    }
}

struct Tally {
    label: String,
    passed: usize,
    failures: Vec<String>,
}

impl Tally {
    fn new(label: String) -> Self {
        Tally {
            label,
            passed: 0,
            failures: Vec::new(),
        }
    }
    fn record(&mut self, name: &str, outcome: Result<(), String>) {
        match outcome {
            Ok(()) => self.passed += 1,
            Err(e) => self.failures.push(format!("{} :: {name}: {e}", self.label)),
        }
    }
}

fn run_dir(sub: &str, mut run_case: impl FnMut(&Path, &Value) -> Result<(), String>) -> Vec<Tally> {
    let Some(root) = conformance_dir() else {
        return Vec::new();
    };
    let dir = root.join(sub);
    assert!(dir.is_dir(), "{} missing", dir.display());
    let mut tallies = Vec::new();
    for file in json_files(&dir) {
        let label = format!("{sub}/{}", file.file_name().unwrap().to_string_lossy());
        let mut tally = Tally::new(label);
        for tc in cases_of(&read_json(&file)) {
            let name = tc["name"].as_str().unwrap_or("<unnamed>").to_string();
            let outcome = run_case(&root, &tc);
            tally.record(&name, outcome);
        }
        tallies.push(tally);
    }
    tallies
}

fn report(tallies: Vec<Tally>) {
    let mut failures = Vec::new();
    for t in &tallies {
        println!(
            "conformance {}: {}/{} passed",
            t.label,
            t.passed,
            t.passed + t.failures.len()
        );
        failures.extend(t.failures.iter().cloned());
    }
    assert!(failures.is_empty(), "conformance failures:\n{}", failures.join("\n"));
}

fn form_of(tc: &Value) -> InputForm {
    match tc.get("form").and_then(Value::as_str) {
        Some("json") => InputForm::Json,
        Some("graphql") => InputForm::Graphql,
        _ => InputForm::Flags,
    }
}

#[test]
fn validate_vectors() {
    report(run_dir("validate", |_, tc| {
        let schema: OperationSchema = serde_json::from_value(tc["schema"].clone()).map_err(|e| e.to_string())?;
        let input = tc["input"].as_object().cloned().unwrap_or_default();
        let res = validate(&schema, &input, ValidationOptions::form(form_of(tc)));
        match (tc["expect"]["ok"].as_bool(), res) {
            (Some(true), Ok(params)) => expect_eq("params", &tc["expect"]["params"], &Value::Object(params)),
            (Some(true), Err(issues)) => Err(format!("unexpected issues {}", serde_json::to_string(&issues).unwrap())),
            (_, Ok(params)) => Err(format!("expected failure, got {}", Value::Object(params))),
            (_, Err(issues)) => check_issues(&tc["expect"]["issues"], &issues, &["param", "suggestion"]),
        }
    }));
}

#[test]
fn argv_vectors() {
    report(run_dir("argv", |_, tc| {
        let schema: OperationSchema = serde_json::from_value(tc["schema"].clone()).map_err(|e| e.to_string())?;
        let globals: Vec<ParamSpec> = match tc.get("globals") {
            Some(g) => serde_json::from_value(g.clone()).map_err(|e| e.to_string())?,
            None => Vec::new(),
        };
        let argv: Vec<String> = serde_json::from_value(tc["argv"].clone()).map_err(|e| e.to_string())?;
        let res = parse_argv_form(&argv, &schema, &globals);
        let exp = &tc["expect"];
        match (exp["ok"].as_bool(), res) {
            (Some(true), Ok(v)) => {
                expect_eq("params", &exp["params"], &Value::Object(v.params))?;
                if let Some(p) = exp.get("positionals") {
                    expect_eq("positionals", p, &serde_json::to_value(&v.positionals).unwrap())?;
                }
                if let Some(g) = exp.get("globals") {
                    expect_eq("globals", g, &Value::Object(v.globals))?;
                }
                Ok(())
            }
            (Some(true), Err(issues)) => Err(format!("unexpected issues {}", serde_json::to_string(&issues).unwrap())),
            (_, Ok(v)) => Err(format!("expected failure, got {:?}", v)),
            (_, Err(issues)) => check_issues(&exp["issues"], &issues, &["param", "suggestion"]),
        }
    }));
}

fn load_schemas(root: &Path, spec: &Value) -> Result<Vec<PublishedCliSchema>, String> {
    let doc = match spec {
        Value::String(rel) => read_json(&root.join(rel)),
        other => other.clone(),
    };
    match doc {
        Value::Array(list) => list
            .into_iter()
            .map(|d| serde_json::from_value(d).map_err(|e| e.to_string()))
            .collect(),
        single => Ok(vec![serde_json::from_value(single).map_err(|e| e.to_string())?]),
    }
}

#[test]
fn interpret_vectors() {
    report(run_dir("interpret", |root, tc| {
        let schemas = load_schemas(root, &tc["schemas"])?;
        let line = tc["line"].as_str().unwrap_or_default();
        let res = interpret_command(&schemas, line, &[]);
        let exp = &tc["expect"];
        match (exp["ok"].as_bool(), res) {
            (Some(true), Ok(v)) => {
                expect_eq("tool", &exp["tool"], &Value::String(v.tool.clone()))?;
                expect_eq("op", &exp["op"], &Value::String(v.op.clone()))?;
                expect_eq("argv", &exp["argv"], &serde_json::to_value(&v.argv).unwrap())?;
                expect_eq("params", &exp["params"], &Value::Object(v.params.clone()))?;
                if let Some(p) = exp.get("positionals") {
                    expect_eq("positionals", p, &serde_json::to_value(&v.positionals).unwrap())?;
                }
                if let Some(g) = exp.get("globals") {
                    expect_eq("globals", g, &Value::Object(v.globals.clone()))?;
                }
                expect_eq("effect", &exp["effect"], &Value::String(v.effect.as_str().into()))
            }
            (Some(true), Err(issues)) => Err(format!("unexpected issues {}", serde_json::to_string(&issues).unwrap())),
            (_, Ok(v)) => Err(format!("expected failure, got {:?}", v)),
            // lmts compares code and suggestion here; lmrust also checks param and op when named.
            (_, Err(issues)) => check_issues(&exp["issues"], &issues, &["param", "op", "suggestion"]),
        }
    }));
}

#[test]
fn publish_vectors() {
    report(run_dir("publish", |_, tc| {
        let res = parse_published_schema(&tc["document"]);
        match (tc["expect"]["ok"].as_bool(), res) {
            (Some(true), Ok(_)) => Ok(()),
            (Some(true), Err(issues)) => Err(format!("unexpected issues {}", serde_json::to_string(&issues).unwrap())),
            (_, Ok(_)) => Err("expected failure, document was accepted".into()),
            (_, Err(issues)) => check_issues(&tc["expect"]["issues"], &issues, &["param", "op"]),
        }
    }));
}

#[test]
fn prompt_vectors() {
    let Some(root) = conformance_dir() else { return };
    let dir = root.join("prompt");
    let mut tally = Tally::new("prompt/cases.json".into());
    for tc in cases_of(&read_json(&dir.join("cases.json"))) {
        let name = tc["name"].as_str().unwrap_or_default().to_string();
        let outcome = load_schemas(&root, &tc["schemas"]).and_then(|schemas| {
            let golden =
                fs::read_to_string(dir.join(tc["golden"].as_str().unwrap_or_default())).map_err(|e| e.to_string())?;
            let rendered = describe_for_prompt(&schemas);
            if rendered == golden {
                Ok(())
            } else {
                let at = rendered.lines().zip(golden.lines()).position(|(a, b)| a != b);
                Err(format!(
                    "rendered text differs from golden (first differing line {at:?})"
                ))
            }
        });
        tally.record(&name, outcome);
    }
    report(vec![tally]);
}

/// Every shared fixture must itself be a valid published document.
#[test]
fn fixture_documents_are_valid() {
    let Some(root) = conformance_dir() else { return };
    let mut tally = Tally::new("fixtures".into());
    for file in json_files(&root.join("fixtures")) {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        let outcome = parse_published_schema(&read_json(&file))
            .map(|_| ())
            .map_err(|issues| serde_json::to_string(&issues).unwrap());
        tally.record(&name, outcome);
    }
    assert!(tally.passed > 0, "no fixtures found");
    report(vec![tally]);
}
