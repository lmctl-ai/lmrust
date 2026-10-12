# Clap bridge — `clap_bridge/` (feature `clap`)

Generalised from lmprobe's lmapi bridge (`lmprobe/crates/lmprobe/src/lmapi/`): a clap
`Command` plus an overlay TOML becomes a native lmapi tool (protocol 6.1). lmts has no
counterpart; this module is lmrust-only.

## Files

- `overlay.rs`: `Overlay` (serde, `deny_unknown_fields`) and its clap-free rules (`problems()`).
- `generate.rs`: clap tree -> `OpPlan` (schema + `Binding`s that rebuild the legacy argv).
- `child.rs`: the only process spawn (self-exec): stdin pipe, capped readers, deadline.
- `mod.rs`: `Bridge`, `legacy_argv`, the per-call handler (policy, stdin, outcome mapping).
- `coverage.rs`: `coverage_problems` / `assert_coverage` for the tool's own tests.
- Tests: unit tests in each file; `tests/clap_bridge.rs` (`harness = false`) is also the fixture
  binary (`CLAP_FIXTURE=1` runs `tests/clap_fixture/`), so self-exec of `current_exe()` is real.

## Overlay reference

Top level (all optional):

| key | default | meaning |
|---|---|---|
| `tool` | clap name | published tool name, env floor prefix (`<TOOL>_READONLY`) |
| `description` | clap `about` | tool help summary |
| `inputs` | `["flags", "json"]` | published input forms |
| `drop_globals` | `[]` | lmapi runner globals the tool owns (`format`) |
| `excluded` | `[]` | clap command paths never published; a first word excludes a group |
| `exclude_params` | `[]` | clap arg ids never published |
| `only_for` | `{}` | `arg_id = ["op", ...]`: publish a (global) arg only for these ops |
| `legacy_json_args` | `[]` | the tool's JSON switch (`["--json"]`, `["--format", "json"]`) |
| `legacy_root_args` | `[]` | args placed before the command words (root-only options) |
| `text_key` | `"text"` | result key for non-JSON stdout |
| `success_exit_codes` | `[0]` | child exits that are results (e.g. `[0, 1]` for grep-like "no match") |
| `exit_codes` | `{}` | `"<exit>" = "<LMAPI_CODE>"` for failures |
| `max_stdout_bytes` | 16 MiB | more is `COMMAND_FAILED` / `OUTPUT_TOO_LARGE` |
| `max_stderr_bytes` | 64 KiB | stderr beyond is dropped |
| `timeout_ms` | none | kill the child, `TIMEOUT` (exit 7) |
| `named_positionals` | `false` | publish clap positionals as named params (library-style calls); argv still positional |
| `inline_inputs` | `{}` | `[inline_inputs.<param>]` + `[[inline_inputs.<param>.form]]`: a file-like input may be passed inline instead |

`[[op]]`: `name` (snake_case, required), `command` (clap path, required), `access`
(`read|write|dynamic`, REQUIRED: a missing access is a parse error, never read-only), `dangerous`
(write/dynamic only), `description`, `exclude_params`, `fixed_args`, `common_options` (false: no
root-level arg is published), `legacy_json` (false: no JSON switch for this op),
`[op.params.<publishedName>]` with `stdin`, `stdin_token` (default `-`, `""` = no argv slot),
`description`, `list`.

`[inline_inputs.<param>]`: `token` (default `-`, what the input's argv slot receives), `description` (the input
param's description when the clap arg has no help text) and one or more
`[[inline_inputs.<param>.form]]` with `param` (published name), `kind` (`records`: array of objects written as a
compact JSON array; `text`: a string written as is; `json`: any value as compact JSON), `args` (legacy args naming
the stdin format, e.g. `["--from", "json"]`) and `description`. Every op that has `<param>` gets the forms as
optional params and `<param>` itself becomes optional and publishes no default (validation would fill it in and collide
with a form; clap still applies the default when no source is given); a call gives exactly one of them (`VALIDATION_ERROR`
`give only one of ...` / `one of ... is required` when the clap arg was required). With a form, its value is
piped to stdin, the input's slot gets the token and the form's args follow the fixed args (before the JSON
switch). Plan errors: an op with two inline inputs, an inline input plus a stdin param, a multi-value input,
a form name that clashes with an op param. This is how a script passes rows it computed without a temp file.

Form keys added for lmsim: `via = "stdin"` (default) | `"file"` (the value is written to `<temp dir>/<file_name>`
and that path takes the input's argv slot: a tool that only reads config files), `file_name`, and `ops` (published
op names the form applies to; an op left with no form keeps a plain path input). Kind `toml` writes a JSON object
as TOML (`toml::to_string`; a null is a `VALIDATION_ERROR`). The stdin-conflict plan error applies only to stdin forms.

`[op.result_file]`: `param` (published name of the command's output-path arg; no longer published), `name` (file
name passed as `<temp dir>/<name>`; absent: the temp dir itself), `read` (file to parse when it is not `name`, e.g.
a `x.csv.summary.json` sidecar), `format` (`csv`: RFC 4180 header + rows as an array of objects, cells typed as
number/boolean/null(empty)/string; `json`; `text`: `{text_key: ...}`). Every call gets a private temp dir
(`lmrust-clap-bridge-<pid>-<nanos>-<n>` under the OS temp dir, removed on drop, also after failures); after a
successful run the file is the result (a missing file is `COMMAND_FAILED` / `NO_RESULT_FILE`, an unparsable one
`BAD_RESULT_FILE`). Files the command derives from the output path (sidecars) land in the same dir. Access is
still the overlay's: an op is only `read` when nothing it writes can land outside that dir.

## Rules and decisions

- **argv order**: `legacy_root_args`, then each command word followed by its level's flags (clap
  order), `fixed_args`, `legacy_json_args`, then positionals by clap index. `--` precedes the
  positionals when one starts with `-`, a positional is `last`, or a `num_args > 1` option
  without delimiter/append was emitted (it would swallow them). Long flags use `--name=value`.
- **Values**: equal-to-default values are omitted (clap applies them). `value_delimiter` lists are
  joined into one argument; an item containing the delimiter is `VALIDATION_ERROR` (it would
  split differently). `Append` repeats the flag. `Count` repeats it `n` times. `SetTrue` and
  `SetFalse` flags are passed when the param is `true`. A required `SetTrue` marker becomes a
  fixed arg (a published boolean could only be `true`).
- **Stdin**: one stdin param per op. A clap-backed param keeps its schema; its slot gets `-`. A
  param that is not a clap arg is added to the schema (string, or `list = true`: array `+`, one
  line per item) and has no argv slot. No stdin param: the child's stdin is null.
- **Results**: stdout parsed as JSON (trimmed) is the result; otherwise `{text_key: stdout}`.
  On success stderr is dropped. Failure `data`: `tool_code` (`EXIT_<n>`, `PANIC`, `SIGNALED`,
  `TIMEOUT`, `OUTPUT_TOO_LARGE`), `legacy_exit_code`, `stderr`, `result` or `text_key`. The message
  is the last stderr line.
- **Order of checks** on a finished child: timeout, signal (never success), stdout overflow,
  success codes, `exit_codes`, panic (101), `COMMAND_FAILED`.
- **No recursion**: the child gets `LMRUST_CLAP_BRIDGE_CHILD=1` (`CHILD_ENV`); `maybe_run` returns
  `None` there, so legacy argv that happens to look lmapi-shaped is never re-entered.
- **Dispatch guard**: `Bridge::is_lmapi_invocation` uses `server::is_lmapi_invocation` on a stub
  spec built from the overlay only, so legacy argv never pays for the clap walk.
- **Coverage** (`assert_coverage`): overlay parse/rules; each op resolves to a clap leaf and is
  not excluded; no op name equals a legacy subcommand name or alias at any depth; every clap leaf
  (except clap's `help`) is published or excluded; stale `excluded` entries; the generated
  document passes `parse_published_schema`.
- **MSRV**: clap 4.5.x (MSRV 1.74) is locked; clap 4.6 needs 1.85, so the requirement `4.5` lets
  newer consumers take 4.6 while 1.79 consumers pin `cargo update -p clap --precise 4.5.61`.

## Not generalised (tool-specific, stays in the tool)

- lmprobe's result-document protocol (`--outfile`, `lmprobe.result.v1`, `status` ok / no_match /
  partial, diagnostics -> `tool_code` and the legacy code table): the bridge only sees exit codes
  and stdout JSON. A tool with such a protocol maps it with `success_exit_codes`/`exit_codes`, or
  keeps its own handler.
- Mutation detection for a `dynamic` op (lmprobe's GraphQL `mutation` scan): `Bridge::dynamic_access`.
- `PR_SET_PDEATHSIG` (child dies with the parent, Linux): lmrust has no `unsafe`/libc; a killed
  parent can orphan the child. A timed-out child is killed, but its own children are not.
- A timed-out child's grandchildren that keep the pipes open delay the reader threads.
