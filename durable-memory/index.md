# lmrust — durable memory

Portable knowledge for the Rust SDK of lmapi. Read this page, then the page for the
module you are touching:

- [conformance.md](conformance.md): how the lmapi vectors run, what is compared, and the
  per-directory counts.
- [js-semantics.md](js-semantics.md): why `src/js.rs` exists (numbers, JSON text,
  collation) and the rules every port must follow.
- [argv-form.md](argv-form.md): the flags form, globals, positionals.
- [interpreter.md](interpreter.md): tokenizer, interpreter, classifier, prompt text.
- [server-runtime.md](server-runtime.md): `run_cli`, `CliSpec`, envelopes, exit codes, product check.
- [policy.md](policy.md): dry-run, read-only, confirmation, env floors.
- [modules.md](modules.md): the `@owner/repo` store (feature `modules`).

## What it is

`lmrust` is the Rust SDK for [lmapi](https://github.com/lmctl-ai/lmapi), in parity with
the TypeScript SDK [lmts](https://github.com/lmctl-ai/lmts). lmts is the reference
implementation: when behaviour is unclear, read the lmts source, not only its docs.
Modules mirror lmts files one to one:

| lmts | lmrust |
|---|---|
| `types.ts` | `types.rs` |
| `synopsis.ts`, `schema.ts` | `synopsis.rs` (+ `interpret::is_effectful`) |
| `validate.ts` | `validate.rs` |
| `argv-form.ts`, `json-form.ts`, `graphql-form.ts`, `inputs.ts` | same names, `_` for `-` |
| `encode.ts`, `help.ts`, `publish.ts`, `interpret.ts`, `version.ts` | same names |
| `server/*.ts` | `server/*.rs` |
| `modules/*.ts` | `modules/*.rs` (+ `http.rs`, `time.rs`) |

## Decisions

- **Values are `serde_json::Value`; records are `lmrust::Record`** (a `serde_json::Map`;
  with the default feature `preserve_order` it keeps insertion order like a JS object). lmts relies on it
  in `encode_argv` (flag order), `encode_json`, published documents and error envelopes.
- **Results**: every parser/validator returns `Outcome<T> = Result<T, Vec<ValidationIssue>>`
  (lmts `Result<T>`); the throwing lmts wrappers (`validateParams`, `parseJsonInput`, ...)
  return `Result<T, SchemaValidationError>` with the same `details` record.
- **`preserve_order` is a default feature, not a hard dependency setting** (0.1.1). Cargo unifies
  features across a binary, so forcing `serde_json/preserve_order` changed the key order of every
  `serde_json::Value` an embedding tool printed: lmprobe's legacy goldens (sorted keys) broke when
  it embedded lmrust 0.1.0. Hosts with sorted-key output use `default-features = false`; lmrust then
  behaves the same except that maps it builds iterate in sorted order. Tests that assert insertion
  order are gated on the feature (`encode_edge_cases`, `booleans_use_k_and_no_k`,
  `print_result_formats_json_to_stdout`, the `encode_argv` doc-test); the `meta.json` layout tests
  read key order from the file text, so they hold either way. CI runs default, `--all-features`,
  `--no-default-features` and `--no-default-features --features modules`.
- **Synchronous everywhere** (no async runtime): `run_cli` and handlers are sync; process
  execution uses `std::process` with a polling deadline and an `AtomicBool` cancel flag in
  place of an `AbortSignal`.
- **Minimal core dependencies**: serde, serde_json and regex-lite (for the product
  detection pattern: counting capture groups in `parse_published_schema` and matching in the
  product check). The version range matcher is in-house (`version.rs`) because ranges are
  1 to 4 plain comparators. The store (`modules` feature) adds ureq (rustls) and sha2.
- **Permissions/approval are deferred by decision**: only the inert lmts defaults exist
  (missing `access` = effectful, env floors, reserved names). Do not add approval machinery.
- Commits: identity `Mike Ma <mike.ma@lmctl.com>`, one `Co-Authored-By:` per contributing model.

## Verify

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                      # default features
cargo test --all-features       # + module store and the spec/ vectors
cargo +1.79 test --all-features --locked   # MSRV
cargo llvm-cov --all-features --summary-only
```

Conformance needs lmapi beside the repo (`../lmapi`) or `LMAPI_DIR`; see conformance.md.

## Known limitations

- **Strings are UTF-8, not UTF-16.** Edit distances, GraphQL `offset`s and the product
  check's `max_output_bytes` count Unicode scalar values where lmts counts UTF-16 code units
  (identical for ASCII). A lone surrogate from a GraphQL `\u` escape becomes U+FFFD.
- **No Infinity/NaN in JSON values.** `--n Infinity` parses in lmts' argv form as the number
  Infinity and is rejected by `validate` (TYPE_MISMATCH); lmrust keeps the text and
  `validate` rejects it the same way (same code, `received` is the string). A GraphQL number
  literal that overflows to Infinity is a `DOCUMENT_PARSE` error in lmrust.
- **JS object key order for integer-like keys** (`{"b":1,"2":0}` iterates `"2"` first in JS)
  is not reproduced; insertion order is kept.
- **`describe_for_prompt` collation** approximates ICU root order (`localeCompare`) for the
  ASCII identifier alphabet: punctuation < digits < letters, case-insensitive first. Exact for
  every published name lmapi allows; non-ASCII names sort by code point after ASCII.
- **Detection patterns use regex-lite syntax**, not JavaScript RegExp: lookaround and
  backreferences are rejected (`count_regexp_groups` returns `None`, the document is
  `DOCUMENT_PARSE`). Patterns lmapi tools publish (`(\d+\.\d+\.\d+)`) behave the same.
- **The typed published document is stricter**: lmts accepts any value in fields it does not
  check (an operation `description` that is a number, `aliases` that is not an array, ...).
  `parse_published_schema` reports such a document as one `TYPE_MISMATCH` issue instead of
  returning an untyped object.
- **Signals**: a child killed by a signal reports exit code `128 + signal`; lmts' Node
  adapter reported 0 there (fixed in lmts branch `fix-parity-findings`, not yet released).
- **Unreachable lmts branch kept**: `run_op`'s "invalid arguments" issue for a document plus
  an unparsable argv cannot trigger (a failed parse also drops the document globals), in lmts
  and here alike; ported for parity.
- **list() is sorted** (owner, repo, channel); lmts returns readdir order.
