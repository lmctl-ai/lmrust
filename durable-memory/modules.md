# Module store — `modules/` (feature `modules`)

Port of lmts `modules/*`; normative description in lmapi `docs/distribution.md`. Same layout
and `meta.json` as lmts, so lmrust and lmts share one cache (checked: an entry written by
either SDK resolves in the other).

## Layout

```text
${LMCTL_HOME:-$HOME/.lmctl}/modules/github.com/<owner>/<repo>/<channel>/
    cli.schema.<channel>.json   (0600)
    meta.json                   (0600; directories 0700)
```

`meta.json` keys in lmts order: `spec, url, sha256, fetched_at, last_checked, etag?, channel,
tool, version, bytes`, written as `JSON.stringify(meta, null, 2) + "\n"`.

## API

- `parse_spec("@owner/repo[#latest|#dev]")` -> `ModuleSpec{owner, repo, channel, key, requested}`
  (`SPEC_SYNTAX`, `SPEC_CHANNEL`).
- `cache_root()` / `cache_root_from(&env)`, `entry_dir(root, spec)` (`PATH_TRAVERSAL`),
  `schema_file_name(channel)`, `find_on_path(tool, path_var)`.
- `resolve_cached` (`NOT_INSTALLED`, `CACHE_CORRUPT` with `run: lmbee module install <key> --force`,
  `BAD_JSON`, `INVALID_SCHEMA`), `install` (installed / unchanged / replaced; `force`),
  `ensure` (TTL 6 h; stale -> one conditional GET with `If-None-Match`; 304 bumps
  `last_checked`; 200 replaces; any failure -> `StaleCache` + warning, files untouched;
  missing + install -> `Installed`; corrupt -> error, never refetched), `list` (problem rows,
  `binary_on_path`), `upgrade` (unchanged / updated with ops added/removed / failed), `remove`
  (`all_channels`, prunes empty parents).
- Network: the `Fetcher` trait performs one GET and must not follow redirects; the store
  follows at most one redirect and only to `https://raw.githubusercontent.com`. Default
  `UreqFetcher` (ureq + rustls, HTTPS only). Tests inject closures.
- Caps: `install` 2 MiB (`MAX_SCHEMA_BYTES`, lmapi distribution.md), `ensure` 1 MiB
  (`ENSURE_MAX_BYTES`, as lmts). Timeouts: install 10 s, ensure 3 s.
- Clocks are injectable (`&dyn Fn() -> SystemTime`); timestamps are `toISOString` format
  (`time.rs`, no date crate).

## Differences from lmts

- `list()` is sorted by owner/repo/channel (lmts: readdir order).
- `ModuleError.details` is boxed (`Option<Box<Record>>`) to keep `Result`s small.
- `upgrade(Some(specs))` keeps the lmts quirk: one unparsable spec returns a single failure
  for the whole call.
- Known limitation shared with lmts: an entry update is two renames (schema, then meta), not one
  atomic unit; a crash between them leaves a hash mismatch, reported as `CACHE_CORRUPT`.
