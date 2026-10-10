# Interpreter — `interpret.rs`

Port of lmts `interpret.ts`; see lmapi `docs/protocol.md` section 8.

- `tokenize_simple_command(line)`: POSIX-style quoting, refusals reported as `SHELL_SYNTAX`
  (unquoted `; & | < > ( ) { } $` backtick, globs `* ? [`, newline/CR/NUL, word-initial `~` and
  `#`, `$`/backtick inside double quotes, unterminated quotes/escapes, leading `NAME=value`).
  Every refusal in the line is reported, not only the first.
- `interpret_command(schemas, line, extra_globals)`: tool (exact match, else `UNKNOWN_TOOL`
  with a suggestion within distance 2), op (`UNKNOWN_OPERATION`, `FLAG_BEFORE_OP`), then
  `parse_argv_form` with the tool's globals plus the caller's (a caller global replaces a
  same-named tool global in place), then `validate` in flags form. Argv issues and validation
  issues are both reported. Reserved names never appear in the result.
- `classify_operation`: `read` only for `access: read`; missing, write and dynamic are
  `effectful`. A schema can never make itself read-only.
- `describe_for_prompt`: tools and ops sorted with `js::locale_compare`, positionals first
  then flags sorted by kebab name, `(read|effectful)`, `— description`, then three rules.
  Matches the lmapi golden files byte for byte.
