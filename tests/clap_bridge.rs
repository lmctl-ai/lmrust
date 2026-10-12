//! Clap bridge end to end (feature `clap`, `harness = false`).
//!
//! This test binary is also the fixture tool: with `CLAP_FIXTURE=1` it runs `fixtool` (the
//! bridge first, then the legacy clap program in `clap_fixture/`). The bridge's default
//! executable is `std::env::current_exe()`, so every lmapi call below really self-execs a clap
//! binary. No network and no external tools.

mod clap_fixture;

use lmrust::clap_bridge::{assert_coverage, coverage_problems, legacy_argv, Bridge};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;

const FIXTURE_ENV: &str = "CLAP_FIXTURE";

fn bridge() -> Bridge {
    Bridge::new(clap_fixture::command(), clap_fixture::OVERLAY).expect("fixture overlay")
}

fn fixture_main() -> i32 {
    if let Some(code) = bridge().maybe_run(std::env::args_os().skip(1)) {
        return code;
    }
    clap_fixture::legacy_main(std::env::args().skip(1).collect())
}

struct Run {
    code: i32,
    stdout: String,
    json: Value,
}

fn fixtool(args: &[&str]) -> Run {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(args)
        .env(FIXTURE_ENV, "1")
        .output()
        .expect("spawn fixture");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    Run {
        code: out.status.code().unwrap_or(-1),
        json: serde_json::from_str(&stdout).unwrap_or(Value::Null),
        stdout,
    }
}

fn in_process(bridge: &Bridge, args: &[&str]) -> (i32, Value) {
    let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    let code = bridge
        .maybe_run_io(&argv, &mut out, &HashMap::new())
        .expect("lmapi-shaped");
    (code, serde_json::from_slice(&out).unwrap_or(Value::Null))
}

fn child_argv(run: &Run) -> Vec<String> {
    serde_json::from_value(run.json["argv"].clone()).unwrap_or_else(|_| panic!("no argv in {}", run.stdout))
}

// ---- schema ------------------------------------------------------------------------------------

fn schema_is_deterministic_and_follows_lmapi() {
    let a = fixtool(&["--schema"]);
    let b = fixtool(&["--schema"]);
    assert_eq!(a.code, 0, "{}", a.stdout);
    assert_eq!(a.stdout, b.stdout, "byte-identical schema");
    assert_eq!(
        a.stdout,
        serde_json::to_string_pretty(&bridge().schema().unwrap()).unwrap() + "\n"
    );
    let doc = &a.json;
    lmrust::parse_published_schema(doc).unwrap_or_else(|issues| panic!("{issues:?}"));
    assert_eq!(doc["schema"], "cli.schema.v1");
    assert_eq!(doc["tool"], "fixtool");
    assert_eq!(doc["version"], "9.9.9", "clap's version when the bridge sets none");
    assert_eq!(doc["inputs"], json!(["flags", "json"]));
    assert_eq!(doc["policy"], json!({"dryrun": true, "readonly": true}));
    assert!(doc.get("product").is_none());
    let globals: Vec<&str> = doc["globals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert!(!globals.contains(&"format"), "the tool owns --format: {globals:?}");
    let names: Vec<&str> = doc["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "stats_run",
            "data_load",
            "data_lines",
            "data_drop",
            "echo_text",
            "exit_with",
            "crash_now",
            "big_output",
            "sleep_for"
        ],
        "overlay order"
    );
    let stats = &doc["operations"][0];
    assert_eq!(stats["access"], "read");
    assert_eq!(stats["description"], "Summarise a file");
    let params: Vec<&str> = stats["params"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        params,
        ["columns", "tag", "limit", "verbose", "mode", "level", "input"],
        "clap order, positionals last"
    );
    let p = |n: &str| {
        stats["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == n)
            .unwrap()
            .clone()
    };
    assert_eq!(p("columns")["type"], "array");
    assert_eq!(p("columns")["occurs"], "*");
    assert_eq!(p("tag")["aliases"], json!(["t"]));
    assert_eq!(p("limit")["type"], "number");
    assert_eq!(p("limit")["default"], json!(10));
    assert_eq!(p("verbose")["type"], "boolean");
    assert_eq!(p("verbose")["aliases"], json!(["loud"]));
    assert_eq!(p("mode")["enum"], json!(["fast", "slow"]));
    assert_eq!(p("mode")["default"], "fast");
    assert_eq!(p("level")["type"], "number");
    assert_eq!(p("input")["positional"], true);
    assert_eq!(p("input")["occurs"], "1");
    assert_eq!(p("input")["description"], "Input path");
    let drop = &doc["operations"][3];
    assert_eq!(drop["access"], "write");
    let lines = &doc["operations"][2]["params"][0];
    assert_eq!(lines["name"], "lines");
    assert_eq!(lines["type"], "array");
    assert_eq!(lines["occurs"], "+");
}

fn legacy_invocations_stay_legacy() {
    let run = fixtool(&["stats", "f.csv", "--format", "json"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.json["input"], "f.csv");
    assert!(run.json.get("success").is_none());
    assert_eq!(fixtool(&[]).stdout, "fixtool legacy help\n");
    assert!(fixtool(&["--help"]).stdout.contains("A fixture CLI"), "clap's own help");
    assert_eq!(fixtool(&["serve"]).stdout, "serving\n");
}

// ---- argv rebuilding and the JSON switch --------------------------------------------------------

fn json_switch_and_argv_in_schema_order() {
    let run = fixtool(&[
        "stats_run",
        "--input",
        "-x.csv",
        "--level",
        "2",
        "--mode",
        "slow",
        "--tag",
        "t1",
        "--tag",
        "t2",
        "--columns",
        "a",
        "--columns",
        "b",
        "--verbose",
        "--limit",
        "10",
    ]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(
        child_argv(&run),
        [
            "stats",
            "--columns=a,b",
            "--tag=t1",
            "--tag=t2",
            "--verbose",
            "--mode=slow",
            "-v",
            "-v",
            "--format",
            "json",
            "--",
            "-x.csv"
        ],
        "default limit omitted, comma list joined, repeats kept, json switch before positionals"
    );
    assert_eq!(run.json["columns"], json!(["a", "b"]));
    assert_eq!(run.json["tags"], json!(["t1", "t2"]));
    assert_eq!(run.json["level"], 2);
    assert_eq!(run.json["limit"], 10);
    assert_eq!(run.json["verbose"], true);
    assert_eq!(run.json["input"], "-x.csv");

    let run = fixtool(&[
        "stats_run",
        "--input-json",
        r#"{"input":"in.csv","limit":3,"mode":"fast"}"#,
    ]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(child_argv(&run), ["stats", "--limit=3", "--format", "json", "in.csv"]);
}

fn comma_list_items_with_the_delimiter_are_refused() {
    let run = fixtool(&["stats_run", "--input", "a", "--columns", "x,y"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert_eq!(run.json["error"]["code"], "VALIDATION_ERROR");
    assert!(
        run.json["error"]["message"].as_str().unwrap().contains("columns"),
        "{}",
        run.stdout
    );
}

fn large_positional_values_pass_through() {
    // The child echoes its argv, so the stdout cap is raised for this call.
    let overlay = clap_fixture::OVERLAY.replace("max_stdout_bytes = 4096", "max_stdout_bytes = 1048576");
    let bridge = Bridge::new(clap_fixture::command(), &overlay).unwrap();
    let big = "p".repeat(100_000);
    let (code, out) = in_process(&bridge, &["stats_run", "--input", &big]);
    assert_eq!(code, 0, "{}", out["error"]);
    assert_eq!(out["input"].as_str().unwrap().len(), 100_000);
}

fn text_output_is_returned_under_the_text_key() {
    let run = fixtool(&["echo_text", "--words", "a", "--words", "-b"]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(run.json, json!({"text": "echo: a -b\n"}));
}

// ---- stdin ----------------------------------------------------------------------------------

fn stdin_params_are_piped_not_placed_in_argv() {
    let run = fixtool(&["data_load", "--source", "hello\nworld"]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(run.json["data"], "hello\nworld");
    assert_eq!(child_argv(&run), ["data", "load", "--format", "json", "-"]);
    let run = fixtool(&["data_lines", "--lines", "a", "--lines", "b", "--lines", "c"]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(run.json["count"], 3);
    assert_eq!(child_argv(&run), ["data", "lines", "--format", "json"]);
}

// ---- policy -----------------------------------------------------------------------------------

fn dangerous_writes_follow_policy() {
    let run = fixtool(&["data_drop", "--table", "t"]);
    assert_eq!(run.code, 9, "{}", run.stdout);
    assert_eq!(run.json["error"]["code"], "MISSING_CONFIRMATION");
    let run = fixtool(&["data_drop", "--table", "t", "--dryrun"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.json["dryRun"], true);
    assert_eq!(
        run.json["command"],
        json!(["fixtool", "data", "drop", "--format", "json", "t"])
    );
    let run = fixtool(&["data_drop", "--table", "t", "--confirm"]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(run.json["dropped"], "t");
    let run = fixtool(&["--readonly", "data_drop", "--table", "t", "--confirm"]);
    assert_eq!(run.code, 6);
    assert_eq!(run.json["error"]["code"], "READONLY_VIOLATION");
}

// ---- exit codes, signals, caps, timeout -----------------------------------------------------------

fn exit_codes_map_to_lmapi_errors() {
    let run = fixtool(&["exit_with", "--code", "4"]);
    assert_eq!(run.code, 4, "{}", run.stdout);
    let e = &run.json["error"];
    assert_eq!(e["code"], "NOT_FOUND");
    assert_eq!(e["message"], "fixtool failed with 4");
    assert_eq!(e["data"]["tool_code"], "EXIT_4");
    assert_eq!(e["data"]["legacy_exit_code"], 4);
    assert_eq!(e["data"]["result"], json!({"failed": 4}));
    let run = fixtool(&["exit_with", "--code", "3"]);
    assert_eq!(run.code, 0, "3 is a success code in the overlay");
    assert_eq!(run.json, json!({"failed": 3}));
    let run = fixtool(&["exit_with", "--code", "7"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.json["error"]["code"], "COMMAND_FAILED");
    assert_eq!(run.json["error"]["data"]["tool_code"], "EXIT_7");
    let run = fixtool(&["exit_with", "--code", "0"]);
    assert_eq!(run.code, 0);
}

fn a_signal_killed_child_never_succeeds() {
    // The child prints a valid success document, then aborts (SIGABRT).
    let run = fixtool(&["crash_now"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert_eq!(run.json["success"], false);
    assert_eq!(run.json["error"]["code"], "INTERNAL_ERROR");
    assert_eq!(run.json["error"]["data"]["tool_code"], "SIGNALED");
    #[cfg(unix)]
    assert_eq!(run.json["error"]["data"]["signal"], 6);
}

fn stdout_over_the_cap_is_an_error() {
    let run = fixtool(&["big_output", "--bytes", "100"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.json["text"].as_str().unwrap().len(), 100);
    let run = fixtool(&["big_output", "--bytes", "200000"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert_eq!(run.json["error"]["code"], "COMMAND_FAILED");
    assert_eq!(run.json["error"]["data"]["tool_code"], "OUTPUT_TOO_LARGE");
    assert_eq!(run.json["error"]["data"]["limit"], 4096);
}

fn a_child_over_the_deadline_is_killed() {
    let overlay = clap_fixture::OVERLAY.replace("timeout_ms = 5000", "timeout_ms = 200");
    let bridge = Bridge::new(clap_fixture::command(), &overlay).unwrap();
    let started = std::time::Instant::now();
    let (code, out) = in_process(&bridge, &["sleep_for", "--ms", "3000"]);
    assert!(started.elapsed().as_millis() < 2500, "killed at the deadline");
    assert_eq!(code, 7, "{out}");
    assert_eq!(out["error"]["code"], "TIMEOUT");
    let (code, out) = in_process(&bridge, &["sleep_for", "--ms", "10"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["slept"], true);
}

fn the_executable_is_overridable() {
    let bridge = bridge().exe("/nonexistent/fixtool");
    let (code, out) = in_process(&bridge, &["stats_run", "--input", "a"]);
    assert_eq!(code, 1);
    assert_eq!(out["error"]["code"], "INTERNAL_ERROR");
    assert!(
        out["error"]["message"].as_str().unwrap().contains("cannot start"),
        "{out}"
    );
    let bridge = bridge.exe(std::env::current_exe().unwrap());
    assert_eq!(in_process(&bridge, &["stats_run", "--input", "a"]).0, 0);
}

fn the_self_exec_child_is_never_lmapi() {
    // The child runs with the bridge's marker: even lmapi-shaped argv goes to the legacy parser.
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--schema"])
        .env(FIXTURE_ENV, "1")
        .env(lmrust::clap_bridge::CHILD_ENV, "1")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains("cli.schema.v1"));
}

// ---- legacy_argv and coverage ---------------------------------------------------------------

fn legacy_argv_is_public() {
    let plans = bridge().plans().unwrap();
    let stats = plans.iter().find(|p| p.name == "stats_run").unwrap();
    let params = json!({"input": "a", "verbose": true}).as_object().unwrap().clone();
    assert_eq!(legacy_argv(stats, &params).unwrap(), ["stats", "--verbose", "a"]);
    assert_eq!(
        bridge().child_argv(stats, &params).unwrap(),
        ["stats", "--verbose", "--format", "json", "a"],
        "with the overlay's JSON switch"
    );
}

fn coverage_passes_for_the_fixture() {
    assert_coverage(&clap_fixture::command(), clap_fixture::OVERLAY);
}

fn coverage_reports_every_rule() {
    let cmd = clap_fixture::command();
    let problems = |overlay: &str| coverage_problems(&cmd, overlay).join("\n");
    // A leaf neither published nor excluded.
    let unpublished = clap_fixture::OVERLAY.replace("excluded = [\"serve\"]", "excluded = []");
    assert!(
        problems(&unpublished).contains("`serve` is neither published nor excluded"),
        "{}",
        problems(&unpublished)
    );
    // A published name equal to a legacy subcommand name (any level).
    for legacy in ["echo", "load", "data"] {
        let clash = clap_fixture::OVERLAY.replace("name = \"echo_text\"", &format!("name = \"{legacy}\""));
        assert!(
            problems(&clash).contains("equals a legacy subcommand"),
            "{legacy}: {}",
            problems(&clash)
        );
    }
    // A missing access is an error, never read-only.
    let no_access = clap_fixture::OVERLAY.replacen("access = \"read\"\n", "", 1);
    assert!(problems(&no_access).contains("access"), "{}", problems(&no_access));
    // Unknown command, group instead of leaf, bad name, duplicate, dangerous read, stale exclusion.
    let bad = format!(
        "{}\n[[op]]\nname = \"x1\"\ncommand = \"nope\"\naccess = \"read\"\n[[op]]\nname = \"x2\"\ncommand = \"data\"\naccess = \"read\"\n\
         [[op]]\nname = \"BadName\"\ncommand = \"serve\"\naccess = \"read\"\n[[op]]\nname = \"stats_run\"\ncommand = \"stats\"\naccess = \"read\"\n",
        clap_fixture::OVERLAY.replace("excluded = [\"serve\"]", "excluded = [\"gone\"]")
    );
    let text = problems(&bad);
    for needle in [
        "no clap command `nope`",
        "`data` is a command group",
        "BadName",
        "duplicate op name stats_run",
        "excluded command `gone`",
    ] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
    let dangerous_read = clap_fixture::OVERLAY.replace(
        "command = \"stats\"\naccess = \"read\"",
        "command = \"stats\"\naccess = \"read\"\ndangerous = true",
    );
    assert!(
        problems(&dangerous_read).contains("dangerous"),
        "{}",
        problems(&dangerous_read)
    );
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| assert_coverage(&cmd, &unpublished)));
    std::panic::set_hook(hook);
    assert!(result.is_err(), "assert_coverage panics on problems");
}

fn main() {
    if std::env::var_os(FIXTURE_ENV).is_some() {
        std::process::exit(fixture_main());
    }
    // Children (the bridge's self-exec of this binary) inherit the fixture mode.
    std::env::set_var(FIXTURE_ENV, "1");
    let tests: &[(&str, fn())] = &[
        (
            "schema_is_deterministic_and_follows_lmapi",
            schema_is_deterministic_and_follows_lmapi,
        ),
        ("legacy_invocations_stay_legacy", legacy_invocations_stay_legacy),
        (
            "json_switch_and_argv_in_schema_order",
            json_switch_and_argv_in_schema_order,
        ),
        (
            "comma_list_items_with_the_delimiter_are_refused",
            comma_list_items_with_the_delimiter_are_refused,
        ),
        (
            "large_positional_values_pass_through",
            large_positional_values_pass_through,
        ),
        (
            "text_output_is_returned_under_the_text_key",
            text_output_is_returned_under_the_text_key,
        ),
        (
            "stdin_params_are_piped_not_placed_in_argv",
            stdin_params_are_piped_not_placed_in_argv,
        ),
        ("dangerous_writes_follow_policy", dangerous_writes_follow_policy),
        ("exit_codes_map_to_lmapi_errors", exit_codes_map_to_lmapi_errors),
        (
            "a_signal_killed_child_never_succeeds",
            a_signal_killed_child_never_succeeds,
        ),
        ("stdout_over_the_cap_is_an_error", stdout_over_the_cap_is_an_error),
        (
            "a_child_over_the_deadline_is_killed",
            a_child_over_the_deadline_is_killed,
        ),
        ("the_executable_is_overridable", the_executable_is_overridable),
        ("the_self_exec_child_is_never_lmapi", the_self_exec_child_is_never_lmapi),
        ("legacy_argv_is_public", legacy_argv_is_public),
        ("coverage_passes_for_the_fixture", coverage_passes_for_the_fixture),
        ("coverage_reports_every_rule", coverage_reports_every_rule),
    ];
    let filter: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, test) in tests {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        ran += 1;
        match std::panic::catch_unwind(*test) {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                println!("test {name} ... FAILED");
                failed.push(*name);
            }
        }
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        println!("failures: {failed:?}");
        std::process::exit(101);
    }
}
