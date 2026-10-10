//! Port of lmts `tests/ensure.test.ts`: evergreen refresh semantics of `ensure()` (TTL,
//! conditional GET with If-None-Match, 304 freshness bumps, atomic 200 replacements,
//! stale-cache fallback on every failure, concurrency and security invariants). Hermetic:
//! fake fetchers count calls to prove "no network"; clocks are injected.
#![cfg(feature = "modules")]

use lmrust::modules::time::{from_epoch_ms, parse_iso_ms};
use lmrust::modules::{
    ensure, entry_dir, install, parse_spec, resolve_cached, upgrade, Channel, EnsureOptions, EnsureSource, FetchError,
    Fetcher, HttpRequest, HttpResponse, InstallAction, InstallOptions, ModuleErrorCode, ModuleSpec, ResolveOptions,
    SpecRef, UpgradeOptions, UpgradeOutcome, DEFAULT_TTL, ENSURE_MAX_BYTES, MAX_SCHEMA_BYTES,
};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn v1() -> Value {
    json!({
        "schema": "cli.schema.v1",
        "tool": "mytool",
        "version": "1.0.0",
        "policy": {"dryrun": true, "readonly": true},
        "inputs": ["flags", "json"],
        "globals": [],
        "operations": [
            {"name": "status", "access": "read", "description": "Check status", "params": []}
        ]
    })
}

fn v2() -> Value {
    json!({
        "schema": "cli.schema.v1",
        "tool": "mytool",
        "version": "2.0.0",
        "policy": {"dryrun": true, "readonly": true},
        "inputs": ["flags", "json"],
        "globals": [],
        "operations": [
            {"name": "status", "access": "read", "description": "Check status", "params": []},
            {"name": "deploy", "access": "write", "description": "Deploy resource", "params": []}
        ]
    })
}

fn compact(doc: &Value) -> Vec<u8> {
    serde_json::to_vec(doc).unwrap()
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        404 => "Not Found",
        500 => "Internal Error",
        _ => "",
    }
}

fn response(status: u16, headers: &[(&str, &str)], body: impl Into<Vec<u8>>) -> Result<HttpResponse, FetchError> {
    Ok(HttpResponse {
        status,
        status_text: status_text(status).to_string(),
        headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        body: Box::new(std::io::Cursor::new(body.into())),
    })
}

fn ok_body(body: impl Into<Vec<u8>>) -> Result<HttpResponse, FetchError> {
    response(200, &[], body)
}

struct FailingBody(std::io::ErrorKind);

impl std::io::Read for FailingBody {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(self.0, "boom"))
    }
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> TempRoot {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lmrust-ensure-{}-{n}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        TempRoot(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn at(iso: &str) -> SystemTime {
    from_epoch_ms(parse_iso_ms(iso).unwrap())
}

fn spec_dir(root: &Path, spec: &str) -> PathBuf {
    entry_dir(root, &parse_spec(spec).unwrap()).unwrap()
}

fn ropts(root: &Path) -> ResolveOptions {
    ResolveOptions {
        root: Some(root.to_path_buf()),
    }
}

fn eopts<'a>(root: &'a Path, fetcher: &'a dyn Fetcher) -> EnsureOptions<'a> {
    EnsureOptions {
        root: Some(root),
        fetcher: Some(fetcher),
        ..EnsureOptions::default()
    }
}

/// `install('@owner/repo#latest', { now: () => iso })` serving `doc` with an optional ETag.
fn install_at(root: &Path, spec: &str, doc: &Value, etag: Option<&str>, iso: &str) {
    let body = compact(doc);
    let f = move |_: &HttpRequest| match etag {
        Some(e) => response(200, &[("ETag", e)], body.clone()),
        None => ok_body(body.clone()),
    };
    let t0 = at(iso);
    let clock = move || t0;
    install(
        spec,
        &InstallOptions {
            root: Some(root),
            fetcher: Some(&f),
            now: Some(&clock),
            ..Default::default()
        },
    )
    .unwrap();
}

fn if_none_match(r: &HttpRequest) -> Option<String> {
    r.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("if-none-match"))
        .map(|(_, v)| v.clone())
}

fn snapshot(dir: &Path) -> (Vec<u8>, Vec<u8>) {
    (
        fs::read(dir.join("cli.schema.latest.json")).unwrap(),
        fs::read(dir.join("meta.json")).unwrap(),
    )
}

const SPEC: &str = "@owner/repo#latest";
const T0: &str = "2026-10-10T10:00:00.000Z";
const T_STALE: &str = "2026-10-10T20:00:00.000Z";

// ---------------------------------------------------------------------------------------------
// Freshness
// ---------------------------------------------------------------------------------------------

#[test]
fn fresh_entry_makes_zero_fetch_calls_and_returns_cache() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), Some("\"etag-1\""), T0);

    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        response(200, &[("ETag", "\"etag-1\"")], compact(&v1()))
    };
    let t1 = at("2026-10-10T11:00:00.000Z");
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.source, EnsureSource::Cache);
    assert_eq!(out.source.as_str(), "cache");
    assert_eq!(out.warning, None);
    assert_eq!(out.module.version.as_deref(), Some("1.0.0"));
    assert_eq!(calls.get(), 0);
}

#[test]
fn entry_exactly_at_ttl_is_stale_and_revalidated() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), Some("\"e1\""), T0);
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        response(304, &[], "")
    };
    let t1 = at("2026-10-10T16:00:00.000Z");
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ttl: Some(Duration::from_secs(6 * 3600)),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(out.source, EnsureSource::Network);

    // One millisecond earlier it is still fresh (default TTL is 6 h).
    assert_eq!(DEFAULT_TTL, Duration::from_secs(6 * 3600));
    let t2 = TempRoot::new();
    install_at(t2.path(), SPEC, &v1(), Some("\"e1\""), T0);
    let t_fresh = at("2026-10-10T15:59:59.999Z");
    let clock = move || t_fresh;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t2.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.source, EnsureSource::Cache);
    assert_eq!(calls.get(), 1);
}

#[test]
fn custom_ttl_is_honoured() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, T0);
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        response(304, &[], "")
    };
    let t1 = at("2026-10-10T10:02:00.000Z");
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ttl: Some(Duration::from_secs(60)),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.source, EnsureSource::Network);
    assert_eq!(calls.get(), 1);
}

// ---------------------------------------------------------------------------------------------
// 304 / 200
// ---------------------------------------------------------------------------------------------

#[test]
fn stale_304_bumps_last_checked_and_etag_only() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), Some("\"etag-v1\""), T0);
    let dir = spec_dir(t.path(), SPEC);
    let schema_path = dir.join("cli.schema.latest.json");
    let schema_before = fs::read(&schema_path).unwrap();
    let meta_before: Value = serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();

    let requests = RefCell::new(Vec::<HttpRequest>::new());
    let f = |r: &HttpRequest| {
        requests.borrow_mut().push(r.clone());
        response(304, &[("ETag", "\"etag-v1-refreshed\"")], "")
    };
    let t1 = at("2026-10-10T17:00:00.000Z");
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();

    assert_eq!(out.source, EnsureSource::Network);
    assert_eq!(out.source.as_str(), "network");
    let reqs = requests.borrow();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].url,
        "https://raw.githubusercontent.com/owner/repo/HEAD/cli.schema.latest.json"
    );
    assert_eq!(if_none_match(&reqs[0]).as_deref(), Some("\"etag-v1\""));
    assert_eq!(reqs[0].timeout, Duration::from_millis(3000));
    assert_eq!(out.module.last_checked, "2026-10-10T17:00:00.000Z");
    assert_eq!(out.module.etag.as_deref(), Some("\"etag-v1-refreshed\""));
    assert_eq!(out.module.fetched_at, T0);

    assert_eq!(fs::read(&schema_path).unwrap(), schema_before);

    let meta_text = fs::read_to_string(dir.join("meta.json")).unwrap();
    assert!(meta_text.ends_with("}\n"));
    let meta: Value = serde_json::from_str(&meta_text).unwrap();
    assert_eq!(meta["last_checked"], "2026-10-10T17:00:00.000Z");
    assert_eq!(meta["etag"], "\"etag-v1-refreshed\"");
    // Everything else is carried over.
    for key in [
        "spec",
        "url",
        "sha256",
        "fetched_at",
        "channel",
        "tool",
        "version",
        "bytes",
    ] {
        assert_eq!(meta[key], meta_before[key], "{key}");
    }
    // Key order of the file itself (independent of how serde_json orders a parsed map).
    let mut keys: Vec<&str> = meta.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_by_key(|k| meta_text.find(&format!("\"{k}\":")).expect("key in meta.json"));
    assert_eq!(
        keys,
        vec![
            "spec",
            "url",
            "sha256",
            "fetched_at",
            "last_checked",
            "etag",
            "channel",
            "tool",
            "version",
            "bytes"
        ]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.join("meta.json")).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // The next call within the TTL is served from cache.
    let t2 = at("2026-10-10T18:00:00.000Z");
    let clock2 = move || t2;
    let again = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock2),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(again.source, EnsureSource::Cache);
    assert_eq!(requests.borrow().len(), 1);
}

#[test]
fn stale_304_without_etag_keeps_stored_etag() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), Some("\"keep\""), T0);
    let f = |_: &HttpRequest| response(304, &[], "");
    let t1 = at(T_STALE);
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.module.etag.as_deref(), Some("\"keep\""));
    let meta: Value =
        serde_json::from_str(&fs::read_to_string(spec_dir(t.path(), SPEC).join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["etag"], "\"keep\"");
}

#[test]
fn stale_200_replaces_entry_atomically_with_new_document() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), Some("\"etag-v1\""), T0);
    let f = |_: &HttpRequest| response(200, &[("ETag", "\"etag-v2\"")], compact(&v2()));
    let t1 = at("2026-10-10T18:00:00.000Z");
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();

    assert_eq!(out.source, EnsureSource::Network);
    assert_eq!(out.module.version.as_deref(), Some("2.0.0"));
    assert_eq!(out.module.etag.as_deref(), Some("\"etag-v2\""));
    assert_eq!(out.module.last_checked, "2026-10-10T18:00:00.000Z");
    assert_eq!(out.module.fetched_at, "2026-10-10T18:00:00.000Z");
    assert!(out.module.document.operations.iter().any(|o| o.name == "deploy"));

    let cached = resolve_cached(SPEC, &ropts(t.path())).unwrap();
    assert_eq!(cached.version.as_deref(), Some("2.0.0"));
    assert_eq!(cached.etag.as_deref(), Some("\"etag-v2\""));
    assert_eq!(cached.sha256, out.module.sha256);
    let dir = spec_dir(t.path(), SPEC);
    assert_eq!(fs::read(dir.join("cli.schema.latest.json")).unwrap(), compact(&v2()));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2, "no temp files left behind");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["cli.schema.latest.json", "meta.json"] {
            assert_eq!(
                fs::metadata(dir.join(name)).unwrap().permissions().mode() & 0o777,
                0o600,
                "{name}"
            );
        }
    }
}

#[test]
fn missing_etag_sends_plain_get() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, T0);
    let sent = RefCell::new(None::<HttpRequest>);
    let f = |r: &HttpRequest| {
        *sent.borrow_mut() = Some(r.clone());
        ok_body(compact(&v2()))
    };
    let t1 = at(T_STALE);
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.source, EnsureSource::Network);
    let req = sent.borrow().clone().expect("one request");
    assert_eq!(if_none_match(&req), None);
    assert!(req.headers.is_empty());
    // The new entry has no etag: meta.json omits the key.
    let meta: Value =
        serde_json::from_str(&fs::read_to_string(spec_dir(t.path(), SPEC).join("meta.json")).unwrap()).unwrap();
    assert!(meta.get("etag").is_none());
}

#[test]
fn if_none_match_is_sent_exactly_when_etag_stored() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), Some("W/\"exact-etag-123\""), T0);
    let sent = RefCell::new(Vec::<HttpRequest>::new());
    let f = |r: &HttpRequest| {
        sent.borrow_mut().push(r.clone());
        response(304, &[], "")
    };
    let t1 = at(T_STALE);
    let clock = move || t1;
    ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    let sent = sent.borrow();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].headers,
        vec![("If-None-Match".to_string(), "W/\"exact-etag-123\"".to_string())]
    );
}

#[test]
fn future_last_checked_is_stale() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, T0);
    let dir = spec_dir(t.path(), SPEC);
    let mut meta: Value = serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    meta["last_checked"] = json!("2026-10-10T23:59:59.000Z");
    fs::write(dir.join("meta.json"), meta.to_string()).unwrap();

    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        response(304, &[], "")
    };
    let now = at("2026-10-10T11:00:00.000Z");
    let clock = move || now;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(out.source, EnsureSource::Network);
    assert_eq!(out.module.last_checked, "2026-10-10T11:00:00.000Z");
}

#[test]
fn missing_last_checked_is_stale() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, T0);
    let dir = spec_dir(t.path(), SPEC);
    let mut meta: Value = serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    meta.as_object_mut().unwrap().remove("last_checked");
    meta.as_object_mut().unwrap().remove("fetched_at");
    fs::write(dir.join("meta.json"), meta.to_string()).unwrap();

    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        response(304, &[], "")
    };
    let out = ensure(SPEC, &eopts(t.path(), &f)).unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(out.source, EnsureSource::Network);
}

#[test]
fn missing_fetched_at_stays_missing_after_304() {
    // lmts: metaDoc.fetched_at = currentModule.fetchedAt (undefined) -> JSON.stringify drops the key.
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, T0);
    let dir = spec_dir(t.path(), SPEC);
    let mut meta: Value = serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    meta.as_object_mut().unwrap().remove("last_checked");
    meta.as_object_mut().unwrap().remove("fetched_at");
    fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
    let f = |_: &HttpRequest| response(304, &[], "");
    ensure(SPEC, &eopts(t.path(), &f)).unwrap();
    let meta: Value = serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    assert!(meta.get("fetched_at").is_none(), "{meta}");
}

#[test]
fn unparseable_last_checked_is_stale() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, T0);
    let dir = spec_dir(t.path(), SPEC);
    let mut meta: Value = serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    meta["last_checked"] = json!("not a date");
    fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        response(304, &[], "")
    };
    let t1 = at("2026-10-10T10:00:01.000Z");
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(out.source, EnsureSource::Network);
}

#[test]
fn update_false_returns_cache_regardless_of_age() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, "2020-01-01T00:00:00.000Z");
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        response(304, &[], "")
    };
    let t1 = at("2026-10-10T12:00:00.000Z");
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            update: Some(false),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.source, EnsureSource::Cache);
    assert_eq!(calls.get(), 0);
}

// ---------------------------------------------------------------------------------------------
// stale-cache fallback on every failure
// ---------------------------------------------------------------------------------------------

/// Install v1 at T0, ensure at T_STALE with `fetch`, and assert stale-cache with a warning
/// containing `needle` and byte-identical files. Returns the warning.
fn assert_stale_fallback(
    fetch: &dyn Fetcher,
    max_bytes: Option<usize>,
    timeout_ms: Option<u64>,
    needle: &str,
) -> String {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), Some("\"etag-v1\""), T0);
    let dir = spec_dir(t.path(), SPEC);
    let before = snapshot(&dir);

    let t1 = at(T_STALE);
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            max_bytes,
            timeout: timeout_ms.map(Duration::from_millis),
            ..eopts(t.path(), fetch)
        },
    )
    .unwrap();
    assert_eq!(out.source, EnsureSource::StaleCache);
    assert_eq!(out.source.as_str(), "stale-cache");
    let warning = out.warning.clone().expect("warning");
    assert!(warning.contains(needle), "expected {needle:?} in {warning:?}");
    assert_eq!(out.module.version.as_deref(), Some("1.0.0"));
    assert_eq!(out.module.last_checked, T0);
    assert_eq!(snapshot(&dir), before, "files must be byte-identical");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2, "no temp files left behind");
    warning
}

#[test]
fn stale_network_error_falls_back_to_stale_cache() {
    let f = |_: &HttpRequest| Err(FetchError::Network("ENOTFOUND raw.githubusercontent.com".into()));
    assert_stale_fallback(&f, None, None, "ENOTFOUND");
}

#[test]
fn stale_timeout_falls_back_to_stale_cache() {
    let f = |_: &HttpRequest| Err(FetchError::Timeout("The operation was aborted".into()));
    let w = assert_stale_fallback(&f, None, Some(50), "timed out");
    assert!(w.contains("50ms"), "{w}");
}

#[test]
fn stale_body_read_timeout_and_error_fall_back_to_stale_cache() {
    let f = |_: &HttpRequest| {
        Ok(HttpResponse {
            status: 200,
            status_text: "OK".into(),
            headers: vec![],
            body: Box::new(FailingBody(std::io::ErrorKind::TimedOut)),
        })
    };
    assert_stale_fallback(&f, None, None, "Fetch stream timed out after 3000ms");
    let g = |_: &HttpRequest| {
        Ok(HttpResponse {
            status: 200,
            status_text: "OK".into(),
            headers: vec![],
            body: Box::new(FailingBody(std::io::ErrorKind::ConnectionReset)),
        })
    };
    assert_stale_fallback(&g, None, None, "Error reading response stream");
}

#[test]
fn stale_404_falls_back_to_stale_cache() {
    let f = |_: &HttpRequest| response(404, &[], "Not Found");
    let w = assert_stale_fallback(&f, None, None, "404");
    assert_eq!(w, "Failed to refresh @owner/repo#latest: HTTP 404 Not Found");
}

#[test]
fn stale_500_falls_back_to_stale_cache() {
    let f = |_: &HttpRequest| response(500, &[], "Server Error");
    assert_stale_fallback(&f, None, None, "500");
}

#[test]
fn stale_non_200_success_falls_back_to_stale_cache() {
    let f = |_: &HttpRequest| response(201, &[], compact(&v2()));
    assert_stale_fallback(&f, None, None, "HTTP 201");
}

#[test]
fn stale_oversized_payload_falls_back_to_stale_cache() {
    let f = |_: &HttpRequest| ok_body("x".repeat(2000));
    assert_stale_fallback(&f, Some(500), None, "exceeds maximum size");
}

#[test]
fn stale_bad_json_falls_back_to_stale_cache() {
    let f = |_: &HttpRequest| ok_body("<html>Error</html>");
    assert_stale_fallback(&f, None, None, "JSON");
}

#[test]
fn stale_invalid_schema_falls_back_to_stale_cache() {
    let mut doc = v1();
    doc["tool"] = json!("INVALID_TOOL_UPPER");
    let body = compact(&doc);
    let f = |_: &HttpRequest| ok_body(body.clone());
    assert_stale_fallback(&f, None, None, "Invalid schema for @owner/repo#latest");
}

#[test]
fn cross_host_redirect_on_refresh_falls_back_to_stale_cache() {
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        response(302, &[("location", "https://evil.com/schema.json")], "")
    };
    assert_stale_fallback(&f, None, None, "refused");
    assert_eq!(calls.get(), 1);
}

#[test]
fn non_https_redirect_on_refresh_falls_back_to_stale_cache() {
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        response(
            302,
            &[(
                "location",
                "http://raw.githubusercontent.com/owner/repo/HEAD/cli.schema.latest.json",
            )],
            "",
        )
    };
    assert_stale_fallback(&f, None, None, "refused");
    assert_eq!(calls.get(), 1);
}

#[test]
fn redirect_without_location_on_refresh_falls_back_to_stale_cache() {
    let f = |_: &HttpRequest| response(301, &[], "");
    assert_stale_fallback(&f, None, None, "Location header");
}

#[test]
fn second_redirect_on_refresh_falls_back_to_stale_cache() {
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        response(
            302,
            &[(
                "location",
                "https://raw.githubusercontent.com/owner/repo/main/cli.schema.latest.json",
            )],
            "",
        )
    };
    assert_stale_fallback(&f, None, None, "HTTP 302");
    assert_eq!(calls.get(), 2);
}

#[test]
fn same_host_https_redirect_on_refresh_is_followed_and_updates_cache() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), Some("\"etag-v1\""), T0);
    let requests = RefCell::new(Vec::<HttpRequest>::new());
    let f = |r: &HttpRequest| {
        requests.borrow_mut().push(r.clone());
        if r.url.contains("HEAD") {
            response(
                302,
                &[(
                    "location",
                    "https://raw.githubusercontent.com/owner/repo/main/cli.schema.latest.json",
                )],
                "",
            )
        } else {
            ok_body(compact(&v2()))
        }
    };
    let t1 = at(T_STALE);
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.source, EnsureSource::Network);
    assert_eq!(out.module.version.as_deref(), Some("2.0.0"));
    let reqs = requests.borrow();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[1].url,
        "https://raw.githubusercontent.com/owner/repo/main/cli.schema.latest.json"
    );
    // The conditional header travels with the redirected request, within the one deadline.
    assert_eq!(if_none_match(&reqs[1]).as_deref(), Some("\"etag-v1\""));
    assert!(reqs[1].timeout <= reqs[0].timeout);
    assert_eq!(
        resolve_cached(SPEC, &ropts(t.path())).unwrap().version.as_deref(),
        Some("2.0.0")
    );
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// `true` when a read-only directory still accepts writes (e.g. running as root).
#[cfg(unix)]
fn read_only_dirs_are_writable(dir: &Path) -> bool {
    let probe = dir.join(".probe");
    let writable = fs::write(&probe, b"x").is_ok();
    let _ = fs::remove_file(&probe);
    writable
}

#[cfg(unix)]
#[test]
fn atomic_write_failure_on_200_keeps_old_entry() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, T0);
    let dir = spec_dir(t.path(), SPEC);
    let before = snapshot(&dir);
    chmod(&dir, 0o500);
    if read_only_dirs_are_writable(&dir) {
        chmod(&dir, 0o700);
        eprintln!("skipping: read-only directories are writable here (root?)");
        return;
    }
    let f = |_: &HttpRequest| ok_body(compact(&v2()));
    let t1 = at(T_STALE);
    let clock = move || t1;
    let res = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    );
    chmod(&dir, 0o700);
    let out = res.unwrap();
    assert_eq!(out.source, EnsureSource::StaleCache);
    assert_eq!(out.module.version.as_deref(), Some("1.0.0"));
    assert!(
        out.warning
            .as_deref()
            .unwrap()
            .contains("Failed to write cached module files"),
        "{:?}",
        out.warning
    );
    assert_eq!(snapshot(&dir), before);
}

#[cfg(unix)]
#[test]
fn meta_write_failure_on_304_keeps_old_entry() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), Some("\"e1\""), T0);
    let dir = spec_dir(t.path(), SPEC);
    let before = snapshot(&dir);
    chmod(&dir, 0o500);
    if read_only_dirs_are_writable(&dir) {
        chmod(&dir, 0o700);
        eprintln!("skipping: read-only directories are writable here (root?)");
        return;
    }
    let f = |_: &HttpRequest| response(304, &[("ETag", "\"e2\"")], "");
    let t1 = at(T_STALE);
    let clock = move || t1;
    let res = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    );
    chmod(&dir, 0o700);
    let out = res.unwrap();
    assert_eq!(out.source, EnsureSource::StaleCache);
    assert!(
        out.warning.as_deref().unwrap().contains("Failed to write meta.json"),
        "{:?}",
        out.warning
    );
    assert_eq!(out.module.last_checked, T0);
    assert_eq!(out.module.etag.as_deref(), Some("\"e1\""));
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn concurrent_ensure_calls_on_stale_entry_end_valid() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, T0);
    let root = t.path();
    let barrier = std::sync::Barrier::new(4);
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let barrier = &barrier;
                s.spawn(move || {
                    // Every caller has resolved the stale entry before any of them writes.
                    let f = |_: &HttpRequest| {
                        barrier.wait();
                        ok_body(compact(&v2()))
                    };
                    let t1 = at(T_STALE);
                    let clock = move || t1;
                    ensure(
                        SPEC,
                        &EnsureOptions {
                            now: Some(&clock),
                            ..eopts(root, &f)
                        },
                    )
                    .map(|o| o.source)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for r in results {
        assert_eq!(r.unwrap(), EnsureSource::Network);
    }
    let verified = resolve_cached(SPEC, &ropts(t.path())).unwrap();
    assert_eq!(verified.version.as_deref(), Some("2.0.0"));
    assert_eq!(fs::read_dir(spec_dir(t.path(), SPEC)).unwrap().count(), 2);
}

// ---------------------------------------------------------------------------------------------
// Missing / corrupt entries
// ---------------------------------------------------------------------------------------------

#[test]
fn not_installed_with_install_true_installs() {
    let t = TempRoot::new();
    let f = |_: &HttpRequest| response(200, &[("ETag", "\"e1\"")], compact(&v1()));
    let out = ensure(
        SPEC,
        &EnsureOptions {
            install: Some(true),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.source, EnsureSource::Installed);
    assert_eq!(out.source.as_str(), "installed");
    assert_eq!(out.module.tool, "mytool");
    assert_eq!(out.module.etag.as_deref(), Some("\"e1\""));
    assert_eq!(out.warning, None);
    assert!(resolve_cached(SPEC, &ropts(t.path())).is_ok());
}

#[test]
fn not_installed_with_install_false_returns_hint() {
    let t = TempRoot::new();
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    let e = ensure(
        SPEC,
        &EnsureOptions {
            install: Some(false),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::NotInstalled);
    assert!(
        e.message.ends_with("run: lmbee module install @owner/repo#latest"),
        "{}",
        e.message
    );
    assert_eq!(calls.get(), 0);
}

#[test]
fn not_installed_install_failure_propagates_error() {
    let t = TempRoot::new();
    let f = |_: &HttpRequest| response(404, &[], "Not Found");
    let e = ensure(SPEC, &eopts(t.path(), &f)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("root of the default branch"), "{}", e.message);
    assert_eq!(e.details.expect("details")["status"], 404);
}

#[test]
fn ensure_install_uses_1_mib_cap_while_install_uses_2_mib() {
    assert_eq!(ENSURE_MAX_BYTES, 1024 * 1024);
    assert_eq!(MAX_SCHEMA_BYTES, 2 * 1024 * 1024);
    let mut big = compact(&v1());
    big.resize(1536 * 1024, b' '); // valid JSON padded to 1.5 MiB
    let f = |_: &HttpRequest| ok_body(big.clone());

    // ensure (missing -> install) applies its own 1 MiB default.
    let t = TempRoot::new();
    let e = ensure(SPEC, &eopts(t.path(), &f)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::OutputLimit);
    assert!(e.message.contains("1048576 bytes"), "{}", e.message);
    assert_eq!(
        resolve_cached(SPEC, &ropts(t.path())).unwrap_err().code,
        ModuleErrorCode::NotInstalled
    );

    // install accepts the same body under its 2 MiB default.
    let out = install(
        SPEC,
        &InstallOptions {
            root: Some(t.path()),
            fetcher: Some(&f),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out.action, InstallAction::Installed);

    // A stale refresh with the same body falls back to the cache.
    let t2 = TempRoot::new();
    install_at(t2.path(), SPEC, &v1(), None, T0);
    let t1 = at(T_STALE);
    let clock = move || t1;
    let out = ensure(
        SPEC,
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t2.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.source, EnsureSource::StaleCache);
    assert!(out
        .warning
        .as_deref()
        .unwrap()
        .contains("exceeds maximum size limit of 1048576 bytes"));
}

#[test]
fn corrupt_entry_returns_cache_corrupt_and_never_refetches() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, T0);
    let dir = spec_dir(t.path(), SPEC);
    fs::write(dir.join("cli.schema.latest.json"), "tampered").unwrap();
    let meta_before = fs::read(dir.join("meta.json")).unwrap();

    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    let e = ensure(SPEC, &eopts(t.path(), &f)).unwrap_err();
    assert_eq!(calls.get(), 0);
    assert_eq!(e.code, ModuleErrorCode::CacheCorrupt);
    assert!(
        e.message
            .contains("run: lmbee module install @owner/repo#latest --force"),
        "{}",
        e.message
    );
    assert_eq!(fs::read(dir.join("cli.schema.latest.json")).unwrap(), b"tampered");
    assert_eq!(fs::read(dir.join("meta.json")).unwrap(), meta_before);

    // Corrupt meta.json is treated the same way.
    fs::write(dir.join("meta.json"), "{nope").unwrap();
    let e = ensure(
        SPEC,
        &EnsureOptions {
            install: Some(true),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::CacheCorrupt);
    assert_eq!(calls.get(), 0);
}

#[test]
fn hash_correct_but_invalid_cached_document_is_returned_without_refetch() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), None, T0);
    let dir = spec_dir(t.path(), SPEC);
    let bad = b"not json".to_vec();
    let mut meta: Value = serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    meta["sha256"] = json!(sha256_hex(&bad));
    fs::write(dir.join("cli.schema.latest.json"), &bad).unwrap();
    fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    assert_eq!(
        ensure(SPEC, &eopts(t.path(), &f)).unwrap_err().code,
        ModuleErrorCode::BadJson
    );
    assert_eq!(calls.get(), 0);
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------------------------
// upgrade / install interplay
// ---------------------------------------------------------------------------------------------

#[test]
fn upgrade_ignores_ttl_and_records_etag() {
    let t = TempRoot::new();
    install_at(t.path(), SPEC, &v1(), Some("\"etag-1\""), T0);
    let t1 = at("2026-10-10T10:05:00.000Z");
    let clock = move || t1;
    let calls = Cell::new(0);
    let f = |r: &HttpRequest| {
        calls.set(calls.get() + 1);
        assert_eq!(if_none_match(r), None, "upgrade is an unconditional GET");
        response(200, &[("ETag", "\"etag-2\"")], compact(&v2()))
    };
    let specs = [SpecRef::from(SPEC)];
    let out = upgrade(
        Some(&specs),
        &UpgradeOptions {
            root: Some(t.path()),
            fetcher: Some(&f),
            now: Some(&clock),
        },
    );
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], UpgradeOutcome::Updated { .. }), "{out:?}");
    assert_eq!(calls.get(), 1);
    let cached = resolve_cached(SPEC, &ropts(t.path())).unwrap();
    assert_eq!(cached.etag.as_deref(), Some("\"etag-2\""));
    assert_eq!(cached.last_checked, "2026-10-10T10:05:00.000Z");
}

#[test]
fn install_records_etag_and_last_checked() {
    let t = TempRoot::new();
    let f = |_: &HttpRequest| response(200, &[("ETag", "\"my-etag\"")], compact(&v1()));
    let t0 = at(T0);
    let clock = move || t0;
    let out = install(
        "@owner/repo#dev",
        &InstallOptions {
            root: Some(t.path()),
            fetcher: Some(&f),
            now: Some(&clock),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out.entry.etag.as_deref(), Some("\"my-etag\""));
    assert_eq!(out.entry.last_checked, T0);
    let resolved = resolve_cached("@owner/repo#dev", &ropts(t.path())).unwrap();
    assert_eq!(resolved.etag.as_deref(), Some("\"my-etag\""));
    assert_eq!(resolved.last_checked, T0);
}

// ---------------------------------------------------------------------------------------------
// Specs
// ---------------------------------------------------------------------------------------------

#[test]
fn ensure_works_on_dev_channel() {
    let t = TempRoot::new();
    let urls = RefCell::new(Vec::<String>::new());
    let f = |r: &HttpRequest| {
        urls.borrow_mut().push(r.url.clone());
        ok_body(compact(&v1()))
    };
    let t0 = at(T0);
    let clock = move || t0;
    let out = ensure(
        "@owner/repo#dev",
        &EnsureOptions {
            now: Some(&clock),
            ..eopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.source, EnsureSource::Installed);
    assert_eq!(out.module.channel, Channel::Dev);
    assert_eq!(
        *urls.borrow(),
        vec!["https://raw.githubusercontent.com/owner/repo/HEAD/cli.schema.dev.json"]
    );
    assert!(t.path().join("github.com/owner/repo/dev/cli.schema.dev.json").exists());
}

#[test]
fn ensure_refuses_path_traversal_spec() {
    let t = TempRoot::new();
    let spec = ModuleSpec {
        owner: "..".into(),
        repo: "escaped".into(),
        channel: Channel::Latest,
        key: "@../escaped#latest".into(),
        requested: "@../escaped".into(),
    };
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    let e = ensure(&spec, &eopts(t.path(), &f)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::PathTraversal);
    assert_eq!(calls.get(), 0);
}

#[test]
fn ensure_refuses_invalid_syntax() {
    let t = TempRoot::new();
    let f = |_: &HttpRequest| ok_body(compact(&v1()));
    assert_eq!(
        ensure("bad-spec", &eopts(t.path(), &f)).unwrap_err().code,
        ModuleErrorCode::SpecSyntax
    );
}

#[test]
fn ensure_refuses_invalid_channel() {
    let t = TempRoot::new();
    let f = |_: &HttpRequest| ok_body(compact(&v1()));
    let e = ensure("@owner/repo#v1.0.0", &eopts(t.path(), &f)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::SpecChannel);
    assert!(e.message.contains("only the latest and dev channels are supported"));
}

#[test]
fn ensure_accepts_parsed_module_spec() {
    let t = TempRoot::new();
    let spec = parse_spec("@owner/repo").unwrap();
    let f = |_: &HttpRequest| ok_body(compact(&v1()));
    let out = ensure(&spec, &eopts(t.path(), &f)).unwrap();
    assert_eq!(out.source, EnsureSource::Installed);
    assert_eq!(out.module.channel, Channel::Latest);
    assert_eq!(out.module.spec, spec);
}
