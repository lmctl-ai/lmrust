# lmrust

The Rust SDK for [lmapi](https://github.com/lmctl-ai/lmapi), the API for the LLM era:
**a CLI is the server**, `<tool> --schema` publishes a `cli.schema.v1` document as the
contract, and an LLM or any program calls the tool through one validated command line.

`lmrust` is in parity with the TypeScript SDK [lmts](https://github.com/lmctl-ai/lmts) and
passes every lmapi conformance vector. Use it to

- **build an lmapi tool in Rust**: give `lmrust::server::run_cli` your operation schemas and
  one handler per operation; it serves `--schema`, `--help`, `--explain`, flags / JSON /
  GraphQL inputs, `--dryrun` / `--readonly` / `--confirm`, JSON error envelopes, exit codes
  and the product version check;
- **call lmapi tools from Rust**: validate and encode inputs, check published documents,
  interpret LLM-produced command lines safely (no shell, ever), render tool descriptions for
  prompts;
- **use `@owner/repo` schema modules** (feature `modules`): the same on-disk cache as lmts;
- **make an existing clap CLI lmapi-native** (feature `clap`): its `clap::Command` plus an
  overlay TOML become a published schema, and every operation self-execs the legacy CLI
  ([Clap bridge](#clap-bridge-make-a-clap-cli-lmapi-native)).

```toml
[dependencies]
lmrust = { git = "https://github.com/lmctl-ai/lmrust" }
# lmrust = { git = "https://github.com/lmctl-ai/lmrust", features = ["modules"] }
```

## Quick start: serve a tool

```rust,no_run
use lmrust::server::{check_policy, run_cli, CheckContext, CliSpec, OpDef, Policy};
use lmrust::{Access, OperationSchema, ParamSpec, ParamType};
use serde_json::{json, Value};

fn main() {
    let spec = CliSpec::with_policy("notes")
        .version("1.0.0")
        .op(OpDef::new("add", Access::Write, "Add a note", |params, policy: &Policy| {
            let text = params["text"].as_str().unwrap_or_default();
            let ctx = CheckContext::new("add").command(["notes", "add", text]);
            if let Some(simulated) = check_policy(Access::Write, policy, &ctx)? {
                return Ok(Value::Object(simulated)); // --dryrun
            }
            Ok(json!({ "added": text }))
        })
        .schema(OperationSchema::new("add").access(Access::Write).describe("Add a note")
            .param(ParamSpec::new("text", ParamType::String).describe("Note text"))));
    std::process::exit(run_cli(&spec, std::env::args().skip(1)));
}
```

```text
$ notes --schema                         # cli.schema.v1 document, exit 0
$ notes add --text hi                    # {"added": "hi"}
$ notes add --input-json '{"text":"hi"}' # same, JSON form
$ notes add --text hi --dryrun           # {"success": true, "dryRun": true, ...}
$ NOTES_READONLY=1 notes add --text hi   # READONLY_VIOLATION envelope, exit 6
$ notes add                              # VALIDATION_ERROR envelope, exit 2
```

**Native CLIs** (a clap tool that implements lmapi itself, lmapi protocol 6.1) keep their own
parser for everything that is not lmapi-shaped:

```rust,no_run
# use lmrust::server::{maybe_run_cli, CliSpec, ErrorCode, CliError};
# use lmrust::InputForm;
let spec = CliSpec::with_policy("probe")
    .version(env!("CARGO_PKG_VERSION"))   // no .product(): the tool is its own product
    .without_global("format")             // the tool owns --format
    .inputs([InputForm::Flags, InputForm::Json]);
let args: Vec<String> = std::env::args().skip(1).collect();
if let Some(code) = maybe_run_cli(&spec, args.clone()) {
    std::process::exit(code); // --schema, --input-json, <published op> ..., --explain <op>
}
// `probe --help`, `probe` and every other argv: the tool's own clap parser runs here.
# let _ = CliError::new(ErrorCode::NotFound, "x").tool_code("E_NXDOMAIN"); // data.tool_code
```

The guard rule, configurable globals, `inputs` subsets and `tool_code` are described in
[durable-memory/server-runtime.md](durable-memory/server-runtime.md#native-tools-lmapi-protocol-61-lmrust-additions).

A published document can be served directly: `CliSpec::from_document_with_policy(&doc).handle("add", handler)`.
A clap-based binary embeds the runner without a wrapper by passing its raw arguments
(`std::env::args().skip(1)`, or an `#[command(external_subcommand)]` vector). The complete
example is [`examples/minitool.rs`](examples/minitool.rs) (`cargo run --example minitool -- --help`).

## Quick start: client side

```rust
use lmrust::{interpret_command, parse_published_schema, validate, Effect, ValidationOptions};
use serde_json::json;

let doc = json!({
    "schema": "cli.schema.v1", "tool": "git2",
    "operations": [{ "name": "commit", "access": "write", "params": [
        { "name": "message", "type": "string" },
        { "name": "allowEmpty", "type": "boolean", "occurs": "?" }
    ]}]
});
let schema = parse_published_schema(&doc).expect("valid cli.schema.v1");

// An LLM-produced line: tokenized without a shell, validated, classified.
let call = interpret_command(&[schema.clone()], r#"git2 commit --message "first" --allow-empty"#, &[]).unwrap();
assert_eq!(call.argv, ["git2", "commit", "--message", "first", "--allow-empty"]);
assert_eq!(call.effect, Effect::Effectful);
assert!(interpret_command(&[schema.clone()], "git2 commit --message x; rm -rf /", &[]).is_err());

// The single validator, for any of the three input forms.
let op = &schema.operations[0];
let issues = validate(op, &json!({"mesage": "x"}).as_object().unwrap(), ValidationOptions::default()).unwrap_err();
assert_eq!(issues[0].code.as_str(), "UNKNOWN_PARAM");
assert_eq!(issues[0].suggestion.as_deref(), Some("message"));
```

## Feature flags

| Feature | Default | Adds | Dependencies |
|---|---|---|---|
| (core) | yes | types, validate, argv/JSON/GraphQL forms, encode, help, publish, interpret, version, `server` | serde, serde_json (`preserve_order`), regex-lite |
| `preserve_order` | yes | insertion-ordered JSON objects, as lmts (`serde_json/preserve_order`) | (feature of serde_json) |
| `modules` | no | `lmrust::modules`: the `@owner/repo[#latest\|#dev]` schema store | + ureq (rustls, HTTPS only), sha2 |
| `clap` | no | `lmrust::clap_bridge`: a clap command tree + overlay TOML served as a native lmapi tool | + clap 4.5 (`std` only), toml 0.8 |

**Embedding in a binary with its own JSON output:** Cargo unifies features, so the default
`preserve_order` also switches every `serde_json::Value` the host binary prints from sorted keys
to insertion order. A tool whose existing output relies on sorted keys (golden files, byte-stable
envelopes) depends on lmrust with `default-features = false`:

```toml
lmrust = { git = "https://github.com/lmctl-ai/lmrust", tag = "v0.1.3", default-features = false }
```

Everything works the same without it; only the key order of JSON objects lmrust builds (e.g. the
order `encode_argv` emits flags in, or keys inside `data` records) follows sorted order instead of
insertion order.

No async runtime anywhere. MSRV: Rust 1.79 (CI runs the suite on 1.79 and stable).

## Clap bridge: make a clap CLI lmapi-native

With feature `clap`, a tool crate needs only its `clap::Command`, an overlay TOML and a few
lines of `main`. Legacy invocations are untouched (lmapi protocol 6.1); lmapi-shaped ones
(`--schema`, a published op name, `--input-json`, ...) are served from the clap tree, and each
operation runs the tool itself (`std::env::current_exe()`) with the legacy argv rebuilt from the
validated params.

```toml
lmrust = { git = "https://github.com/lmctl-ai/lmrust", tag = "v0.1.3", features = ["clap"] }
```

```rust,ignore
fn main() {
    let bridge = lmrust::clap_bridge::Bridge::new(cli::command(), include_str!("../lmapi/overlay.toml"))
        .expect("lmapi overlay")
        .version(env!("CARGO_PKG_VERSION"));
    if let Some(code) = bridge.maybe_run(std::env::args_os().skip(1)) {
        std::process::exit(code);
    }
    cli::run(); // the tool's own clap parsing, unchanged
}

#[test]
fn lmapi_overlay_covers_the_cli() {
    lmrust::clap_bridge::assert_coverage(&cli::command(), include_str!("../lmapi/overlay.toml"));
}
```

```toml
# lmapi/overlay.toml
drop_globals = ["format"]                 # the tool owns --format; lmapi does not publish its own
legacy_json_args = ["--format", "json"]   # the tool's JSON switch, added to every self-exec call
exclude_params = ["format"]               # clap args never published
excluded = ["serve"]                      # clap commands never published
success_exit_codes = [0]                  # other exits are errors ...
timeout_ms = 600000                       # optional deadline (TIMEOUT, exit 7)

[exit_codes]
"4" = "NOT_FOUND"                         # ... COMMAND_FAILED unless mapped here

[[op]]
name = "stats_summary"                    # published name: snake_case, never a legacy subcommand name
command = "stats"                         # clap command path ("data load" for nested commands)
access = "read"                           # required: read | write | dynamic

[[op]]
name = "data_import"
command = "data load"
access = "write"
dangerous = true                          # needs --confirm (exit 9 without it)
[op.params.source]
stdin = true                              # value piped to the child; its argv slot gets "-"
```

- **Schema**: one operation per `[[op]]`, generated from clap: long flags (camelCase names),
  shorts and visible aliases, `occurs` (`1 ? + *`), number / boolean / enum types, defaults,
  positionals and help text. Deterministic: ops in overlay order, params in clap order with
  positionals last. `tool` and `version` come from clap (or the overlay / `Bridge::version`),
  `inputs` defaults to flags + JSON, `product` is omitted.
- **Calls**: comma lists (`value_delimiter`) are joined, repeated flags (`Append`) repeated,
  counts repeated, booleans passed as bare flags, values equal to the default left out.
  JSON on the child's stdout is the result; other output is returned as `{"text": "..."}`
  (`text_key`). Stdout is capped (`max_stdout_bytes`, default 16 MiB: `OUTPUT_TOO_LARGE`),
  stderr truncated (`max_stderr_bytes`).
- **Failures**: an exit code outside `success_exit_codes` becomes `exit_codes[code]` or
  `COMMAND_FAILED` (a Rust panic, 101, is `INTERNAL_ERROR`) with `data.tool_code = "EXIT_<n>"`,
  `data.legacy_exit_code`, `data.stderr` and the parsed stdout under `data.result`. A child
  killed by a signal is `INTERNAL_ERROR` / `SIGNALED`, even when it printed a result first.
- **Inline tables**: `[inline_inputs.file]` with `[[inline_inputs.file.form]] param = "records", kind = "records",
  args = ["--from", "json"]` lets every op that reads `file` take rows (or CSV text) inline instead of a path:
  the value is piped to stdin and the slot gets `-`. `named_positionals = true` publishes clap positionals as
  named params, so a library-style call passes `{file: ...}` or `{records: [...]}` by name.
- **Result files and file configs**: `[op.result_file] param = "outputPath", name = "r.csv", format = "csv"`
  turns a command that only writes files into one that returns data: the bridge passes a temp path, parses
  the file (CSV rows, JSON or text) as the result and removes it. An inline form with `via = "file"` writes
  its value (JSON, text or `kind = "toml"` from a map) to a temp file whose path the command reads.
- **Policy**: write and dangerous ops follow `--dryrun`, `--readonly`, `--confirm` and the
  `<TOOL>_READONLY` / `<TOOL>_DRYRUN` env floors; a `dynamic` op is a write unless the tool
  registers `Bridge::dynamic_access(op, |params| ...)`.

Full overlay reference: [durable-memory/clap-bridge.md](durable-memory/clap-bridge.md).
Consumers on Rust 1.79 need clap 4.5 in their lockfile (clap 4.6 requires Rust 1.85).

## Parity with lmts

| lmts | lmrust | Notes |
|---|---|---|
| `types.ts`, `schema.ts`, `synopsis.ts` | `types`, `synopsis`, `is_effectful` | serde types with builders |
| `validate()`, `validateParams()` | `validate()`, `validate_params()` | `Outcome<T> = Result<T, Vec<ValidationIssue>>` |
| `parseArgvForm`, `coerceJsonLiteral` | `parse_argv_form`, `coerce_json_literal` | |
| `parseJsonDocument/Input`, `parseGraphqlDocument/Input` | same, snake_case | GraphQL offsets count chars, not UTF-16 units |
| `resolveInput` | `resolve_input(ResolveArgs)` | |
| `encodeArgv/Json/Graphql` | `encode_argv/json/graphql` | GraphQL returns `Result<_, EncodeError>` |
| `renderOpHelp`, `renderToolHelp` | `render_op_help`, `render_tool_help` | byte-identical text |
| `publishSchema`, `parsePublishedSchema`, `schemaWarnings`, `findOperation` | same, snake_case | typed document; see limitations |
| `tokenizeSimpleCommand`, `interpretCommand`, `classifyOperation`, `describeForPrompt` | same, snake_case | prompt text matches the lmapi golden files |
| `parseVersion`, `satisfies`, `isValidSupportedRange`, `countRegExpGroups` | same, snake_case | in-house range matcher; regex-lite pattern syntax |
| `./server`: `runCli`, `GLOBAL_FLAGS`, `checkPolicy`, `CliError`/`ErrorCode`, envelopes | `server::run_cli` / `run_cli_io`, `global_flags()`, `check_policy`, `CliError`/`ErrorCode` | sync handlers; panics become `UNKNOWN_ERROR` |
| `spawnCommand` (Bun/Node) | `server::spawn_command` | std::process; `AtomicBool` cancel instead of `AbortSignal` |
| `checkProductVersion` | `server::check_product_version` | same cache, PATH rules and messages |
| `./modules` (store) | `modules` (feature) | same layout and `meta.json`; `Fetcher` trait instead of `fetch` |
| `./validate` light entry | (not needed) | the core has no heavy dependencies |
| (none) | `is_lmapi_invocation`, `maybe_run_cli`, `without_global` / `rename_global`, `inputs([...])`, `CliError::tool_code` | lmrust additions for native tools (lmapi protocol 3 and 6.1) |

Known gaps and deliberate differences are listed in
[durable-memory/index.md](durable-memory/index.md#known-limitations).

## Conformance

`tests/conformance.rs` runs every vector in lmapi's `conformance/` directory (validate, argv, encode,
interpret, publish, prompt, spec, fixtures), comparing issue codes and param/op identity, never
message text. It looks for `$LMAPI_DIR/conformance`, default `../lmapi`, and fails in CI
(`GITHUB_ACTIONS`) when the directory is missing.

```sh
git clone https://github.com/lmctl-ai/lmapi ../lmapi
cargo test --all-features --test conformance -- --nocapture
```

## Property and Differential Tests

- **Property tests** (`tests/property.rs`, in-tree pure Rust, no Node required):
  1. `encode -> parse` round-trip for every fixture op under random values (JSON, GraphQL, argv).
  2. `validate` idempotence (validating already-validated output yields identical output).
  3. Tokenizer fuzzing ($10^5$ random strings without panic).
  4. `parse_published_schema(publish(schema))` round-trip across synthetic schemas.
  5. Server `run_cli_io` argv fuzzing ($10^4$ inputs, valid exit code, no panic).

- **Differential tests** (`tests/differential.rs`, compares lmrust with reference `@lmctl-ai/lmts` via `tests/support/lmts-oracle.mjs`):
  - Over 2,500 deterministic pseudo-random cases across `validate`, `parse_argv_form`, `json_form`, `graphql_form`, `encode` round-trips, and `interpret_command`.
  - Compares canonical JSON of results and `(code, param)` of issues.
  - Skips locally with a clear message when `LMTS_DIR` / `node` is absent; fails in CI when `GITHUB_ACTIONS` is set.
  - CI checks out `lmts` pinned to `LMTS_REF` and runs the suite in under 15 seconds.
  - Known divergences are minimized and preserved as `#[ignore = "divergence: ..."]` regression tests.

```sh
# Run property tests (no external dependencies)
cargo test --test property

# Run differential tests against reference lmts checkout
LMTS_DIR=../lmts cargo test --test differential -- --nocapture
```

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
LMTS_DIR=../lmts cargo test --all-features   # includes differential suite
cargo llvm-cov --all-features --summary-only   # line coverage
cargo +1.79 test --locked --all-features      # MSRV
```

`Cargo.lock` is committed and pinned to versions that build on Rust 1.79. When updating it,
keep it MSRV-compatible: `CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback cargo update`.

Design notes per module live in [`durable-memory/`](durable-memory/index.md).

## License

MIT, see [LICENSE](LICENSE).
