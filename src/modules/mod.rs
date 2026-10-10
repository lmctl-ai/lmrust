//! The module store (lmts `./modules`, cargo feature `modules`): `@owner/repo[#latest|#dev]`
//! schema modules cached under `${LMCTL_HOME:-~/.lmctl}/modules/github.com/<owner>/<repo>/<channel>/`
//! as `cli.schema.<channel>.json` plus `meta.json` (directories 0700, files 0600). The
//! layout and `meta.json` are identical to lmts, so both SDKs share one cache.
//!
//! Security: HTTPS only, at most one redirect and only within `raw.githubusercontent.com`,
//! a size cap on every download, SHA-256 verification on every cache read, atomic writes,
//! and no path can escape the cache root.

pub mod http;
pub mod paths;
pub mod spec;
pub mod store;
pub mod time;
pub mod types;

pub use http::{FetchError, Fetcher, HttpRequest, HttpResponse, UreqFetcher};
pub use paths::{cache_root, cache_root_from, entry_dir, find_on_path, schema_file_name};
pub use spec::parse_spec;
pub use store::{
    ensure, install, list, remove, resolve_cached, upgrade, Clock, EnsureOptions, InstallOptions, ListOptions,
    RemoveOptions, ResolveOptions, SpecRef, UpgradeOptions, DEFAULT_TTL, ENSURE_MAX_BYTES, MAX_SCHEMA_BYTES,
};
pub use types::*;
