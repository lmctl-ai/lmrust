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

`[[op]]`: `name` (snake_case, required), `command` (clap path, required), `access`
(`read|write|dynamic`, REQUIRED: a missing access is a parse error, never read-only), `dangerous`
(write/dynamic only), `description`, `exclude_params`, `fixed_args`, `common_options` (false: no
root-level arg is published), `legacy_json` (false: no JSON switch for this op),
`[op.params.<publishedName>]` with `stdin`, `stdin_token` (default `-`, `""` = no argv slot),
`description`, `list`.

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
