//! A small clap CLI (`fixtool`) used by `tests/clap_bridge.rs`: its own test binary is the
//! fixture executable (`CLAP_FIXTURE=1`), so the bridge self-execs a real clap program.

use clap::{value_parser, Arg, ArgAction, ArgMatches, Command};
use std::io::{Read, Write};

pub const OVERLAY: &str = r#"
tool = "fixtool"
description = "Fixture tool for the clap bridge"
drop_globals = ["format"]
legacy_json_args = ["--format", "json"]
excluded = ["serve"]
exclude_params = ["format"]
success_exit_codes = [0, 3]
timeout_ms = 5000
max_stdout_bytes = 4096

[exit_codes]
"4" = "NOT_FOUND"

[[op]]
name = "stats_run"
command = "stats"
access = "read"

[[op]]
name = "data_load"
command = "data load"
access = "read"
[op.params.source]
stdin = true

[[op]]
name = "data_lines"
command = "data lines"
access = "read"
[op.params.lines]
stdin = true
list = true
description = "Lines fed on stdin"

[[op]]
name = "data_drop"
command = "data drop"
access = "write"
dangerous = true

[[op]]
name = "echo_text"
command = "echo"
access = "read"
legacy_json = false

[[op]]
name = "exit_with"
command = "fail"
access = "read"

[[op]]
name = "crash_now"
command = "crash"
access = "read"

[[op]]
name = "big_output"
command = "big"
access = "read"

[[op]]
name = "sleep_for"
command = "sleep"
access = "read"
"#;

pub fn command() -> Command {
    Command::new("fixtool")
        .version("9.9.9")
        .about("A fixture CLI")
        .arg(
            Arg::new("format")
                .long("format")
                .global(true)
                .value_parser(["text", "json"])
                .default_value("text")
                .help("Output format"),
        )
        .subcommand(
            Command::new("stats")
                .about("Summarise a file")
                .arg(Arg::new("input").required(true).help("Input path"))
                .arg(
                    Arg::new("columns")
                        .long("columns")
                        .value_delimiter(',')
                        .action(ArgAction::Append)
                        .help("Comma list"),
                )
                .arg(
                    Arg::new("tag")
                        .long("tag")
                        .short('t')
                        .action(ArgAction::Append)
                        .help("Repeated tag"),
                )
                .arg(
                    Arg::new("limit")
                        .long("limit")
                        .value_parser(value_parser!(u32))
                        .default_value("10"),
                )
                .arg(
                    Arg::new("verbose")
                        .long("verbose")
                        .action(ArgAction::SetTrue)
                        .visible_alias("loud"),
                )
                .arg(
                    Arg::new("mode")
                        .long("mode")
                        .value_parser(["fast", "slow"])
                        .default_value("fast"),
                )
                .arg(Arg::new("level").short('v').action(ArgAction::Count)),
        )
        .subcommand(
            Command::new("data")
                .about("Data commands")
                .subcommand(
                    Command::new("load")
                        .about("Load data")
                        .arg(Arg::new("source").required(true)),
                )
                .subcommand(Command::new("lines").about("Count stdin lines"))
                .subcommand(
                    Command::new("drop")
                        .about("Drop a table")
                        .arg(Arg::new("table").required(true))
                        .arg(Arg::new("cascade").long("cascade").action(ArgAction::SetTrue)),
                ),
        )
        .subcommand(
            Command::new("echo")
                .about("Echo words")
                .arg(Arg::new("words").num_args(1..).last(true)),
        )
        .subcommand(
            Command::new("fail").about("Exit with a code").arg(
                Arg::new("code")
                    .long("code")
                    .value_parser(value_parser!(i32))
                    .required(true),
            ),
        )
        .subcommand(Command::new("crash").about("Abort (killed by SIGABRT)"))
        .subcommand(
            Command::new("big").about("Print a lot").arg(
                Arg::new("bytes")
                    .long("bytes")
                    .value_parser(value_parser!(u64))
                    .required(true),
            ),
        )
        .subcommand(
            Command::new("sleep").about("Sleep").arg(
                Arg::new("ms")
                    .long("ms")
                    .value_parser(value_parser!(u64))
                    .required(true),
            ),
        )
        .subcommand(Command::new("serve").about("Not published"))
}

fn json_out(m: &ArgMatches, value: serde_json::Value) {
    if m.get_one::<String>("format").map(String::as_str) == Some("json") {
        println!("{value}");
    } else {
        println!("{}", value.to_string().replace(['{', '}', '"'], ""));
    }
}

fn strings(m: &ArgMatches, id: &str) -> Vec<String> {
    m.get_many::<String>(id)
        .map(|v| v.cloned().collect())
        .unwrap_or_default()
}

/// The legacy behaviour of `fixtool`; returns the process exit code.
pub fn legacy_main(args: Vec<String>) -> i32 {
    let matches = match command().try_get_matches_from(std::iter::once("fixtool".to_string()).chain(args.clone())) {
        Ok(m) => m,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    let argv: Vec<serde_json::Value> = args.iter().map(|a| serde_json::json!(a)).collect();
    match matches.subcommand() {
        Some(("stats", m)) => json_out(
            m,
            serde_json::json!({
                "argv": argv,
                "input": m.get_one::<String>("input"),
                "columns": strings(m, "columns"),
                "tags": strings(m, "tag"),
                "limit": m.get_one::<u32>("limit"),
                "verbose": m.get_flag("verbose"),
                "mode": m.get_one::<String>("mode"),
                "level": m.get_count("level"),
            }),
        ),
        Some(("data", d)) => match d.subcommand() {
            Some(("load", m)) => {
                let source = m.get_one::<String>("source").cloned().unwrap_or_default();
                let mut text = String::new();
                if source == "-" {
                    let _ = std::io::stdin().read_to_string(&mut text);
                } else {
                    text = format!("file:{source}");
                }
                json_out(m, serde_json::json!({"argv": argv, "data": text}));
            }
            Some(("lines", m)) => {
                let mut text = String::new();
                let _ = std::io::stdin().read_to_string(&mut text);
                json_out(m, serde_json::json!({"count": text.lines().count(), "argv": argv}));
            }
            Some(("drop", m)) => json_out(m, serde_json::json!({"dropped": m.get_one::<String>("table")})),
            _ => return 2,
        },
        Some(("echo", m)) => println!("echo: {}", strings(m, "words").join(" ")),
        Some(("fail", m)) => {
            let code = *m.get_one::<i32>("code").unwrap();
            eprintln!("first line\nfixtool failed with {code}");
            println!("{}", serde_json::json!({"failed": code}));
            return code;
        }
        Some(("crash", m)) => {
            json_out(m, serde_json::json!({"status": "ok"}));
            let _ = std::io::stdout().flush();
            std::process::abort();
        }
        Some(("big", m)) => {
            let n = *m.get_one::<u64>("bytes").unwrap() as usize;
            print!("{}", "x".repeat(n));
        }
        Some(("sleep", m)) => {
            std::thread::sleep(std::time::Duration::from_millis(*m.get_one::<u64>("ms").unwrap()));
            json_out(m, serde_json::json!({"slept": true}));
        }
        Some(("serve", _)) => println!("serving"),
        _ => {
            println!("fixtool legacy help");
        }
    }
    0
}
