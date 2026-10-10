# Server runtime — `server/`

`run_cli(&spec, args)` / `run_cli_io(&spec, &argv, &mut out, &env)` port lmts `runCli`.

## CliSpec

- `CliSpec::new(tool, |policy| ctx)` + `.op(OpDef::new(name, access, description, handler).schema(op_schema))`.
- `CliSpec::from_document(&doc, |policy| ctx).handle("op", handler)`: serve a published
  document directly; access comes from the document (missing = `write`), an op without a
  handler answers `INTERNAL_ERROR`.
- `CliSpec::with_policy(tool)` / `from_document_with_policy(&doc)`: the handler context is
  the `Policy` itself.
- Handlers: `Fn(&Record, &C) -> Result<Value, CliError>`. A panic becomes `UNKNOWN_ERROR`
  (exit 1), like a thrown generic Error in lmts.
- Embedding: a clap (or any) binary passes its raw arguments: `std::process::exit(run_cli(&spec, std::env::args().skip(1)))`
  or an `external_subcommand` vector. No wrapper is needed.

## Flow (same order as lmts)

1. No args: tool help. Find the op token (skipping global flags and their values; `--` makes
   the next token the op).
2. No op: `--help`/`-h`, `--list`, `--schema`, `--explain <op> [--format text]`, else infer the
   op from a JSON envelope or the GraphQL field, else `INVALID_REQUEST` "No operation specified".
3. Op with `--help`/`-h`: op help.
4. Product check (when `spec.product`): failure is `UNSUPPORTED_PRODUCT_VERSION`, exit 10,
   before the unknown-op check.
5. Unknown op: `INVALID_REQUEST` with a `--list` hint.
6. Documents may accompany globals but not op flags/positionals (`MIXED_INPUT`).
7. Policy from flags and env floors, `resolve_input`, issues -> `VALIDATION_ERROR` (exit 2,
   `data: {op, issues}`, UNKNOWN_PARAM synopsis rewritten to the op synopsis).
8. `build_context(policy)`, handler, result printed as pretty JSON (`JSON.stringify(r, null, 2)`).

## Errors and exit codes

`ErrorCode` strings and exit codes are lmapi protocol section 6 (INVALID_REQUEST/VALIDATION_ERROR 2,
PARSE_ERROR 3, NOT_FOUND 4, CONFLICT 5, FORBIDDEN/READONLY_VIOLATION 6, TIMEOUT 7,
SERVICE_UNAVAILABLE 8, MISSING_CONFIRMATION 9, UNSUPPORTED_PRODUCT_VERSION 10, COMMAND_NOT_FOUND 127,
others 1). Envelope: `{ "success": false, "error": { "code", "message", "data"? } }` on stdout.

## Exec and product

- `spawn_command(cmd, args, cwd, env, ExecOptions{timeout, allow_non_zero_exit, cancel})`:
  never a shell, env replaces the parent's, stdin closed, output read on threads (no pipe
  deadlock), deadline kills the child. Non-zero exit is `ExecError::NonZeroExit` (text
  `Exit Code: n\nStderr: ...\nStdout: ...`) unless allowed.
- `check_product_version(product, opts)`: `detect.argv[0]` resolved on absolute `PATH`
  entries only (env `PATH`, else the process `PATH`), run with a 3 s deadline, first capture of
  `detect.pattern` in the first 4096 characters of `stdout\nstderr`, `satisfies(found, supported)`.
  Cached per product name for the process (`reset_product_version_cache()` for tests).
