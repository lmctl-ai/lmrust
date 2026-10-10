# Conformance vectors

`tests/conformance.rs` runs every JSON vector in `lmapi/conformance/` against lmrust.

- **Location**: `$LMAPI_DIR/conformance`, default the sibling checkout `../lmapi`. Missing
  directory: skipped with a message locally, **fails** when `GITHUB_ACTIONS` is set (CI can
  never pass without running the vectors). CI checks lmapi out next to lmrust at a pinned
  commit (`LMAPI_REF` in `.github/workflows/ci.yml`); bump it deliberately.
- **Directories**: `validate/`, `argv/`, `interpret/`, `publish/`, `prompt/` (golden text,
  byte-exact), `spec/` (needs `--features modules`), and `fixtures/` (each shared fixture must
  itself pass `parse_published_schema`; an lmrust addition).
- **Comparison** (lmapi conformance README): values deep-equal with JavaScript number
  semantics (`1` equals `1.0`, `lmrust::js::deep_eq`); every expected issue must match an
  actual issue by `code` and by each identity field the vector names (`param`, `op`,
  `suggestion`), never by message text; matched issues must have a non-empty message,
  synopsis and usage. lmrust also checks `param`/`op` on interpret vectors, where lmts only
  checks `code` and `suggestion`.
- **Counts** (lmapi `c7768fb` plus branch `array-enum-default` `26bfb30`): validate 22/22, argv 14/14,
  interpret 40/40, publish 29/29, prompt 2/2, spec 18/18, fixtures 3/3.
  `cargo test --all-features --test conformance -- --nocapture` prints them per file.
- **Enum defaults**: an `enum` constrains each value of a default: the scalar, or every item of an
  array default (`occurs` `+`/`*` or `type: array`); a mismatch is `TYPE_MISMATCH`. lmts and lmrust
  0.1.0 compared the whole array with the enum and rejected valid documents.
- The harness was checked against deliberate mutations (suggestion distance, enum check):
  both made it fail.
