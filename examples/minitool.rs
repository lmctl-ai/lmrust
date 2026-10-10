//! A complete lmapi tool in one file: `minitool` serves its schema, help, three input
//! forms, policy flags and JSON envelopes through `lmrust::server::run_cli`.
//!
//! ```text
//! cargo run --example minitool -- --schema
//! cargo run --example minitool -- greet --name Ada --times 2
//! cargo run --example minitool -- greet --input-json '{"name":"Ada","shout":true}'
//! cargo run --example minitool -- save_note --text hello --dryrun
//! MINITOOL_READONLY=1 cargo run --example minitool -- save_note --text hello   # exit 6
//! cargo run --example minitool -- greet                                      # exit 2, envelope
//! ```
//!
//! A clap-based binary embeds the same runner by handing it the raw arguments, e.g. from an
//! `#[command(external_subcommand)]` variant or straight from `std::env::args().skip(1)`.

use lmrust::server::{check_policy, run_cli, CheckContext, CliError, CliSpec, ErrorCode, OpDef, Policy};
use lmrust::{Access, OperationSchema, ParamSpec, ParamType};
use serde_json::{json, Value};

fn spec() -> CliSpec<Policy> {
    CliSpec::with_policy("minitool")
        .version(env!("CARGO_PKG_VERSION"))
        .description("Example lmapi tool built with lmrust")
        .op(OpDef::new("greet", Access::Read, "Greet someone", |params, _policy| {
            let name = params["name"].as_str().unwrap_or_default();
            let times = params["times"].as_i64().unwrap_or(1).max(0) as usize;
            let shout = params.get("shout") == Some(&Value::Bool(true));
            let mut line = format!("hello, {name}");
            if shout {
                line = line.to_uppercase();
            }
            Ok(json!({ "greeting": vec![line; times] }))
        })
        .schema(
            OperationSchema::new("greet")
                .access(Access::Read)
                .describe("Greet someone")
                .param(
                    ParamSpec::new("name", ParamType::String)
                        .positional()
                        .describe("Who to greet"),
                )
                .param(
                    ParamSpec::new("times", ParamType::Number)
                        .optional()
                        .with_default(json!(1))
                        .describe("Repeat count"),
                )
                .param(
                    ParamSpec::new("shout", ParamType::Boolean)
                        .optional()
                        .describe("Upper-case the greeting"),
                ),
        ))
        .op(OpDef::new(
            "save_note",
            Access::Write,
            "Pretend to save a note",
            |params, policy: &Policy| {
                let text = params["text"].as_str().unwrap_or_default();
                if text.is_empty() {
                    return Err(CliError::new(ErrorCode::InvalidRequest, "text must not be empty"));
                }
                let ctx = CheckContext::new("save_note").command(["notes", "add", text]);
                if let Some(simulated) = check_policy(Access::Write, policy, &ctx)? {
                    return Ok(Value::Object(simulated));
                }
                Ok(json!({ "saved": true, "bytes": text.len() }))
            },
        )
        .schema(
            OperationSchema::new("save_note")
                .access(Access::Write)
                .describe("Pretend to save a note")
                .param(ParamSpec::new("text", ParamType::String).describe("Note text")),
        ))
}

fn main() {
    std::process::exit(run_cli(&spec(), std::env::args().skip(1)));
}
