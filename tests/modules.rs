//! Port of lmts `tests/modules.test.ts`: spec parsing, cache_root, entry_dir, schema_file_name,
//! resolve_cached, install, list, upgrade, remove, find_on_path, atomic writes, tamper
//! detection and error codes. Hermetic: every test has its own temp root and a fake fetcher
//! (no network); clocks are injected.
#![cfg(feature = "modules")]

use lmrust::modules::time::{from_epoch_ms, parse_iso_ms, to_iso_string};
use lmrust::modules::{
    cache_root_from, entry_dir, find_on_path, install, list, parse_spec, remove, resolve_cached, schema_file_name,
    upgrade, Channel, FetchError, Fetcher, HttpRequest, HttpResponse, InstallAction, InstallOptions, ListOptions,
    ModuleErrorCode, ModuleSpec, RemoveOptions, ResolveOptions, SpecRef, UpgradeOptions, UpgradeOutcome, UreqFetcher,
    MAX_SCHEMA_BYTES,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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
            {"name": "query", "access": "read", "description": "Query items",
             "params": [{"name": "filter", "type": "string", "occurs": "?"}]},
            {"name": "apply", "access": "write", "description": "Apply changes",
             "params": [{"name": "name", "type": "string"}]}
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
            {"name": "query", "access": "read", "description": "Query items",
             "params": [{"name": "filter", "type": "string", "occurs": "?"}]},
            {"name": "batch", "access": "write", "description": "Batch changes",
             "params": [{"name": "items", "type": "array"}]}
        ]
    })
}

/// `JSON.stringify(doc)`
fn compact(doc: &Value) -> Vec<u8> {
    serde_json::to_vec(doc).unwrap()
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        404 => "Not Found",
        500 => "Internal Server Error",
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

/// A body whose read fails.
struct FailingBody(std::io::ErrorKind);

impl std::io::Read for FailingBody {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(self.0, "boom"))
    }
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A unique temp directory removed on drop.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> TempRoot {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lmrust-modules-{tag}-{}-{n}-{nanos}", std::process::id()));
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

fn must_parse(spec: &str) -> ModuleSpec {
    parse_spec(spec).unwrap_or_else(|e| panic!("Failed to parse spec {spec}: {}", e.message))
}

fn hostile(owner: &str, repo: &str) -> ModuleSpec {
    ModuleSpec {
        owner: owner.into(),
        repo: repo.into(),
        channel: Channel::Latest,
        key: format!("@{owner}/{repo}#latest"),
        requested: format!("@{owner}/{repo}"),
    }
}

fn at(iso: &str) -> SystemTime {
    from_epoch_ms(parse_iso_ms(iso).unwrap())
}

fn iopts<'a>(root: &'a Path, fetcher: &'a dyn Fetcher) -> InstallOptions<'a> {
    InstallOptions {
        root: Some(root),
        fetcher: Some(fetcher),
        ..InstallOptions::default()
    }
}

fn ropts(root: &Path) -> ResolveOptions {
    ResolveOptions {
        root: Some(root.to_path_buf()),
    }
}

fn install_doc(root: &Path, spec: &str, doc: &Value) {
    let body = compact(doc);
    let f = move |_: &HttpRequest| ok_body(body.clone());
    install(spec, &iopts(root, &f)).unwrap();
}

/// A PATH containing only an empty directory (so `mytool` is never found).
fn empty_path_dir(root: &TempRoot) -> String {
    let bin = root.path().join("empty-bin");
    fs::create_dir_all(&bin).unwrap();
    bin.to_string_lossy().into_owned()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
fn make_executable(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn outcome_spec(o: &UpgradeOutcome) -> &str {
    match o {
        UpgradeOutcome::Unchanged { spec, .. }
        | UpgradeOutcome::Updated { spec, .. }
        | UpgradeOutcome::Failed { spec, .. } => spec,
    }
}

fn action(o: &UpgradeOutcome) -> &'static str {
    match o {
        UpgradeOutcome::Unchanged { .. } => "unchanged",
        UpgradeOutcome::Updated { .. } => "updated",
        UpgradeOutcome::Failed { .. } => "failed",
    }
}

// ---------------------------------------------------------------------------------------------
// parse_spec
// ---------------------------------------------------------------------------------------------

#[test]
fn parse_spec_basic_defaults_to_latest_with_normalized_key() {
    let s = parse_spec("@lmctl-ai/lmgit").unwrap();
    assert_eq!(s.owner, "lmctl-ai");
    assert_eq!(s.repo, "lmgit");
    assert_eq!(s.channel, Channel::Latest);
    assert_eq!(s.key, "@lmctl-ai/lmgit#latest");
    assert_eq!(s.requested, "@lmctl-ai/lmgit");
}

#[test]
fn parse_spec_explicit_latest() {
    let s = parse_spec("@lmctl-ai/lmgit#latest").unwrap();
    assert_eq!(s.owner, "lmctl-ai");
    assert_eq!(s.repo, "lmgit");
    assert_eq!(s.channel, Channel::Latest);
    assert_eq!(s.key, "@lmctl-ai/lmgit#latest");
    assert_eq!(s.requested, "@lmctl-ai/lmgit#latest");
}

#[test]
fn parse_spec_explicit_dev() {
    let s = parse_spec("@lmctl-ai/lmgit#dev").unwrap();
    assert_eq!(s.owner, "lmctl-ai");
    assert_eq!(s.repo, "lmgit");
    assert_eq!(s.channel, Channel::Dev);
    assert_eq!(s.key, "@lmctl-ai/lmgit#dev");
    assert_eq!(s.requested, "@lmctl-ai/lmgit#dev");
}

#[test]
fn parse_spec_rejects_other_channels_with_spec_channel() {
    for spec in [
        "@lmctl-ai/lmgit#v1.0.0",
        "@lmctl-ai/lmgit#main",
        "@lmctl-ai/lmgit#e1613098921f45271c3b7bbcb40f92f6b10beaed",
        "@owner/repo#",
        "@owner/repo#dev/foo",
        "@owner/repo#../../etc",
        "@owner/repo#invalid!ref",
        "@owner/repo#Latest",
    ] {
        let e = parse_spec(spec).unwrap_err();
        assert_eq!(e.code, ModuleErrorCode::SpecChannel, "{spec}");
        assert!(
            e.message.contains("only the latest and dev channels are supported"),
            "{spec}: {}",
            e.message
        );
    }
}

#[test]
fn parse_spec_rejects_syntax_errors_with_spec_syntax() {
    // `null` (non-string input) is not representable in Rust; blank strings cover the same branch.
    for spec in [
        "",
        "   ",
        "owner/repo",
        "@owner",
        "@owner/",
        "@/repo",
        "@../repo#latest",
        "@bad$owner/repo#latest",
        "@../repo",
        "@owner/..",
        "@bad$owner/repo",
        "@owner/repo$name",
        "@./repo",
        "@owner/.",
        "@owner/repo/extra",
        "@-owner/repo",
    ] {
        let e = parse_spec(spec).unwrap_err();
        assert_eq!(e.code, ModuleErrorCode::SpecSyntax, "{spec:?}");
    }
}

#[test]
fn parse_spec_error_codes_compare_to_strings() {
    let e = parse_spec("@owner/repo#main").unwrap_err();
    assert!(e.code == "SPEC_CHANNEL");
    assert_eq!(e.code.as_str(), "SPEC_CHANNEL");
    assert!(e.details.is_none());
}

// ---------------------------------------------------------------------------------------------
// cache_root, entry_dir, schema_file_name
// ---------------------------------------------------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn cache_root_uses_lmctl_home_when_present() {
    assert_eq!(
        cache_root_from(&env(&[("LMCTL_HOME", "/custom/lmctl")])),
        PathBuf::from("/custom/lmctl/modules")
    );
    assert_eq!(
        cache_root_from(&env(&[("LMCTL_HOME", "/custom/lmctl"), ("HOME", "/home/testuser")])),
        PathBuf::from("/custom/lmctl/modules")
    );
}

#[test]
fn cache_root_uses_home_when_lmctl_home_unset() {
    assert_eq!(
        cache_root_from(&env(&[("HOME", "/home/testuser")])),
        PathBuf::from("/home/testuser/.lmctl/modules")
    );
}

#[test]
fn cache_root_uses_home_when_lmctl_home_blank() {
    assert_eq!(
        cache_root_from(&env(&[("LMCTL_HOME", ""), ("HOME", "/home/testuser")])),
        PathBuf::from("/home/testuser/.lmctl/modules")
    );
    assert_eq!(
        cache_root_from(&env(&[("LMCTL_HOME", "  "), ("HOME", "/home/testuser")])),
        PathBuf::from("/home/testuser/.lmctl/modules")
    );
}

#[test]
fn entry_dir_formats_root_github_owner_repo_channel() {
    let dir = entry_dir(Path::new("/cache/root"), &must_parse("@owner/repo#dev")).unwrap();
    assert_eq!(dir, PathBuf::from("/cache/root/github.com/owner/repo/dev"));
}

#[test]
fn entry_dir_uses_latest_when_channel_omitted() {
    let dir = entry_dir(Path::new("/cache/root"), &must_parse("@owner/repo")).unwrap();
    assert_eq!(dir, PathBuf::from("/cache/root/github.com/owner/repo/latest"));
}

#[test]
fn entry_dir_normalizes_the_root() {
    let dir = entry_dir(Path::new("/cache/./x/../root"), &must_parse("@owner/repo")).unwrap();
    assert_eq!(dir, PathBuf::from("/cache/root/github.com/owner/repo/latest"));
    let rel = entry_dir(Path::new("relative/root"), &must_parse("@owner/repo")).unwrap();
    assert!(rel.is_absolute());
    assert!(rel.ends_with("relative/root/github.com/owner/repo/latest"));
}

#[test]
fn schema_file_name_formats_channel() {
    assert_eq!(schema_file_name(Channel::Latest), "cli.schema.latest.json");
    assert_eq!(schema_file_name(Channel::Dev), "cli.schema.dev.json");
}

#[test]
fn entry_dir_stays_within_root() {
    let dir = entry_dir(Path::new("/tmp/test-modules"), &must_parse("@owner/repo")).unwrap();
    assert!(dir.starts_with("/tmp/test-modules"));
}

#[test]
fn entry_dir_refuses_traversal_specs() {
    for (owner, repo) in [
        ("..", "escaped"),
        ("../..", "x"),
        ("owner", "../../x"),
        ("/etc", "repo"),
        ("a/../..", "repo"),
        ("..", ".."),
    ] {
        let e = entry_dir(Path::new("/cache/root"), &hostile(owner, repo)).unwrap_err();
        assert_eq!(e.code, ModuleErrorCode::PathTraversal, "{owner}/{repo}");
        assert!(e.message.contains("Path traversal detected"), "{}", e.message);
    }
    // Deliberate hardening over lmts: a hand-built spec with a segment parse_spec rejects is
    // refused even when it normalizes back inside github.com (lmts accepts `./x`).
    let e = entry_dir(Path::new("/cache/root"), &hostile(".", "x")).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::PathTraversal);
}

#[test]
fn remove_all_channels_refuses_dot_segments_that_name_github_com() {
    // lmts bug (shared before the fix): owner "." + repo "." resolves to github.com itself and
    // removing "all channels" would delete every cached module.
    let t = TempRoot::new("remove-dot");
    let keep = t.path().join("github.com").join("acme").join("tool").join("latest");
    std::fs::create_dir_all(&keep).unwrap();
    for (owner, repo) in [(".", "."), ("x", ".."), (".", "x")] {
        let e = remove(
            &hostile(owner, repo),
            &RemoveOptions {
                root: Some(t.path()),
                all_channels: true,
            },
        )
        .unwrap_err();
        assert_eq!(e.code, ModuleErrorCode::PathTraversal, "{owner}/{repo}");
    }
    assert!(keep.is_dir());
}

// ---------------------------------------------------------------------------------------------
// resolve_cached
// ---------------------------------------------------------------------------------------------

#[test]
fn resolve_not_installed_when_entry_dir_missing() {
    let t = TempRoot::new("resolve");
    let e = resolve_cached("@owner/repo", &ropts(t.path())).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::NotInstalled);
}

#[test]
fn resolve_not_installed_when_schema_missing() {
    let t = TempRoot::new("resolve");
    let spec = must_parse("@owner/repo");
    let dir = entry_dir(t.path(), &spec).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("meta.json"), r#"{"sha256":"abc"}"#).unwrap();
    let e = resolve_cached(&spec, &ropts(t.path())).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::NotInstalled);
}

#[test]
fn resolve_not_installed_when_meta_missing() {
    let t = TempRoot::new("resolve");
    let spec = must_parse("@owner/repo");
    let dir = entry_dir(t.path(), &spec).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(schema_file_name(spec.channel)), compact(&v1())).unwrap();
    let e = resolve_cached(&spec, &ropts(t.path())).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::NotInstalled);
}

#[test]
fn resolve_returns_verified_cached_module() {
    let t = TempRoot::new("resolve");
    let spec = must_parse("@owner/repo#dev");
    let dir = entry_dir(t.path(), &spec).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let schema = serde_json::to_string_pretty(&v1()).unwrap();
    let sha = sha_hex(schema.as_bytes());
    fs::write(dir.join("cli.schema.dev.json"), &schema).unwrap();
    let meta = json!({
        "spec": "@owner/repo#dev",
        "url": "https://raw.githubusercontent.com/owner/repo/HEAD/cli.schema.dev.json",
        "sha256": sha,
        "fetched_at": "2026-10-10T12:00:00Z",
        "tool": "mytool",
        "version": "1.0.0",
        "bytes": schema.len(),
    });
    fs::write(dir.join("meta.json"), meta.to_string()).unwrap();

    let m = resolve_cached(&spec, &ropts(t.path())).unwrap();
    assert_eq!(m.sha256, sha);
    assert_eq!(m.tool, "mytool");
    assert_eq!(m.version.as_deref(), Some("1.0.0"));
    assert_eq!(m.document.tool, "mytool");
    assert_eq!(m.path, dir.join("cli.schema.dev.json"));
    assert_eq!(m.dir, dir);
    assert_eq!(m.channel, Channel::Dev);
    assert_eq!(m.fetched_at, "2026-10-10T12:00:00Z");
    // No last_checked recorded: falls back to fetched_at.
    assert_eq!(m.last_checked, "2026-10-10T12:00:00Z");
    assert_eq!(m.etag, None);
}

#[test]
fn resolve_tamper_detection_returns_cache_corrupt_naming_file() {
    let t = TempRoot::new("resolve");
    let spec = must_parse("@owner/repo");
    let dir = entry_dir(t.path(), &spec).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let mut tampered = compact(&v1());
    tampered.extend_from_slice(b" // tampered");
    fs::write(dir.join("cli.schema.latest.json"), tampered).unwrap();
    fs::write(
        dir.join("meta.json"),
        json!({"spec": "@owner/repo#latest", "sha256": "original-sha", "fetched_at": "2026-10-10T12:00:00Z"})
            .to_string(),
    )
    .unwrap();

    let e = resolve_cached(&spec, &ropts(t.path())).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::CacheCorrupt);
    assert_eq!(e.file.as_deref(), Some(dir.join("cli.schema.latest.json").as_path()));
    assert!(e.message.contains("expected original-sha"), "{}", e.message);
    assert!(
        e.message
            .contains("run: lmbee module install @owner/repo#latest --force"),
        "{}",
        e.message
    );
}

#[test]
fn resolve_cache_corrupt_when_meta_is_not_json() {
    let t = TempRoot::new("resolve");
    install_doc(t.path(), "@owner/repo", &v1());
    let dir = entry_dir(t.path(), &must_parse("@owner/repo")).unwrap();
    fs::write(dir.join("meta.json"), "{not json").unwrap();
    let e = resolve_cached("@owner/repo", &ropts(t.path())).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::CacheCorrupt);
    assert_eq!(e.file.as_deref(), Some(dir.join("meta.json").as_path()));
    assert!(
        e.message
            .contains("run: lmbee module install @owner/repo#latest --force"),
        "{}",
        e.message
    );
}

#[test]
fn resolve_bad_json_when_hash_correct_document_is_not_json() {
    let t = TempRoot::new("resolve");
    let spec = must_parse("@owner/repo");
    let dir = entry_dir(t.path(), &spec).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("cli.schema.latest.json"), "not json").unwrap();
    fs::write(
        dir.join("meta.json"),
        json!({"spec": "@owner/repo#latest", "sha256": sha_hex(b"not json"), "fetched_at": "2026-10-10T12:00:00Z"})
            .to_string(),
    )
    .unwrap();
    let e = resolve_cached(&spec, &ropts(t.path())).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::BadJson);
    assert_eq!(e.file.as_deref(), Some(dir.join("cli.schema.latest.json").as_path()));
}

#[test]
fn resolve_invalid_schema_when_document_fails_validation() {
    let t = TempRoot::new("resolve");
    let spec = must_parse("@owner/repo");
    let dir = entry_dir(t.path(), &spec).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let mut doc = v1();
    doc["tool"] = json!("INVALID_TOOL_NAME");
    let bytes = compact(&doc);
    fs::write(dir.join("cli.schema.latest.json"), &bytes).unwrap();
    fs::write(
        dir.join("meta.json"),
        json!({"spec": "@owner/repo#latest", "sha256": sha_hex(&bytes), "fetched_at": "2026-10-10T12:00:00Z"})
            .to_string(),
    )
    .unwrap();
    let e = resolve_cached(&spec, &ropts(t.path())).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::InvalidSchema);
    assert!(e.issues.as_ref().is_some_and(|i| !i.is_empty()));
}

// ---------------------------------------------------------------------------------------------
// install
// ---------------------------------------------------------------------------------------------

#[test]
fn install_happy_path_layout_meta_and_permissions() {
    let t = TempRoot::new("install");
    let body = serde_json::to_string_pretty(&v1()).unwrap().into_bytes();
    let requests = RefCell::new(Vec::<HttpRequest>::new());
    let f = |r: &HttpRequest| {
        requests.borrow_mut().push(r.clone());
        response(200, &[("ETag", "\"e1\"")], body.clone())
    };
    let fixed = at("2026-10-10T10:00:00.000Z");
    let clock = move || fixed;
    let out = install(
        "@lmctl-ai/mytool#dev",
        &InstallOptions {
            now: Some(&clock),
            ..iopts(t.path(), &f)
        },
    )
    .unwrap();

    assert_eq!(out.action, InstallAction::Installed);
    assert_eq!(out.action.as_str(), "installed");
    assert_eq!(out.entry.tool, "mytool");
    assert_eq!(out.entry.fetched_at, "2026-10-10T10:00:00.000Z");
    assert_eq!(out.entry.fetched_at, to_iso_string(fixed));
    assert_eq!(out.entry.last_checked, "2026-10-10T10:00:00.000Z");
    assert_eq!(out.entry.etag.as_deref(), Some("\"e1\""));
    assert_eq!(out.entry.sha256, sha_hex(&body));

    // One plain GET of the raw URL with the install defaults.
    let reqs = requests.borrow();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].url,
        "https://raw.githubusercontent.com/lmctl-ai/mytool/HEAD/cli.schema.dev.json"
    );
    assert!(reqs[0].headers.is_empty());
    assert_eq!(reqs[0].timeout, Duration::from_millis(10_000));

    // Layout: root/github.com/<owner>/<repo>/<channel>/{cli.schema.<channel>.json, meta.json}, no temp files.
    let dir = t.path().join("github.com/lmctl-ai/mytool/dev");
    assert_eq!(out.entry.dir, dir);
    assert_eq!(out.entry.path, dir.join("cli.schema.dev.json"));
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["cli.schema.dev.json", "meta.json"]);
    assert_eq!(fs::read(dir.join("cli.schema.dev.json")).unwrap(), body);

    // meta.json: exact keys, order and formatting (JSON.stringify(meta, null, 2) + "\n").
    let expected = format!(
        "{{\n  \"spec\": \"@lmctl-ai/mytool#dev\",\n  \"url\": \"https://raw.githubusercontent.com/lmctl-ai/mytool/HEAD/cli.schema.dev.json\",\n  \"sha256\": \"{}\",\n  \"fetched_at\": \"2026-10-10T10:00:00.000Z\",\n  \"last_checked\": \"2026-10-10T10:00:00.000Z\",\n  \"etag\": \"\\\"e1\\\"\",\n  \"channel\": \"dev\",\n  \"tool\": \"mytool\",\n  \"version\": \"1.0.0\",\n  \"bytes\": {}\n}}\n",
        sha_hex(&body),
        body.len()
    );
    let meta_text = fs::read_to_string(dir.join("meta.json")).unwrap();
    assert_eq!(meta_text, expected);
    let keys = keys_in_file_order(&meta_text);
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
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("cli.schema.dev.json")), 0o600);
        assert_eq!(mode(&dir.join("meta.json")), 0o600);
    }
}

#[test]
fn install_meta_omits_etag_and_defaults_version_to_unknown() {
    let t = TempRoot::new("install");
    let mut doc = v1();
    doc.as_object_mut().unwrap().remove("version");
    let body = compact(&doc);
    let f = |_: &HttpRequest| ok_body(body.clone());
    let out = install("@owner/repo", &iopts(t.path(), &f)).unwrap();
    assert_eq!(out.entry.version, None);
    assert_eq!(out.entry.etag, None);
    let meta_text = fs::read_to_string(out.entry.dir.join("meta.json")).unwrap();
    let meta: Value = serde_json::from_str(&meta_text).unwrap();
    let keys = keys_in_file_order(&meta_text);
    assert_eq!(
        keys,
        vec![
            "spec",
            "url",
            "sha256",
            "fetched_at",
            "last_checked",
            "channel",
            "tool",
            "version",
            "bytes"
        ]
    );
    assert_eq!(meta["version"], "unknown");
    assert_eq!(meta["channel"], "latest");
}

#[test]
fn install_is_idempotent_without_refetching() {
    let t = TempRoot::new("install");
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    let first = install("@owner/repo", &iopts(t.path(), &f)).unwrap();
    assert_eq!(calls.get(), 1);
    let second = install("@owner/repo", &iopts(t.path(), &f)).unwrap();
    assert_eq!(second.action, InstallAction::Unchanged);
    assert_eq!(second.entry, first.entry);
    assert_eq!(calls.get(), 1);
}

#[test]
fn install_force_refetches() {
    let t = TempRoot::new("install");
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    install("@owner/repo", &iopts(t.path(), &f)).unwrap();
    assert_eq!(calls.get(), 1);
    install(
        "@owner/repo",
        &InstallOptions {
            force: true,
            ..iopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 2);
}

#[test]
fn install_force_replaces_even_with_identical_sha() {
    let t = TempRoot::new("install");
    let f = |_: &HttpRequest| ok_body(compact(&v1()));
    install("@owner/repo", &iopts(t.path(), &f)).unwrap();
    let out = install(
        "@owner/repo",
        &InstallOptions {
            force: true,
            ..iopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.action, InstallAction::Replaced);
    assert_eq!(out.action.as_str(), "replaced");
}

#[test]
fn install_force_with_new_content_replaces_entry() {
    let t = TempRoot::new("install");
    install_doc(t.path(), "@owner/repo", &v1());
    let f = |_: &HttpRequest| ok_body(compact(&v2()));
    let out = install(
        "@owner/repo",
        &InstallOptions {
            force: true,
            ..iopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.action, InstallAction::Replaced);
    assert_eq!(
        resolve_cached("@owner/repo", &ropts(t.path()))
            .unwrap()
            .version
            .as_deref(),
        Some("2.0.0")
    );
}

#[test]
fn install_over_corrupt_entry_refetches_and_reports_installed() {
    let t = TempRoot::new("install");
    install_doc(t.path(), "@owner/repo", &v1());
    let dir = entry_dir(t.path(), &must_parse("@owner/repo")).unwrap();
    fs::write(dir.join("cli.schema.latest.json"), "tampered").unwrap();
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    let out = install("@owner/repo", &iopts(t.path(), &f)).unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(out.action, InstallAction::Installed);
    assert!(resolve_cached("@owner/repo", &ropts(t.path())).is_ok());
}

#[test]
fn install_404_names_expected_file_at_root_of_default_branch() {
    let t = TempRoot::new("install");
    let f = |_: &HttpRequest| response(404, &[], "Not Found");
    let e = install("@owner/repo#dev", &iopts(t.path(), &f)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("cli.schema.dev.json"), "{}", e.message);
    assert!(e.message.contains("root of the default branch"), "{}", e.message);
    let details = e.details.expect("details");
    assert_eq!(details["status"], 404);
    assert_eq!(
        details["url"],
        "https://raw.githubusercontent.com/owner/repo/HEAD/cli.schema.dev.json"
    );
    assert_eq!(details["file"], "cli.schema.dev.json");
    // Nothing is written on failure.
    assert!(!t.path().join("github.com").exists());
}

#[test]
fn install_404_leaves_existing_entry_untouched() {
    let t = TempRoot::new("install");
    install_doc(t.path(), "@owner/repo", &v1());
    let f = |_: &HttpRequest| response(404, &[], "Not Found");
    assert!(install(
        "@owner/repo",
        &InstallOptions {
            force: true,
            ..iopts(t.path(), &f)
        }
    )
    .is_err());
    assert_eq!(resolve_cached("@owner/repo", &ropts(t.path())).unwrap().tool, "mytool");
}

/// Install `v1`, run a failing forced install, and assert the code and byte-identical files.
fn assert_forced_failure_keeps_entry(
    tag: &str,
    fetch: &dyn Fetcher,
    max_bytes: Option<usize>,
    code: ModuleErrorCode,
) -> lmrust::modules::ModuleError {
    let t = TempRoot::new(tag);
    install_doc(t.path(), "@owner/repo", &v1());
    let dir = entry_dir(t.path(), &must_parse("@owner/repo")).unwrap();
    let schema_before = fs::read(dir.join("cli.schema.latest.json")).unwrap();
    let meta_before = fs::read(dir.join("meta.json")).unwrap();

    let e = install(
        "@owner/repo",
        &InstallOptions {
            root: Some(t.path()),
            fetcher: Some(fetch),
            force: true,
            max_bytes,
            timeout: Some(Duration::from_millis(50)),
            ..InstallOptions::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code, code, "{}", e.message);

    assert!(resolve_cached("@owner/repo", &ropts(t.path())).is_ok());
    assert_eq!(fs::read(dir.join("cli.schema.latest.json")).unwrap(), schema_before);
    assert_eq!(fs::read(dir.join("meta.json")).unwrap(), meta_before);
    let names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    e
}

#[test]
fn install_500_returns_fetch_failed_and_keeps_entry() {
    let f = |_: &HttpRequest| response(500, &[], "Server Error");
    let e = assert_forced_failure_keeps_entry("install", &f, None, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("HTTP 500 Internal Server Error"), "{}", e.message);
    let details = e.details.expect("details");
    assert_eq!(details["status"], 500);
    assert_eq!(
        details["url"],
        "https://raw.githubusercontent.com/owner/repo/HEAD/cli.schema.latest.json"
    );
    assert!(details.get("file").is_none());
}

#[test]
fn install_timeout_returns_fetch_failed_and_keeps_entry() {
    let f = |_: &HttpRequest| Err(FetchError::Timeout("The operation was aborted".into()));
    let e = assert_forced_failure_keeps_entry("install", &f, None, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("timed out after 50ms"), "{}", e.message);
}

#[test]
fn install_body_read_errors_return_fetch_failed_and_keep_entry() {
    let f = |_: &HttpRequest| {
        Ok(HttpResponse {
            status: 200,
            status_text: "OK".into(),
            headers: vec![],
            body: Box::new(FailingBody(std::io::ErrorKind::TimedOut)),
        })
    };
    let e = assert_forced_failure_keeps_entry("install", &f, None, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("Fetch stream timed out after 50ms"), "{}", e.message);

    let g = |_: &HttpRequest| {
        Ok(HttpResponse {
            status: 200,
            status_text: "OK".into(),
            headers: vec![],
            body: Box::new(FailingBody(std::io::ErrorKind::ConnectionReset)),
        })
    };
    let e = assert_forced_failure_keeps_entry("install", &g, None, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("Error reading response stream"), "{}", e.message);
    assert!(e.message.contains("boom"), "{}", e.message);
}

#[test]
fn install_oversized_payload_returns_output_limit_and_keeps_entry() {
    let f = |_: &HttpRequest| ok_body("x".repeat(2000));
    let e = assert_forced_failure_keeps_entry("install", &f, Some(500), ModuleErrorCode::OutputLimit);
    assert!(
        e.message.contains("exceeds maximum size limit of 500 bytes"),
        "{}",
        e.message
    );
}

#[test]
fn install_payload_exactly_at_cap_is_accepted() {
    let t = TempRoot::new("install");
    let body = compact(&v1());
    let len = body.len();
    let f = |_: &HttpRequest| ok_body(body.clone());
    let out = install(
        "@owner/repo",
        &InstallOptions {
            max_bytes: Some(len),
            ..iopts(t.path(), &f)
        },
    )
    .unwrap();
    assert_eq!(out.action, InstallAction::Installed);
    let t2 = TempRoot::new("install");
    let e = install(
        "@owner/repo",
        &InstallOptions {
            max_bytes: Some(len - 1),
            ..iopts(t2.path(), &f)
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::OutputLimit);
}

#[test]
fn install_default_cap_is_2_mib() {
    assert_eq!(MAX_SCHEMA_BYTES, 2 * 1024 * 1024);
    let t = TempRoot::new("install");
    // A valid document padded with trailing whitespace to 1.5 MiB installs under the default cap...
    let mut big = compact(&v1());
    big.resize(1536 * 1024, b' ');
    let f = |_: &HttpRequest| ok_body(big.clone());
    assert_eq!(
        install("@owner/big", &iopts(t.path(), &f)).unwrap().action,
        InstallAction::Installed
    );
    // ...and 2 MiB + 1 byte does not.
    let mut huge = compact(&v1());
    huge.resize(MAX_SCHEMA_BYTES + 1, b' ');
    let g = |_: &HttpRequest| ok_body(huge.clone());
    let e = install("@owner/huge", &iopts(t.path(), &g)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::OutputLimit);
    assert!(e.message.contains("2097152 bytes"), "{}", e.message);
}

#[test]
fn install_bad_json_returns_bad_json_and_keeps_entry() {
    let f = |_: &HttpRequest| ok_body("<html>Error</html>");
    let e = assert_forced_failure_keeps_entry("install", &f, None, ModuleErrorCode::BadJson);
    assert!(e.message.contains("cli.schema.latest.json"), "{}", e.message);
}

#[test]
fn install_invalid_schema_returns_issues_and_keeps_entry() {
    let f = |_: &HttpRequest| ok_body(compact(&json!({"schema": "cli.schema.v1", "tool": "INVALID_TOOL"})));
    let e = assert_forced_failure_keeps_entry("install", &f, None, ModuleErrorCode::InvalidSchema);
    let issues = e.issues.expect("issues");
    assert!(!issues.is_empty());
    assert!(e.message.contains(&issues[0].message), "{}", e.message);
}

#[test]
fn install_cross_host_redirect_is_refused() {
    let t = TempRoot::new("install");
    let calls = Cell::new(0);
    let f = |r: &HttpRequest| {
        calls.set(calls.get() + 1);
        if r.url.contains("raw.githubusercontent.com") {
            response(302, &[("location", "https://evil-host.com/schema.json")], "")
        } else {
            ok_body(compact(&v1()))
        }
    };
    let e = install("@owner/repo", &iopts(t.path(), &f)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("refused"), "{}", e.message);
    assert_eq!(calls.get(), 1);
}

#[test]
fn install_refuses_http_protocol_relative_and_lookalike_redirects() {
    for location in [
        "http://raw.githubusercontent.com/owner/repo/HEAD/cli.schema.latest.json",
        "//evil.example/cli.schema.latest.json",
        "https://raw.githubusercontent.com.evil.com/x",
        "ftp://raw.githubusercontent.com/x",
    ] {
        let t = TempRoot::new("install");
        let calls = Cell::new(0);
        let f = |_: &HttpRequest| {
            calls.set(calls.get() + 1);
            response(301, &[("Location", location)], "")
        };
        let e = install("@owner/repo", &iopts(t.path(), &f)).unwrap_err();
        assert_eq!(e.code, ModuleErrorCode::FetchFailed, "{location}");
        assert!(e.message.contains("refused"), "{location}: {}", e.message);
        assert_eq!(calls.get(), 1, "{location}");
    }
}

#[test]
fn install_refuses_backslash_cross_host_redirect() {
    // lmts: new URL("\\\\evil.com/x", base).hostname === "evil.com" -> refused without a second fetch.
    let t = TempRoot::new("install");
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            response(302, &[("location", "\\\\evil.com/x")], "")
        } else {
            ok_body(compact(&v1()))
        }
    };
    let e = install("@owner/repo", &iopts(t.path(), &f)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("refused"), "{}", e.message);
    assert_eq!(calls.get(), 1);
}

#[test]
fn install_follows_same_host_https_redirect_once() {
    let t = TempRoot::new("install");
    let redirects = Cell::new(0);
    let urls = RefCell::new(Vec::<String>::new());
    let f = |r: &HttpRequest| {
        urls.borrow_mut().push(r.url.clone());
        if r.url.contains("HEAD") {
            redirects.set(redirects.get() + 1);
            response(
                302,
                &[(
                    "location",
                    "https://raw.githubusercontent.com/owner/repo/main/cli.schema.latest.json",
                )],
                "",
            )
        } else {
            ok_body(compact(&v1()))
        }
    };
    let out = install("@owner/repo", &iopts(t.path(), &f)).unwrap();
    assert_eq!(out.action, InstallAction::Installed);
    assert_eq!(redirects.get(), 1);
    assert_eq!(
        *urls.borrow(),
        vec![
            "https://raw.githubusercontent.com/owner/repo/HEAD/cli.schema.latest.json".to_string(),
            "https://raw.githubusercontent.com/owner/repo/main/cli.schema.latest.json".to_string(),
        ]
    );
    // meta.json records the canonical URL, not the redirect target.
    let meta: Value = serde_json::from_str(&fs::read_to_string(out.entry.dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(
        meta["url"],
        "https://raw.githubusercontent.com/owner/repo/HEAD/cli.schema.latest.json"
    );
}

#[test]
fn install_resolves_relative_redirect_against_request_url() {
    let t = TempRoot::new("install");
    let urls = RefCell::new(Vec::<String>::new());
    let f = |r: &HttpRequest| {
        urls.borrow_mut().push(r.url.clone());
        if r.url.contains("HEAD") {
            response(302, &[("Location", "/owner/repo/main/cli.schema.latest.json")], "")
        } else {
            ok_body(compact(&v1()))
        }
    };
    install("@owner/repo", &iopts(t.path(), &f)).unwrap();
    assert_eq!(
        urls.borrow()[1],
        "https://raw.githubusercontent.com/owner/repo/main/cli.schema.latest.json"
    );
}

#[test]
fn install_follows_at_most_one_redirect() {
    let t = TempRoot::new("install");
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
    let e = install("@owner/repo", &iopts(t.path(), &f)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("HTTP 302"), "{}", e.message);
    assert_eq!(calls.get(), 2);
}

#[test]
fn install_redirect_without_location_fails() {
    let t = TempRoot::new("install");
    let f = |_: &HttpRequest| response(301, &[], "");
    let e = install("@owner/repo", &iopts(t.path(), &f)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("Location header"), "{}", e.message);
}

#[test]
fn install_generic_network_error_returns_fetch_failed() {
    let t = TempRoot::new("install");
    let f = |_: &HttpRequest| Err(FetchError::Network("Connection refused".into()));
    let e = install("@owner/repo", &iopts(t.path(), &f)).unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::FetchFailed);
    assert!(e.message.contains("Connection refused"), "{}", e.message);
}

#[test]
fn install_rejects_bad_specs_before_fetching() {
    let t = TempRoot::new("install");
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    assert_eq!(
        install("owner/repo", &iopts(t.path(), &f)).unwrap_err().code,
        ModuleErrorCode::SpecSyntax
    );
    assert_eq!(
        install("@owner/repo#main", &iopts(t.path(), &f)).unwrap_err().code,
        ModuleErrorCode::SpecChannel
    );
    assert_eq!(calls.get(), 0);
}

#[test]
fn concurrent_installs_of_same_spec_do_not_corrupt_entry() {
    let t = TempRoot::new("install");
    let barrier = std::sync::Barrier::new(3);
    let root = t.path();
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let barrier = &barrier;
                s.spawn(move || {
                    let f = |_: &HttpRequest| {
                        barrier.wait();
                        ok_body(compact(&v1()))
                    };
                    install("@owner/repo", &iopts(root, &f)).map(|o| o.action)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for r in results {
        assert!(r.is_ok(), "{r:?}");
    }
    assert!(resolve_cached("@owner/repo", &ropts(t.path())).is_ok());
    let dir = entry_dir(t.path(), &must_parse("@owner/repo")).unwrap();
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
}

// ---------------------------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------------------------

#[test]
fn list_empty_cache_root() {
    let t = TempRoot::new("list");
    assert!(list(&ListOptions {
        root: Some(t.path()),
        path_var: Some("")
    })
    .is_empty());
    let missing = t.path().join("does-not-exist");
    assert!(list(&ListOptions {
        root: Some(&missing),
        path_var: Some("")
    })
    .is_empty());
}

#[test]
fn list_reports_installed_entries() {
    let t = TempRoot::new("list");
    install_doc(t.path(), "@owner/repo1#dev", &v1());
    install_doc(t.path(), "@owner/repo2", &v1());
    let path_var = empty_path_dir(&t);

    let entries = list(&ListOptions {
        root: Some(t.path()),
        path_var: Some(&path_var),
    });
    assert_eq!(entries.len(), 2);
    let e1 = entries.iter().find(|e| e.spec == "@owner/repo1#dev").expect("repo1");
    let e2 = entries.iter().find(|e| e.spec == "@owner/repo2#latest").expect("repo2");
    assert_eq!(e1.channel, Some(Channel::Dev));
    assert_eq!(e1.tool.as_deref(), Some("mytool"));
    assert_eq!(e1.version.as_deref(), Some("1.0.0"));
    assert_eq!(e1.sha256.as_deref(), Some(sha_hex(&compact(&v1())).as_str()));
    assert!(e1.fetched_at.is_some());
    assert_eq!(e1.dir, t.path().join("github.com/owner/repo1/dev"));
    assert_eq!(e1.binary_on_path, Some(None)); // checked, not on PATH (lmts: null)
    assert_eq!(e1.problem, None);
    assert_eq!(e2.channel, Some(Channel::Latest));
}

#[cfg(unix)]
#[test]
fn list_reports_binary_on_path() {
    let t = TempRoot::new("list");
    install_doc(t.path(), "@owner/repo", &v1());
    let bin = t.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("mytool");
    fs::write(&exe, "#!/bin/sh\necho ok\n").unwrap();
    make_executable(&exe, 0o755);
    let path_var = format!("relative:{}", bin.display());
    let entries = list(&ListOptions {
        root: Some(t.path()),
        path_var: Some(&path_var),
    });
    assert_eq!(entries[0].binary_on_path, Some(Some(exe)));
}

#[test]
fn list_is_sorted_by_owner_repo_channel() {
    // Intentional lmrust difference: lmts returns readdir order.
    let t = TempRoot::new("list");
    install_doc(t.path(), "@zeta/b", &v1());
    install_doc(t.path(), "@owner/repo2", &v1());
    install_doc(t.path(), "@owner/repo1#latest", &v1());
    install_doc(t.path(), "@owner/repo1#dev", &v1());
    install_doc(t.path(), "@alpha/a", &v1());
    let specs: Vec<String> = list(&ListOptions {
        root: Some(t.path()),
        path_var: Some(""),
    })
    .into_iter()
    .map(|e| e.spec)
    .collect();
    assert_eq!(
        specs,
        vec![
            "@alpha/a#latest",
            "@owner/repo1#dev",
            "@owner/repo1#latest",
            "@owner/repo2#latest",
            "@zeta/b#latest"
        ]
    );
}

#[test]
fn list_reports_incomplete_entry_as_problem_row() {
    let t = TempRoot::new("list");
    install_doc(t.path(), "@owner/good", &v1());
    let corrupt = t.path().join("github.com/owner/bad/latest");
    fs::create_dir_all(&corrupt).unwrap();
    fs::write(corrupt.join("cli.schema.latest.json"), "{}").unwrap();

    let entries = list(&ListOptions {
        root: Some(t.path()),
        path_var: Some(""),
    });
    assert_eq!(entries.len(), 2);
    let bad = entries.iter().find(|e| e.spec == "@owner/bad#latest").expect("bad row");
    assert_eq!(
        bad.problem.as_deref(),
        Some("Missing cli.schema.latest.json or meta.json")
    );
    assert_eq!(bad.channel, Some(Channel::Latest));
    assert_eq!(bad.binary_on_path, None);
    assert_eq!(bad.tool, None);
    assert_eq!(bad.dir, corrupt);
}

#[test]
fn list_reports_checksum_mismatch_as_problem_row() {
    let t = TempRoot::new("list");
    install_doc(t.path(), "@owner/tampered", &v1());
    let entry = resolve_cached("@owner/tampered", &ropts(t.path())).unwrap();
    fs::write(&entry.path, "tampered content").unwrap();
    let entries = list(&ListOptions {
        root: Some(t.path()),
        path_var: Some(""),
    });
    let row = entries
        .iter()
        .find(|e| e.spec == "@owner/tampered#latest")
        .expect("row");
    let problem = row.problem.as_deref().unwrap();
    assert!(problem.contains("Checksum mismatch"), "{problem}");
    assert!(problem.contains(&sha_hex(b"tampered content")), "{problem}");
}

#[test]
fn list_reports_unparseable_meta_and_unknown_channels() {
    let t = TempRoot::new("list");
    install_doc(t.path(), "@owner/repo", &v1());
    let dir = entry_dir(t.path(), &must_parse("@owner/repo")).unwrap();
    fs::write(dir.join("meta.json"), "{oops").unwrap();
    let main = t.path().join("github.com/owner/repo/main");
    fs::create_dir_all(&main).unwrap();

    let entries = list(&ListOptions {
        root: Some(t.path()),
        path_var: Some(""),
    });
    assert_eq!(entries.len(), 2);
    let latest = entries.iter().find(|e| e.spec == "@owner/repo#latest").unwrap();
    assert!(
        latest
            .problem
            .as_deref()
            .unwrap()
            .starts_with("Unreadable or corrupt files"),
        "{latest:?}"
    );
    let main_row = entries.iter().find(|e| e.spec == "@owner/repo#main").unwrap();
    assert_eq!(main_row.channel, None);
    assert_eq!(
        main_row.problem.as_deref(),
        Some("Missing cli.schema.main.json or meta.json")
    );
}

#[test]
fn list_ignores_non_directory_files_in_tree() {
    let t = TempRoot::new("list");
    let gh = t.path().join("github.com");
    fs::create_dir_all(gh.join("owner")).unwrap();
    fs::write(gh.join("stray-file.txt"), "hello").unwrap();
    fs::write(gh.join("owner/stray-file-2.txt"), "hello").unwrap();
    assert!(list(&ListOptions {
        root: Some(t.path()),
        path_var: Some("")
    })
    .is_empty());
}

// ---------------------------------------------------------------------------------------------
// upgrade
// ---------------------------------------------------------------------------------------------

fn uopts<'a>(root: &'a Path, fetcher: &'a dyn Fetcher) -> UpgradeOptions<'a> {
    UpgradeOptions {
        root: Some(root),
        fetcher: Some(fetcher),
        now: None,
    }
}

#[test]
fn upgrade_identical_sha_is_unchanged() {
    let t = TempRoot::new("upgrade");
    install_doc(t.path(), "@owner/moving", &v1());
    let f = |_: &HttpRequest| ok_body(compact(&v1()));
    let specs = [SpecRef::from("@owner/moving")];
    let out = upgrade(Some(&specs), &uopts(t.path(), &f));
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0],
        UpgradeOutcome::Unchanged {
            spec: "@owner/moving#latest".into(),
            channel: Channel::Latest,
            sha256: sha_hex(&compact(&v1())),
            version: Some("1.0.0".into()),
        }
    );
}

#[test]
fn upgrade_different_sha_replaces_entry_and_reports_operation_diff() {
    let t = TempRoot::new("upgrade");
    let version = Cell::new(1);
    let f = |_: &HttpRequest| {
        let doc = if version.get() == 1 { v1() } else { v2() };
        ok_body(compact(&doc))
    };
    install("@owner/moving#dev", &iopts(t.path(), &f)).unwrap();
    version.set(2);

    let specs = [SpecRef::from("@owner/moving#dev")];
    let out = upgrade(Some(&specs), &uopts(t.path(), &f));
    assert_eq!(out.len(), 1);
    match &out[0] {
        UpgradeOutcome::Updated {
            spec,
            channel,
            from_sha,
            to_sha,
            from_version,
            to_version,
            operations_added,
            operations_removed,
        } => {
            assert_eq!(spec, "@owner/moving#dev");
            assert_eq!(*channel, Channel::Dev);
            assert_eq!(*from_sha, sha_hex(&compact(&v1())));
            assert_eq!(*to_sha, sha_hex(&compact(&v2())));
            assert_eq!(from_version.as_deref(), Some("1.0.0"));
            assert_eq!(to_version.as_deref(), Some("2.0.0"));
            assert_eq!(operations_added, &vec!["batch".to_string()]);
            assert_eq!(operations_removed, &vec!["apply".to_string()]);
        }
        other => panic!("expected updated, got {other:?}"),
    }
    let updated = resolve_cached("@owner/moving#dev", &ropts(t.path())).unwrap();
    assert_eq!(updated.version.as_deref(), Some("2.0.0"));
}

#[test]
fn upgrade_without_specs_upgrades_every_installed_entry() {
    let t = TempRoot::new("upgrade");
    install_doc(t.path(), "@owner/m1", &v1());
    install_doc(t.path(), "@owner/m2#dev", &v1());
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    let out = upgrade(None, &uopts(t.path(), &f));
    assert_eq!(out.len(), 2);
    assert_eq!(calls.get(), 2);
    let m1 = out.iter().find(|o| outcome_spec(o) == "@owner/m1#latest").unwrap();
    assert_eq!(action(m1), "unchanged");
    let m2 = out.iter().find(|o| outcome_spec(o) == "@owner/m2#dev").unwrap();
    assert_eq!(action(m2), "unchanged");
}

#[test]
fn upgrade_without_specs_reports_corrupt_entries_as_failed_and_skips_unknown_channels() {
    let t = TempRoot::new("upgrade");
    install_doc(t.path(), "@owner/ok", &v1());
    install_doc(t.path(), "@owner/broken", &v1());
    let dir = entry_dir(t.path(), &must_parse("@owner/broken")).unwrap();
    fs::write(dir.join("cli.schema.latest.json"), "tampered").unwrap();
    fs::create_dir_all(t.path().join("github.com/owner/ok/main")).unwrap();

    let f = |_: &HttpRequest| ok_body(compact(&v1()));
    let out = upgrade(None, &uopts(t.path(), &f));
    assert_eq!(out.len(), 2, "{out:?}");
    match out.iter().find(|o| outcome_spec(o) == "@owner/broken#latest").unwrap() {
        UpgradeOutcome::Failed { channel, error, .. } => {
            assert_eq!(*channel, Some(Channel::Latest));
            assert!(error.contains("hash mismatch"), "{error}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        action(out.iter().find(|o| outcome_spec(o) == "@owner/ok#latest").unwrap()),
        "unchanged"
    );
}

#[test]
fn upgrade_partial_failure_does_not_stop_others() {
    let t = TempRoot::new("upgrade");
    install_doc(t.path(), "@owner/ok", &v1());
    install_doc(t.path(), "@owner/failing", &v1());
    let f = |r: &HttpRequest| {
        if r.url.contains("failing") {
            response(500, &[], "Server Error")
        } else {
            ok_body(compact(&v1()))
        }
    };
    let specs = [SpecRef::from("@owner/ok"), SpecRef::from("@owner/failing")];
    let out = upgrade(Some(&specs), &uopts(t.path(), &f));
    assert_eq!(out.len(), 2);
    assert_eq!(
        action(out.iter().find(|o| outcome_spec(o) == "@owner/ok#latest").unwrap()),
        "unchanged"
    );
    match out.iter().find(|o| outcome_spec(o) == "@owner/failing#latest").unwrap() {
        UpgradeOutcome::Failed { channel, error, .. } => {
            assert_eq!(*channel, Some(Channel::Latest));
            assert!(error.contains("HTTP 500"), "{error}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn upgrade_invalid_spec_returns_single_failure() {
    let t = TempRoot::new("upgrade");
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    let specs = [SpecRef::from("invalid-spec-without-at")];
    let out = upgrade(Some(&specs), &uopts(t.path(), &f));
    assert_eq!(out.len(), 1);
    match &out[0] {
        UpgradeOutcome::Failed { spec, channel, .. } => {
            assert_eq!(spec, "invalid-spec-without-at");
            assert_eq!(*channel, None);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(calls.get(), 0);
}

#[test]
fn upgrade_not_installed_fails_without_fetching() {
    let t = TempRoot::new("upgrade");
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    let specs = [SpecRef::from("@owner/not-installed")];
    let out = upgrade(Some(&specs), &uopts(t.path(), &f));
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "failed");
    assert_eq!(calls.get(), 0);
}

#[test]
fn upgrade_refetch_failure_is_failed_and_keeps_entry() {
    let t = TempRoot::new("upgrade");
    install_doc(t.path(), "@owner/moving", &v1());
    let f = |_: &HttpRequest| response(500, &[], "500 Internal Error");
    let specs = [SpecRef::from("@owner/moving")];
    let out = upgrade(Some(&specs), &uopts(t.path(), &f));
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "failed");
    assert_eq!(
        resolve_cached("@owner/moving", &ropts(t.path()))
            .unwrap()
            .version
            .as_deref(),
        Some("1.0.0")
    );
}

// ---------------------------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------------------------

#[test]
fn remove_single_channel_and_prune_empty_parents() {
    let t = TempRoot::new("remove");
    install_doc(t.path(), "@owner/repo#dev", &v1());
    let dir = entry_dir(t.path(), &must_parse("@owner/repo#dev")).unwrap();
    let out = remove(
        "@owner/repo#dev",
        &RemoveOptions {
            root: Some(t.path()),
            all_channels: false,
        },
    )
    .unwrap();
    assert_eq!(out.removed, vec![dir]);
    assert_eq!(
        resolve_cached("@owner/repo#dev", &ropts(t.path())).unwrap_err().code,
        ModuleErrorCode::NotInstalled
    );
    assert!(!t.path().join("github.com/owner/repo").exists());
    assert!(!t.path().join("github.com/owner").exists());
    assert!(!t.path().join("github.com").exists());
    assert!(t.path().exists(), "the cache root itself is never pruned");
}

#[test]
fn remove_single_channel_keeps_siblings() {
    let t = TempRoot::new("remove");
    install_doc(t.path(), "@owner/repo#dev", &v1());
    install_doc(t.path(), "@owner/repo#latest", &v1());
    install_doc(t.path(), "@owner/other", &v1());
    remove(
        "@owner/repo#dev",
        &RemoveOptions {
            root: Some(t.path()),
            all_channels: false,
        },
    )
    .unwrap();
    assert!(resolve_cached("@owner/repo#latest", &ropts(t.path())).is_ok());
    remove(
        "@owner/repo",
        &RemoveOptions {
            root: Some(t.path()),
            all_channels: false,
        },
    )
    .unwrap();
    assert!(!t.path().join("github.com/owner/repo").exists());
    assert!(resolve_cached("@owner/other", &ropts(t.path())).is_ok());
}

#[test]
fn remove_all_channels() {
    let t = TempRoot::new("remove");
    install_doc(t.path(), "@owner/repo#latest", &v1());
    install_doc(t.path(), "@owner/repo#dev", &v1());
    let out = remove(
        "@owner/repo",
        &RemoveOptions {
            root: Some(t.path()),
            all_channels: true,
        },
    )
    .unwrap();
    assert_eq!(out.removed, vec![t.path().join("github.com/owner/repo")]);
    assert!(!t.path().join("github.com/owner/repo").exists());
}

#[test]
fn remove_not_installed() {
    let t = TempRoot::new("remove");
    let e = remove(
        "@owner/nonexistent",
        &RemoveOptions {
            root: Some(t.path()),
            all_channels: false,
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::NotInstalled);
    let e = remove(
        "@owner/nonexistent",
        &RemoveOptions {
            root: Some(t.path()),
            all_channels: true,
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::NotInstalled);
    let e = remove(
        "bad",
        &RemoveOptions {
            root: Some(t.path()),
            all_channels: false,
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::SpecSyntax);
}

#[test]
fn remove_refuses_traversal_and_deletes_nothing() {
    let t = TempRoot::new("remove");
    // A directory that the hostile spec points at (inside the root but outside github.com).
    let outside = t.path().join("escaped/latest");
    fs::create_dir_all(&outside).unwrap();
    let spec = hostile("..", "escaped");
    let e = remove(
        &spec,
        &RemoveOptions {
            root: Some(t.path()),
            all_channels: false,
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::PathTraversal);
    let e = remove(
        &spec,
        &RemoveOptions {
            root: Some(t.path()),
            all_channels: true,
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ModuleErrorCode::PathTraversal);
    assert!(outside.exists());
}

// ---------------------------------------------------------------------------------------------
// PATH_TRAVERSAL through the store
// ---------------------------------------------------------------------------------------------

#[test]
fn resolve_and_install_refuse_traversal_specs() {
    let t = TempRoot::new("edge");
    let spec = hostile("..", "escaped");
    assert_eq!(
        resolve_cached(&spec, &ropts(t.path())).unwrap_err().code,
        ModuleErrorCode::PathTraversal
    );
    let calls = Cell::new(0);
    let f = |_: &HttpRequest| {
        calls.set(calls.get() + 1);
        ok_body(compact(&v1()))
    };
    assert_eq!(
        install(&spec, &iopts(t.path(), &f)).unwrap_err().code,
        ModuleErrorCode::PathTraversal
    );
    assert_eq!(calls.get(), 0);
    assert!(!t.path().join("escaped").exists());
}

// ---------------------------------------------------------------------------------------------
// find_on_path
// ---------------------------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn find_on_path_finds_executable_in_absolute_entry() {
    let t = TempRoot::new("path");
    let bin = t.path().join("mytool");
    fs::write(&bin, "#!/bin/sh\necho ok\n").unwrap();
    make_executable(&bin, 0o755);
    assert_eq!(find_on_path("mytool", Some(&t.path().to_string_lossy())), Some(bin));
}

#[cfg(unix)]
#[test]
fn find_on_path_first_match_wins_and_skips_directories() {
    let t = TempRoot::new("path");
    let a = t.path().join("a");
    let b = t.path().join("b");
    let c = t.path().join("c");
    fs::create_dir_all(a.join("mytool")).unwrap(); // a directory named like the tool
    fs::create_dir_all(&b).unwrap();
    fs::create_dir_all(&c).unwrap();
    for dir in [&b, &c] {
        fs::write(dir.join("mytool"), "#!/bin/sh\n").unwrap();
        make_executable(&dir.join("mytool"), 0o755);
    }
    let path_var = format!("{}: {} :{}", a.display(), b.display(), c.display());
    assert_eq!(find_on_path("mytool", Some(&path_var)), Some(b.join("mytool")));
}

#[cfg(unix)]
#[test]
fn find_on_path_ignores_relative_entries() {
    let t = TempRoot::new("path");
    let bin = t.path().join("mytool");
    fs::write(&bin, "#!/bin/sh\necho ok\n").unwrap();
    make_executable(&bin, 0o755);
    assert_eq!(find_on_path("mytool", Some("./relative-dir:bin")), None);
}

#[test]
fn find_on_path_ignores_empty_entries() {
    assert_eq!(find_on_path("mytool", Some("::")), None);
    assert_eq!(find_on_path("mytool", Some("")), None);
}

#[cfg(unix)]
#[test]
fn find_on_path_ignores_non_executable_files() {
    let t = TempRoot::new("path");
    let bin = t.path().join("nonexec");
    fs::write(&bin, "text file").unwrap();
    make_executable(&bin, 0o644);
    assert_eq!(find_on_path("nonexec", Some(&t.path().to_string_lossy())), None);
}

#[test]
fn find_on_path_missing_binary() {
    let t = TempRoot::new("path");
    assert_eq!(find_on_path("missingtool", Some(&t.path().to_string_lossy())), None);
}

#[cfg(unix)]
#[test]
fn find_on_path_refuses_invalid_tool_names() {
    let t = TempRoot::new("path");
    for name in ["MyTool", "tool;rm", "1tool", "", "../mytool", "my-tool"] {
        let bin = t.path().join(if name.is_empty() || name.contains('/') {
            "placeholder"
        } else {
            name
        });
        fs::write(&bin, "#!/bin/sh\n").unwrap();
        make_executable(&bin, 0o755);
        assert_eq!(find_on_path(name, Some(&t.path().to_string_lossy())), None, "{name:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// Default fetcher
// ---------------------------------------------------------------------------------------------

#[test]
fn ureq_fetcher_refuses_non_https_urls_before_any_io() {
    let req = HttpRequest {
        url: "http://raw.githubusercontent.com/owner/repo/HEAD/cli.schema.latest.json".into(),
        headers: vec![],
        timeout: Duration::from_millis(10),
    };
    match UreqFetcher.fetch(&req) {
        Err(FetchError::Network(m)) => assert!(m.contains("refusing non-HTTPS URL"), "{m}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Top-level keys of a JSON object in the order they appear in its text (independent of how
/// serde_json orders a parsed map, i.e. of the `preserve_order` feature).
fn keys_in_file_order(text: &str) -> Vec<String> {
    let parsed: Value = serde_json::from_str(text).unwrap();
    let mut keys: Vec<String> = parsed.as_object().unwrap().keys().cloned().collect();
    keys.sort_by_key(|k| text.find(&format!("\"{k}\":")).expect("key in text"));
    keys
}
