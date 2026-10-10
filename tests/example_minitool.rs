//! End to end: run the `minitool` example (examples/minitool.rs) as a real process and
//! check stdout JSON and exit codes. `cargo test` builds examples before running tests;
//! when the binary is missing (a filtered `cargo test --test ...` run) the test says so.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

fn minitool() -> Option<PathBuf> {
    // target/<profile>/deps/<this test> -> target/<profile>/examples/minitool
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?;
    let bin = profile_dir
        .join("examples")
        .join(format!("minitool{}", std::env::consts::EXE_SUFFIX));
    bin.is_file().then_some(bin)
}

fn run(args: &[&str], env: &[(&str, &str)]) -> Option<(i32, Value)> {
    let Some(bin) = minitool() else {
        eprintln!("minitool example not built (run plain `cargo test`); skipping");
        return None;
    };
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env_remove("MINITOOL_READONLY")
        .env_remove("MINITOOL_DRYRUN");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run minitool");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or(Value::String(stdout));
    Some((out.status.code().unwrap_or(-1), value))
}

#[test]
fn serves_schema_and_operations_end_to_end() {
    let Some((code, schema)) = run(&["--schema"], &[]) else {
        return;
    };
    assert_eq!(code, 0);
    assert_eq!(schema["schema"], json!("cli.schema.v1"));
    assert_eq!(schema["tool"], json!("minitool"));
    assert_eq!(schema["operations"].as_array().unwrap().len(), 2);
    // The published document is valid for every client.
    assert!(lmrust::parse_published_schema(&schema).is_ok());

    let (code, out) = run(&["greet", "Ada", "--times", "2"], &[]).unwrap();
    assert_eq!((code, out), (0, json!({"greeting": ["hello, Ada", "hello, Ada"]})));
    let (code, out) = run(
        &["--input-json", r#"{"op":"greet","params":{"name":"Ada","shout":true}}"#],
        &[],
    )
    .unwrap();
    assert_eq!((code, out), (0, json!({"greeting": ["HELLO, ADA"]})));
    let (code, out) = run(&["greet", "--input-graphql", r#"{ greet(name: "Bo") }"#], &[]).unwrap();
    assert_eq!((code, out), (0, json!({"greeting": ["hello, Bo"]})));
}

#[test]
fn policy_and_errors_end_to_end() {
    let Some((code, out)) = run(&["save_note", "--text", "hi", "--dryrun"], &[]) else {
        return;
    };
    assert_eq!(code, 0);
    assert_eq!(out["dryRun"], json!(true));
    assert_eq!(out["command"], json!(["notes", "add", "hi"]));
    let (code, out) = run(&["save_note", "--text", "hi"], &[("MINITOOL_READONLY", "1")]).unwrap();
    assert_eq!((code, out["error"]["code"].clone()), (6, json!("READONLY_VIOLATION")));
    let (code, out) = run(&["save_note", "--text", ""], &[]).unwrap();
    assert_eq!((code, out["error"]["code"].clone()), (2, json!("INVALID_REQUEST")));
    let (code, out) = run(&["greet"], &[]).unwrap();
    assert_eq!(
        (code, out["error"]["data"]["issues"][0]["code"].clone()),
        (2, json!("MISSING_REQUIRED"))
    );
    let (code, out) = run(&["nope"], &[]).unwrap();
    assert_eq!((code, out["error"]["code"].clone()), (2, json!("INVALID_REQUEST")));
    let (code, out) = run(&["save_note", "--text", "hi"], &[]).unwrap();
    assert_eq!((code, out), (0, json!({"saved": true, "bytes": 2})));
}

#[test]
fn help_is_text() {
    let Some((code, out)) = run(&[], &[]) else { return };
    assert_eq!(code, 0);
    let text = out.as_str().unwrap();
    assert!(text.starts_with("minitool — structured CLI wrapper"));
    assert!(text.contains("greet"));
    let (code, out) = run(&["greet", "--help"], &[]).unwrap();
    assert_eq!(code, 0);
    assert!(out.as_str().unwrap().contains("minitool greet <name>"));
}
