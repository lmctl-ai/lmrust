# Testing Strategy: Conformance, Property and Differential Tests

lmrust uses a three-tier test methodology to guarantee full protocol compliance, behavioral parity with the TypeScript reference SDK (`@lmctl-ai/lmts`), and robust resilience against hostile or malformed inputs.

## 1. Conformance Vectors (`tests/conformance.rs`)

Normative test vectors provided by [lmapi](https://github.com/lmctl-ai/lmapi).
- **Location**: `$LMAPI_DIR/conformance`, defaulting to sibling `../lmapi`.
- **Policy**: Skips with a clear message if missing during local development, but **fails** unconditionally under CI (`GITHUB_ACTIONS` set).
- **Vectors**: `validate/`, `argv/`, `interpret/`, `publish/`, `prompt/`, `spec/`, and `fixtures/`.
- **Comparison**: Deep equality with JS numeric equivalence (`1 == 1.0`), issue identity by `code` and named identity fields (`param`, `op`, `suggestion`).

## 2. In-Tree Property Tests (`tests/property.rs`)

Pure Rust, zero-dependency property and fuzz tests using a deterministic SplitMix64 PRNG (`Rng`). Requires no external tools or Node runtime.

1. **`encode -> parse` Round-Trip**:
   - For every operation in declared fixtures (`git2`, `gh2`, `aws2`), generates random valid inputs and asserts that encoding to JSON, GraphQL, and Argv forms and parsing back recovers the original parameters.
   - Respects known format constraints (GraphQL identifier names, Argv non-empty array requirement).
2. **`validate` Idempotence**:
   - Validating already-validated output against an `OperationSchema` produces identical output (`validate(schema, validate(schema, input)) == validate(schema, input)`).
3. **Tokenizer Panic Resilience**:
   - Fuzzes `tokenize_simple_command` with $100,000$ pseudo-random strings consisting of arbitrary ASCII, symbols, quotes, escapes, unicode scalar values, and control characters. Confirms the lexer never panics.
4. **`publish_schema` Round-Trip**:
   - Builds random published schema documents across tools and operations, serializes to JSON, and asserts `parse_published_schema(publish(schema))` succeeds and round-trips identically.
5. **Server `run_cli_io` Argv Fuzzing**:
   - Executes $10,000$ arbitrary token sequences against a `CliSpec` runner. Confirms zero panics and that every returned process exit code strictly belongs to the documented set `{0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 127}`.

## 3. Differential Tests (`tests/differential.rs`)

Automated differential test harness comparing `lmrust` against the reference TypeScript implementation `lmts` (`@lmctl-ai/lmts`).
- **Bridge**: `tests/support/lmts-oracle.mjs` executes `@lmctl-ai/lmts/dist` modules under Node 20+, communicating over stdin/stdout via JSON lines.
- **Coverage**: Over 2,500 deterministic pseudo-random cases:
  - `validate`: 600 cases across published fixtures, synthetic schema variations, type mismatches, required fields, occurrences (`1`, `?`, `+`, `*`), and enums.
  - `parse_argv_form`: 500 cases covering `--flag=value`, repeated flags, short aliases, boolean negation (`--no-flag`), and `--` positional boundaries.
  - `json_form` & `graphql_form`: 500 cases testing valid envelopes, bare objects, queries, mutations, comments, and syntax errors.
  - `encode` round-trips: 400 cases comparing `encode_argv`, `encode_json`, and `encode_graphql` outputs between Rust and Node.
  - `interpret_command`: 500 cases testing quotes, escapes, shell syntax rejections (pipes, redirects, env assignment, globs), and unicode commands.
- **Comparison Rule**: Canonical JSON comparison of success values; for validation issues, compares `code` and `param` identity.
- **CI & Local Execution**: Skips locally if `LMTS_DIR` / `node` is absent; fails in CI if `GITHUB_ACTIONS` is set. Pinned in CI to `LMTS_REF`.
- **Documented Divergences**:
  - `divergence_graphql_overflow_infinity`: In GraphQL, `1e309` overflows to `Infinity` in JS (`ok: true`), but fails with `DOCUMENT_PARSE` ("invalid number") in Rust because JSON values cannot represent `Infinity`.
  - `divergence_argv_infinity_number`: `--num Infinity` parses to `Infinity` in JS/lmts (later rejected by `validate`), while Rust preserves the string `"Infinity"` for `validate` to reject with the identical `TYPE_MISMATCH` issue code.
