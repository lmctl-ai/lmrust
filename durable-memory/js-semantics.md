# JavaScript semantics (`src/js.rs`)

lmts is JavaScript, so some behaviour comes from the language rather than from lmts code.
`js.rs` reproduces exactly those pieces; use it instead of Rust's defaults whenever a value
or a text must match lmts.

| JavaScript | lmrust | Used by |
|---|---|---|
| `Number(string)` (trim, `0x`/`0o`/`0b`, `Infinity`, empty = 0) | `js::parse_number` | argv number coercion |
| `String(number)` / `JSON.stringify` numbers (`1` not `1.0`, `1e+21`, `1e-7`) | `js::number_to_string`, `js::number_text` | string coercion, messages |
| `JSON.stringify(v)` / `(v, null, 2)` | `js::stringify`, `js::stringify_pretty` (serde_json with a JS number formatter) | encode_json, `--schema`, envelopes, meta.json |
| `===` on primitives (enum checks, `includes`) | `js::strict_eq` | validate, publish defaults |
| deep equality of test expectations | `js::deep_eq` | conformance |
| `typeof` | `js::type_of` | messages |
| `String.prototype.trim` (Unicode whitespace) | `js::trim` | several |
| `localeCompare` | `js::locale_compare` | describe_for_prompt |

Rules:

- A number produced from text is stored with `js::number_value`, which keeps integral values
  as integers so `json!(2)` and a parsed `"2.0"` compare and print the same.
- Never format a `Value` with `to_string()` for user-visible text; use `js::stringify` or
  `js::to_display` (`String(value)`).
