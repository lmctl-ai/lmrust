# Argv (flags) form — `argv_form.rs`

`parse_argv_form(argv, schema, globals)` is a token-by-token port of lmts `parseArgvForm`:

- Tokens are used as the shell split them; never joined or re-lexed.
- `--k v`, `--k=v` (split on the first `=`), `--k` (boolean / json true), `--no-k`
  (boolean / json false; a value on `--no-k=` is `TYPE_MISMATCH`), `-x` only when `x` is a
  declared name or alias, `--` ends flags.
- Lookup order: exact name, kebab name, aliases (op params win over globals), then camelCase
  of the token. Reserved names are never registered.
- Types come from the schema: `string` keeps text, `number` uses `Number()` semantics,
  `boolean` takes only `true`/`false`, `object`/`array` are JSON, `json` is a loose literal
  (`coerce_json_literal`).
- Multi (`+`, `*`, `array`) accumulate; repeated scalars are `DUPLICATE_FLAG`; a value flag
  followed by another `--flag` or the end is `MISSING_VALUE` (json params become `true`).
- Unknown flags: swallow a following value, report `UNKNOWN_PARAM` with `param` = camelCase
  name and a `--kebab` suggestion within edit distance 2; with `additionalParams` they are kept
  (loose literals; repeats become arrays).
- Positionals bind in order to positional params not already set (multi slots absorb the
  rest); surplus is `EXTRA_POSITIONAL` unless `additionalParams`.
- Every issue carries `argvIndex`.

`resolve_input` (`inputs.rs`) chooses exactly one form: a JSON document (`--input-json` /
`--input-file`), a GraphQL document, or the flags; globals may accompany a document, op flags
and positionals may not.
