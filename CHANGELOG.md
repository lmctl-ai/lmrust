# Changelog

## 0.1.3

### Added
- Cargo feature `clap` (off by default): `lmrust::clap_bridge`, generalised from lmprobe's lmapi
  bridge. `Bridge::new(clap::Command, overlay_toml)` publishes a deterministic `cli.schema.v1`
  document generated from the clap tree plus an overlay (op names, required `access`,
  `dangerous`, parameter exclusions, `drop_globals`, `inputs`), and serves each operation by
  self-executing `std::env::current_exe()` (`Bridge::exe` overrides it) with the legacy argv rebuilt
  in schema order. Overlay keys for the tool's JSON switch (`legacy_json_args`), root args, stdin
  params (`[op.params.x] stdin = true`), exit-code mapping (`success_exit_codes`, `exit_codes`),
  output caps, a deadline and the text-result key. A signal-killed child is `INTERNAL_ERROR`
  (`SIGNALED`), never a success. `assert_coverage` / `coverage_problems` check that every clap leaf is
  published or excluded and that no published name equals a legacy subcommand name or alias.
- Dependencies of the feature: clap 4.5 (`std` only; 4.5.x keeps the 1.79 MSRV, clap 4.6 needs
  1.85) and toml 0.8. Default features and their dependencies are unchanged.

## 0.1.2

### Fixed
- `encode_argv` with `positional: true` emits positional params after `--` in schema order (the
  order the runner assigns positionals in) instead of the params map's key order. Under sorted keys
  (`default-features = false`) or any map whose keys were not in schema order, positional values
  were swapped silently (`{"symbol", "path"}` became `-- <path> <symbol>`). lmapi conformance
  `encode/` vectors; lmts has the same fix.

## 0.1.1

### Changed
- `preserve_order` is now a default cargo feature (`preserve_order = ["serde_json/preserve_order"]`)
  instead of a fixed `serde_json` feature. Embedding binaries whose own JSON output relies on
  sorted keys depend on lmrust with `default-features = false` (Cargo feature unification otherwise
  switches their `serde_json::Value` maps to insertion order). Defaults are unchanged.

### Fixed
- `parse_published_schema` checks an array param's `enum` against each item of its `default`
  (occurs `+`/`*` or `type: array`) instead of the whole array, so documents such as
  `find --display` with `default: ["name", "lastmod", "size"]` are accepted; an item outside the enum
  is `TYPE_MISMATCH`, as for a scalar default (lmapi conformance `publish/` vectors).

## 0.1.0

- First release: cli.schema.v1 core (one validator, flags/JSON/GraphQL forms, publish check,
  interpreter, encoders, help), server runtime (`run_cli`, policy, envelopes, exit codes, product
  check, native-tool guard and configuration) and the `modules` store, in parity with lmts.
