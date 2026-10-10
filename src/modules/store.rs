//! The module store (lmts `modules/store.ts`): resolve, ensure (TTL + conditional GET),
//! install, list, upgrade and remove `@owner/repo` schema modules. Same on-disk layout and
//! `meta.json` as lmts, so both SDKs share one cache.

use super::http::{FetchError, Fetcher, HttpRequest, HttpResponse, UreqFetcher};
use super::paths::{cache_root, entry_dir, find_on_path, is_within, resolve, schema_file_name};
use super::spec::parse_spec;
use super::time::{epoch_ms, parse_iso_ms, to_iso_string};
use super::types::{
    CachedModule, Channel, EnsureOutcome, EnsureSource, InstallAction, InstallOutcome, MetaDocument, ModuleEntry,
    ModuleError, ModuleErrorCode as Code, ModuleSpec, RemoveOutcome, UpgradeOutcome,
};
use crate::publish::parse_published_schema;
use crate::types::{PublishedCliSchema, Record};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

const GITHUB_RAW_BASE: &str = "https://raw.githubusercontent.com";
const GITHUB_RAW_HOST: &str = "raw.githubusercontent.com";
/// Install cap (2 MiB, lmapi distribution.md section 6).
pub const MAX_SCHEMA_BYTES: usize = 2 * 1024 * 1024;
/// Refresh cap used by [`ensure`] (1 MiB, as lmts).
pub const ENSURE_MAX_BYTES: usize = 1024 * 1024;
/// Freshness TTL of [`ensure`] (6 hours).
pub const DEFAULT_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// A clock (tests inject fixed times).
pub type Clock<'a> = &'a dyn Fn() -> SystemTime;

/// A spec given as text or already parsed.
#[derive(Debug, Clone, Copy)]
pub enum SpecRef<'a> {
    Text(&'a str),
    Parsed(&'a ModuleSpec),
}

impl<'a> From<&'a str> for SpecRef<'a> {
    fn from(s: &'a str) -> Self {
        SpecRef::Text(s)
    }
}

impl<'a> From<&'a String> for SpecRef<'a> {
    fn from(s: &'a String) -> Self {
        SpecRef::Text(s)
    }
}

impl<'a> From<&'a ModuleSpec> for SpecRef<'a> {
    fn from(s: &'a ModuleSpec) -> Self {
        SpecRef::Parsed(s)
    }
}

fn normalize(spec: SpecRef<'_>) -> Result<ModuleSpec, ModuleError> {
    match spec {
        SpecRef::Text(s) => parse_spec(s),
        SpecRef::Parsed(s) => Ok(s.clone()),
    }
}

/// Options of [`resolve_cached`].
#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    /// Cache root (default [`cache_root`]).
    pub root: Option<PathBuf>,
}

/// Options of [`ensure`].
#[derive(Clone, Copy, Default)]
pub struct EnsureOptions<'a> {
    pub root: Option<&'a Path>,
    /// Freshness TTL (default 6 h).
    pub ttl: Option<Duration>,
    pub now: Option<Clock<'a>>,
    pub fetcher: Option<&'a dyn Fetcher>,
    /// Install a missing entry (default true).
    pub install: Option<bool>,
    /// Revalidate a stale entry (default true).
    pub update: Option<bool>,
    /// Network deadline (default 3 s).
    pub timeout: Option<Duration>,
    /// Body cap (default 1 MiB).
    pub max_bytes: Option<usize>,
}

/// Options of [`install`].
#[derive(Clone, Copy, Default)]
pub struct InstallOptions<'a> {
    pub root: Option<&'a Path>,
    pub fetcher: Option<&'a dyn Fetcher>,
    pub now: Option<Clock<'a>>,
    /// Network deadline (default 10 s).
    pub timeout: Option<Duration>,
    /// Body cap (default 2 MiB).
    pub max_bytes: Option<usize>,
    /// Refetch and rewrite even when a verified entry exists.
    pub force: bool,
}

/// Options of [`list`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ListOptions<'a> {
    pub root: Option<&'a Path>,
    /// `PATH` searched for each entry's tool binary (default: the process `PATH`).
    pub path_var: Option<&'a str>,
}

/// Options of [`upgrade`].
#[derive(Clone, Copy, Default)]
pub struct UpgradeOptions<'a> {
    pub root: Option<&'a Path>,
    pub fetcher: Option<&'a dyn Fetcher>,
    pub now: Option<Clock<'a>>,
}

/// Options of [`remove`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RemoveOptions<'a> {
    pub root: Option<&'a Path>,
    /// Remove every channel of the repository.
    pub all_channels: bool,
}

fn root_or_default(root: Option<&Path>) -> PathBuf {
    root.map(Path::to_path_buf).unwrap_or_else(cache_root)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn url_for(spec: &ModuleSpec) -> String {
    format!(
        "{GITHUB_RAW_BASE}/{}/{}/HEAD/{}",
        spec.owner,
        spec.repo,
        schema_file_name(spec.channel)
    )
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(String::from)
}

/// Load a cache entry, verifying its SHA-256 and the document. `NOT_INSTALLED` when
/// missing, `CACHE_CORRUPT` on unreadable files or a hash mismatch, `BAD_JSON` /
/// `INVALID_SCHEMA` when the (hash-correct) document does not parse or validate.
pub fn resolve_cached<'a>(spec: impl Into<SpecRef<'a>>, options: &ResolveOptions) -> Result<CachedModule, ModuleError> {
    let spec = normalize(spec.into())?;
    let root = root_or_default(options.root.as_deref());
    resolve_in(&spec, &root)
}

fn resolve_in(spec: &ModuleSpec, root: &Path) -> Result<CachedModule, ModuleError> {
    let dir = entry_dir(root, spec)?;
    let schema_path = dir.join(schema_file_name(spec.channel));
    let meta_path = dir.join("meta.json");
    if !schema_path.exists() || !meta_path.exists() {
        return Err(ModuleError::new(
            Code::NotInstalled,
            format!("Module {} is not installed", spec.key),
        ));
    }
    let force_hint = format!("run: lmbee module install {} --force", spec.key);
    let bytes = fs::read(&schema_path).map_err(|e| {
        ModuleError::new(
            Code::CacheCorrupt,
            format!("Failed to read {}: {e}; {force_hint}", schema_path.display()),
        )
        .file(&schema_path)
    })?;
    let meta: Value = fs::read_to_string(&meta_path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        .map_err(|e| {
            ModuleError::new(
                Code::CacheCorrupt,
                format!("Failed to read or parse {}: {e}; {force_hint}", meta_path.display()),
            )
            .file(&meta_path)
        })?;
    let computed = sha256_hex(&bytes);
    let recorded = meta.get("sha256").and_then(Value::as_str);
    if recorded != Some(computed.as_str()) {
        let shown = recorded.map(String::from).unwrap_or_else(|| "undefined".into());
        return Err(ModuleError::new(
            Code::CacheCorrupt,
            format!(
                "Cached schema hash mismatch in {}: expected {shown}, got {computed}; {force_hint}",
                schema_path.display()
            ),
        )
        .file(&schema_path));
    }
    let json: Value = serde_json::from_str(&String::from_utf8_lossy(&bytes)).map_err(|e| {
        ModuleError::new(
            Code::BadJson,
            format!("Failed to parse {} as JSON: {e}", schema_path.display()),
        )
        .file(&schema_path)
    })?;
    let doc = parse_published_schema(&json).map_err(|issues| {
        let mut err = ModuleError::new(
            Code::InvalidSchema,
            format!(
                "Cached schema in {} is invalid: {}",
                schema_path.display(),
                issues[0].message
            ),
        )
        .file(&schema_path);
        err.issues = Some(issues);
        err
    })?;
    let fetched_at = str_field(&meta, "fetched_at").unwrap_or_default();
    Ok(CachedModule {
        spec: spec.clone(),
        dir,
        path: schema_path,
        sha256: computed,
        last_checked: str_field(&meta, "last_checked").unwrap_or_else(|| fetched_at.clone()),
        fetched_at,
        etag: str_field(&meta, "etag"),
        channel: spec.channel,
        tool: doc.tool.clone(),
        version: doc.version.clone(),
        document: doc,
    })
}

/// Resolve a `Location` header against the request URL.
fn resolve_location(location: &str, base: &str) -> String {
    // WHATWG URL parsing (what lmts' `new URL()` does) for special schemes: leading and
    // trailing C0 controls and spaces are trimmed, tabs/newlines removed, `\` means `/`.
    let cleaned: String = location
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .map(|c| if c == '\\' { '/' } else { c })
        .collect();
    let mut location = cleaned.as_str();
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    if let Some((s, after)) = location.split_once(':') {
        let is_scheme = s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if is_scheme {
            if after.starts_with("//") || !s.eq_ignore_ascii_case(scheme) {
                // Absolute; `http:host/x` (another special scheme without slashes) still names a host.
                return format!("{}://{}", s.to_ascii_lowercase(), after.trim_start_matches('/'));
            }
            // `https:x` with the base's own scheme is relative to the base.
            location = after;
        }
    }
    if let Some(net) = location.strip_prefix("//") {
        return format!("{scheme}://{net}");
    }
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if location.starts_with('/') {
        return format!("{scheme}://{authority}{location}");
    }
    let path = &rest[authority_end..];
    let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(0)];
    format!(
        "{scheme}://{authority}{}{location}",
        if dir.is_empty() { "/" } else { dir }
    )
}

/// `(scheme, lowercase hostname)` of an absolute URL.
fn scheme_and_host(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let host_port = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let host = if host_port.starts_with('[') {
        &host_port[..host_port.find(']').map(|i| i + 1).unwrap_or(host_port.len())]
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    Some((scheme.to_ascii_lowercase(), host.to_ascii_lowercase()))
}

/// One GET with manual redirect handling: a single redirect is followed only when it
/// stays on `https://raw.githubusercontent.com` (304 is not a redirect).
fn fetch_with_redirects(
    url: &str,
    fetcher: &dyn Fetcher,
    timeout: Duration,
    etag: Option<&str>,
) -> Result<HttpResponse, ModuleError> {
    let headers: Vec<(String, String)> = etag
        .map(|e| vec![("If-None-Match".to_string(), e.to_string())])
        .unwrap_or_default();
    let started = std::time::Instant::now();
    let call = |target: &str, timeout: Duration| {
        fetcher
            .fetch(&HttpRequest {
                url: target.to_string(),
                headers: headers.clone(),
                timeout,
            })
            .map_err(|e| match e {
                FetchError::Timeout(_) => ModuleError::new(
                    Code::FetchFailed,
                    format!("Fetch timed out after {}ms for {url}", timeout.as_millis()),
                ),
                FetchError::Network(m) => {
                    ModuleError::new(Code::FetchFailed, format!("Network error fetching {url}: {m}"))
                }
            })
    };
    let response = call(url, timeout)?;
    if (300..400).contains(&response.status) && response.status != 304 {
        let Some(location) = response.header("location").map(String::from) else {
            return Err(ModuleError::new(
                Code::FetchFailed,
                format!("Redirect response from {url} missing Location header"),
            ));
        };
        let target = resolve_location(&location, url);
        match scheme_and_host(&target) {
            Some((scheme, host)) if scheme == "https" && host == GITHUB_RAW_HOST => {}
            _ => {
                return Err(ModuleError::new(
                    Code::FetchFailed,
                    format!("Cross-host or non-HTTPS redirect to \"{location}\" refused (must stay on raw.githubusercontent.com)"),
                ))
            }
        }
        // One deadline covers both requests, like lmts's single AbortSignal.
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(ModuleError::new(
                Code::FetchFailed,
                format!("Fetch timed out after {}ms for {url}", timeout.as_millis()),
            ));
        }
        return call(&target, remaining);
    }
    Ok(response)
}

struct Payload {
    bytes: Vec<u8>,
    sha256: String,
    schema: PublishedCliSchema,
}

/// Read at most `max_bytes` of the body, parse it and validate it as `cli.schema.v1`.
fn read_payload(
    response: HttpResponse,
    max_bytes: usize,
    timeout: Duration,
    spec: &ModuleSpec,
    url: &str,
) -> Result<Payload, ModuleError> {
    let mut bytes = Vec::new();
    let mut limited = response.body.take(max_bytes as u64 + 1);
    if let Err(e) = limited.read_to_end(&mut bytes) {
        let timed_out = matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock);
        return Err(ModuleError::new(
            Code::FetchFailed,
            if timed_out {
                format!("Fetch stream timed out after {}ms", timeout.as_millis())
            } else {
                format!("Error reading response stream: {e}")
            },
        ));
    }
    if bytes.len() > max_bytes {
        return Err(ModuleError::new(
            Code::OutputLimit,
            format!("Schema exceeds maximum size limit of {max_bytes} bytes"),
        ));
    }
    let file = schema_file_name(spec.channel);
    let json: Value = serde_json::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|e| ModuleError::new(Code::BadJson, format!("Failed to parse {file} from {url} as JSON: {e}")))?;
    let schema = parse_published_schema(&json).map_err(|issues| {
        let mut err = ModuleError::new(
            Code::InvalidSchema,
            format!("Invalid schema for {}: {}", spec.key, issues[0].message),
        );
        err.issues = Some(issues);
        err
    })?;
    let sha256 = sha256_hex(&bytes);
    Ok(Payload { bytes, sha256, schema })
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_name(dir: &Path, kind: &str) -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!(
        ".tmp.{kind}.{}.{}-{n}-{nanos}",
        epoch_ms(SystemTime::now()),
        std::process::id()
    ))
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

fn chmod_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn meta_text(meta: &MetaDocument) -> String {
    format!("{}\n", crate::js::stringify_pretty(meta))
}

/// Write a temp file then rename it over `target` (0600).
fn replace_file(dir: &Path, kind: &str, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = temp_name(dir, kind);
    let res = write_private(&tmp, bytes).and_then(|_| fs::rename(&tmp, target));
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    } else {
        chmod_private(target);
    }
    res
}

/// Write the schema file and `meta.json` through temp files and renames (0700 / 0600).
fn atomic_write_entry(dir: &Path, file_name: &str, bytes: &[u8], meta: &MetaDocument) -> Result<(), ModuleError> {
    let res = create_private_dir(dir)
        .and_then(|_| replace_file(dir, "schema", &dir.join(file_name), bytes))
        .and_then(|_| replace_file(dir, "meta", &dir.join("meta.json"), meta_text(meta).as_bytes()));
    res.map_err(|e| {
        ModuleError::new(
            Code::AtomicWriteFailed,
            format!("Failed to write cached module files: {e}"),
        )
    })
}

fn atomic_write_meta(dir: &Path, meta: &MetaDocument) -> Result<(), ModuleError> {
    replace_file(dir, "meta", &dir.join("meta.json"), meta_text(meta).as_bytes())
        .map_err(|e| ModuleError::new(Code::AtomicWriteFailed, format!("Failed to write meta.json: {e}")))
}

fn now_of(now: Option<Clock<'_>>) -> SystemTime {
    now.map(|f| f()).unwrap_or_else(SystemTime::now)
}

/// Make sure an evergreen module is cached and fresh enough.
///
/// - Younger than the TTL: the cache, no network (`Cache`).
/// - Stale and `update`: one conditional GET (`If-None-Match`). 304 bumps `last_checked`,
///   200 validates and replaces the entry (`Network`); any failure keeps the existing entry
///   (`StaleCache` with a warning) and leaves the files untouched.
/// - Missing and `install`: download and cache (`Installed`); otherwise `NOT_INSTALLED`.
/// - Corrupt: `CACHE_CORRUPT`, never refetched silently.
pub fn ensure<'a>(spec: impl Into<SpecRef<'a>>, options: &EnsureOptions<'_>) -> Result<EnsureOutcome, ModuleError> {
    let spec = normalize(spec.into())?;
    let root = root_or_default(options.root);
    let ttl = options.ttl.unwrap_or(DEFAULT_TTL);
    let default_fetcher = UreqFetcher;
    let fetcher: &dyn Fetcher = options.fetcher.unwrap_or(&default_fetcher);
    let timeout = options.timeout.unwrap_or(Duration::from_millis(3000));
    let max_bytes = options.max_bytes.unwrap_or(ENSURE_MAX_BYTES);

    let current = match resolve_in(&spec, &root) {
        Ok(m) => m,
        Err(e) if e.code == Code::NotInstalled => {
            if !options.install.unwrap_or(true) {
                return Err(ModuleError::new(
                    Code::NotInstalled,
                    format!(
                        "Module {} is not installed; run: lmbee module install {}",
                        spec.key, spec.key
                    ),
                ));
            }
            let installed = install(
                &spec,
                &InstallOptions {
                    root: Some(&root),
                    fetcher: Some(fetcher),
                    now: options.now,
                    timeout: Some(timeout),
                    max_bytes: Some(max_bytes),
                    force: false,
                },
            )?;
            return Ok(EnsureOutcome {
                module: installed.entry,
                source: EnsureSource::Installed,
                warning: None,
            });
        }
        Err(e) => return Err(e),
    };

    let now_ms = epoch_ms(now_of(options.now));
    let fresh = match parse_iso_ms(&current.last_checked) {
        Some(checked) => checked <= now_ms && ((now_ms - checked) as u128) < ttl.as_millis(),
        None => false,
    };
    if fresh || !options.update.unwrap_or(true) {
        return Ok(EnsureOutcome {
            module: current,
            source: EnsureSource::Cache,
            warning: None,
        });
    }

    let stale = |module: CachedModule, warning: String| {
        Ok(EnsureOutcome {
            module,
            source: EnsureSource::StaleCache,
            warning: Some(warning),
        })
    };
    let url = url_for(&spec);
    let response = match fetch_with_redirects(&url, fetcher, timeout, current.etag.as_deref()) {
        Ok(r) => r,
        Err(e) => return stale(current, e.message),
    };

    if response.status == 304 {
        let last_checked = to_iso_string(now_of(options.now));
        let etag = response
            .header("etag")
            .map(String::from)
            .or_else(|| current.etag.clone());
        let bytes = fs::metadata(&current.path).map(|m| m.len()).unwrap_or(0);
        let meta = MetaDocument {
            spec: spec.key.clone(),
            url,
            sha256: current.sha256.clone(),
            fetched_at: (!current.fetched_at.is_empty()).then(|| current.fetched_at.clone()),
            last_checked: last_checked.clone(),
            etag: etag.clone(),
            channel: spec.channel,
            tool: current.tool.clone(),
            version: current.version.clone(),
            bytes,
        };
        if let Err(e) = atomic_write_meta(&current.dir, &meta) {
            return stale(current, e.message);
        }
        let module = CachedModule {
            last_checked,
            etag,
            ..current
        };
        return Ok(EnsureOutcome {
            module,
            source: EnsureSource::Network,
            warning: None,
        });
    }

    if response.status != 200 {
        let warning = format!(
            "Failed to refresh {}: HTTP {} {}",
            spec.key, response.status, response.status_text
        );
        return stale(current, warning);
    }

    let etag = response.header("etag").map(String::from);
    let payload = match read_payload(response, max_bytes, timeout, &spec, &url) {
        Ok(p) => p,
        Err(e) => return stale(current, e.message),
    };
    let fetched_at = to_iso_string(now_of(options.now));
    let meta = MetaDocument {
        spec: spec.key.clone(),
        url,
        sha256: payload.sha256.clone(),
        fetched_at: Some(fetched_at.clone()),
        last_checked: fetched_at.clone(),
        etag: etag.clone(),
        channel: spec.channel,
        tool: payload.schema.tool.clone(),
        version: Some(payload.schema.version.clone().unwrap_or_else(|| "unknown".into())),
        bytes: payload.bytes.len() as u64,
    };
    if let Err(e) = atomic_write_entry(&current.dir, &schema_file_name(spec.channel), &payload.bytes, &meta) {
        return stale(current, e.message);
    }
    let module = CachedModule {
        spec: spec.clone(),
        dir: current.dir.clone(),
        path: current.path.clone(),
        sha256: payload.sha256,
        fetched_at: fetched_at.clone(),
        last_checked: fetched_at,
        etag,
        channel: spec.channel,
        tool: payload.schema.tool.clone(),
        version: payload.schema.version.clone(),
        document: payload.schema,
    };
    Ok(EnsureOutcome {
        module,
        source: EnsureSource::Network,
        warning: None,
    })
}

/// Download a module schema into the cache (streamed with a size cap, validated, written
/// atomically). An existing verified entry is `Unchanged` unless `force`.
pub fn install<'a>(spec: impl Into<SpecRef<'a>>, options: &InstallOptions<'_>) -> Result<InstallOutcome, ModuleError> {
    let spec = normalize(spec.into())?;
    let root = root_or_default(options.root);
    let default_fetcher = UreqFetcher;
    let fetcher: &dyn Fetcher = options.fetcher.unwrap_or(&default_fetcher);
    let timeout = options.timeout.unwrap_or(Duration::from_millis(10_000));
    let max_bytes = options.max_bytes.unwrap_or(MAX_SCHEMA_BYTES);

    let dir = entry_dir(&root, &spec)?;
    let file_name = schema_file_name(spec.channel);
    let url = url_for(&spec);

    let prior = resolve_in(&spec, &root);
    if let (false, Ok(entry)) = (options.force, &prior) {
        return Ok(InstallOutcome {
            action: InstallAction::Unchanged,
            entry: entry.clone(),
        });
    }

    let response = fetch_with_redirects(&url, fetcher, timeout, None)?;
    if response.status == 404 {
        let mut details = Record::new();
        details.insert("status".into(), Value::from(404));
        details.insert("url".into(), Value::String(url.clone()));
        details.insert("file".into(), Value::String(file_name.clone()));
        let mut err = ModuleError::new(
            Code::FetchFailed,
            format!(
                "Module {} not found: expected {file_name} at the root of the default branch for {}/{}",
                spec.key, spec.owner, spec.repo
            ),
        );
        err.details = Some(Box::new(details));
        return Err(err);
    }
    if !response.ok() {
        let mut details = Record::new();
        details.insert("status".into(), Value::from(response.status));
        details.insert("url".into(), Value::String(url.clone()));
        let mut err = ModuleError::new(
            Code::FetchFailed,
            format!(
                "Failed to fetch {url}: HTTP {} {}",
                response.status, response.status_text
            ),
        );
        err.details = Some(Box::new(details));
        return Err(err);
    }

    let etag = response.header("etag").map(String::from);
    let payload = read_payload(response, max_bytes, timeout, &spec, &url)?;
    if let (false, Ok(entry)) = (options.force, &prior) {
        if entry.sha256 == payload.sha256 {
            return Ok(InstallOutcome {
                action: InstallAction::Unchanged,
                entry: entry.clone(),
            });
        }
    }
    let fetched_at = to_iso_string(now_of(options.now));
    let meta = MetaDocument {
        spec: spec.key.clone(),
        url,
        sha256: payload.sha256.clone(),
        fetched_at: Some(fetched_at.clone()),
        last_checked: fetched_at.clone(),
        etag: etag.clone(),
        channel: spec.channel,
        tool: payload.schema.tool.clone(),
        version: Some(payload.schema.version.clone().unwrap_or_else(|| "unknown".into())),
        bytes: payload.bytes.len() as u64,
    };
    atomic_write_entry(&dir, &file_name, &payload.bytes, &meta)?;
    let entry = CachedModule {
        spec: spec.clone(),
        path: dir.join(&file_name),
        dir,
        sha256: payload.sha256,
        fetched_at: fetched_at.clone(),
        last_checked: fetched_at,
        etag,
        channel: spec.channel,
        tool: payload.schema.tool.clone(),
        version: payload.schema.version.clone(),
        document: payload.schema,
    };
    let action = if prior.is_ok() {
        InstallAction::Replaced
    } else {
        InstallAction::Installed
    };
    Ok(InstallOutcome { action, entry })
}

fn sorted_dirs(dir: &Path) -> Option<Vec<(String, PathBuf)>> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(_, p)| fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false))
        .collect();
    out.sort();
    Some(out)
}

/// Every entry in the cache (sorted by owner, repo, channel); corrupt entries are listed
/// with a `problem` instead of being skipped.
pub fn list(options: &ListOptions<'_>) -> Vec<ModuleEntry> {
    let root = root_or_default(options.root);
    let gh = root.join("github.com");
    let mut entries = Vec::new();
    for (owner, owner_dir) in sorted_dirs(&gh).unwrap_or_default() {
        for (repo, repo_dir) in sorted_dirs(&owner_dir).unwrap_or_default() {
            for (channel_name, dir) in sorted_dirs(&repo_dir).unwrap_or_default() {
                let key = format!("@{owner}/{repo}#{channel_name}");
                let channel = Channel::parse(&channel_name);
                let file_name = format!("cli.schema.{channel_name}.json");
                let schema_file = dir.join(&file_name);
                let meta_file = dir.join("meta.json");
                let problem = |problem: String| ModuleEntry {
                    spec: key.clone(),
                    channel,
                    tool: None,
                    version: None,
                    sha256: None,
                    fetched_at: None,
                    dir: dir.clone(),
                    binary_on_path: None,
                    problem: Some(problem),
                };
                if !schema_file.exists() || !meta_file.exists() {
                    entries.push(problem(format!("Missing {file_name} or meta.json")));
                    continue;
                }
                let loaded = fs::read(&schema_file).map_err(|e| e.to_string()).and_then(|bytes| {
                    let meta = fs::read_to_string(&meta_file).map_err(|e| e.to_string())?;
                    let meta: Value = serde_json::from_str(&meta).map_err(|e| e.to_string())?;
                    Ok((sha256_hex(&bytes), meta))
                });
                let (sha, meta) = match loaded {
                    Ok(v) => v,
                    Err(e) => {
                        entries.push(problem(format!("Unreadable or corrupt files: {e}")));
                        continue;
                    }
                };
                let recorded = meta.get("sha256").and_then(Value::as_str);
                if recorded != Some(sha.as_str()) {
                    let shown = recorded.map(String::from).unwrap_or_else(|| "undefined".into());
                    entries.push(problem(format!("Checksum mismatch: expected {shown}, got {sha}")));
                    continue;
                }
                let tool = str_field(&meta, "tool").filter(|t| !t.is_empty());
                entries.push(ModuleEntry {
                    spec: str_field(&meta, "spec").unwrap_or(key.clone()),
                    channel: str_field(&meta, "channel")
                        .as_deref()
                        .and_then(Channel::parse)
                        .or(channel),
                    binary_on_path: Some(tool.as_deref().and_then(|t| find_on_path(t, options.path_var))),
                    tool,
                    version: str_field(&meta, "version"),
                    sha256: recorded.map(String::from),
                    fetched_at: str_field(&meta, "fetched_at"),
                    dir: dir.clone(),
                    problem: None,
                });
            }
        }
    }
    entries
}

/// Refetch installed modules now (both channels move): the given specs, or every
/// installed entry. A spec that does not parse ends the run with a single failure (as lmts).
pub fn upgrade(specs: Option<&[SpecRef<'_>]>, options: &UpgradeOptions<'_>) -> Vec<UpgradeOutcome> {
    let root = root_or_default(options.root);
    let mut targets: Vec<ModuleSpec> = Vec::new();
    match specs {
        Some(list) => {
            for item in list {
                match normalize(*item) {
                    Ok(s) => targets.push(s),
                    Err(e) => {
                        let spec = match item {
                            SpecRef::Text(t) => t.to_string(),
                            SpecRef::Parsed(p) => p.key.clone(),
                        };
                        return vec![UpgradeOutcome::Failed {
                            spec,
                            channel: None,
                            error: e.message,
                        }];
                    }
                }
            }
        }
        None => {
            for entry in list(&ListOptions {
                root: Some(&root),
                path_var: Some(""),
            }) {
                if entry.channel.is_some() {
                    if let Ok(s) = parse_spec(&entry.spec) {
                        targets.push(s);
                    }
                }
            }
        }
    }

    let mut outcomes = Vec::new();
    for spec in targets {
        let current = match resolve_in(&spec, &root) {
            Ok(c) => c,
            Err(e) => {
                outcomes.push(UpgradeOutcome::Failed {
                    spec: spec.key.clone(),
                    channel: Some(spec.channel),
                    error: e.message,
                });
                continue;
            }
        };
        let res = install(
            &spec,
            &InstallOptions {
                root: Some(&root),
                fetcher: options.fetcher,
                now: options.now,
                force: true,
                ..InstallOptions::default()
            },
        );
        let fresh = match res {
            Ok(r) => r.entry,
            Err(e) => {
                outcomes.push(UpgradeOutcome::Failed {
                    spec: spec.key.clone(),
                    channel: Some(spec.channel),
                    error: e.message,
                });
                continue;
            }
        };
        if fresh.sha256 == current.sha256 {
            outcomes.push(UpgradeOutcome::Unchanged {
                spec: spec.key.clone(),
                channel: spec.channel,
                sha256: current.sha256,
                version: current.version,
            });
        } else {
            let old: Vec<&str> = current.document.operations.iter().map(|o| o.name.as_str()).collect();
            let new: Vec<&str> = fresh.document.operations.iter().map(|o| o.name.as_str()).collect();
            let mut added: Vec<String> = new.iter().filter(|n| !old.contains(n)).map(|s| s.to_string()).collect();
            let mut removed: Vec<String> = old.iter().filter(|n| !new.contains(n)).map(|s| s.to_string()).collect();
            added.sort();
            added.dedup();
            removed.sort();
            removed.dedup();
            outcomes.push(UpgradeOutcome::Updated {
                spec: spec.key.clone(),
                channel: spec.channel,
                from_sha: current.sha256,
                to_sha: fresh.sha256,
                from_version: current.version,
                to_version: fresh.version,
                operations_added: added,
                operations_removed: removed,
            });
        }
    }
    outcomes
}

/// Remove one entry (or every channel of the repository) and prune empty parents.
/// Never deletes outside the cache root.
pub fn remove<'a>(spec: impl Into<SpecRef<'a>>, options: &RemoveOptions<'_>) -> Result<RemoveOutcome, ModuleError> {
    let spec = normalize(spec.into())?;
    let root = root_or_default(options.root);
    let abs_root = resolve(&root);
    super::paths::check_segments(&spec)?;
    let target = if options.all_channels {
        let gh = abs_root.join("github.com");
        let repo_dir = resolve(&gh.join(&spec.owner).join(&spec.repo));
        if !is_within(&abs_root, &repo_dir) || !is_within(&gh, &repo_dir) {
            return Err(ModuleError::new(Code::PathTraversal, "Target path escapes cache root"));
        }
        if !repo_dir.exists() {
            return Err(ModuleError::new(
                Code::NotInstalled,
                format!("Module {}/{} not found in cache", spec.owner, spec.repo),
            ));
        }
        repo_dir
    } else {
        let dir = entry_dir(&root, &spec)?;
        if !dir.exists() {
            return Err(ModuleError::new(
                Code::NotInstalled,
                format!("Module {} is not installed", spec.key),
            ));
        }
        dir
    };
    let _ = fs::remove_dir_all(&target);
    prune_parents(&target, &abs_root);
    Ok(RemoveOutcome { removed: vec![target] })
}

fn prune_parents(deleted: &Path, root: &Path) {
    let mut current = deleted.parent().map(Path::to_path_buf);
    while let Some(dir) = current {
        if dir == root || !dir.starts_with(root) {
            break;
        }
        let empty = fs::read_dir(&dir).map(|mut rd| rd.next().is_none()).unwrap_or(false);
        if !empty || fs::remove_dir(&dir).is_err() {
            break;
        }
        current = dir.parent().map(Path::to_path_buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_resolve_like_url() {
        let base = "https://raw.githubusercontent.com/o/r/HEAD/cli.schema.latest.json";
        assert_eq!(resolve_location("https://x.example/a", base), "https://x.example/a");
        assert_eq!(resolve_location("//evil.example/a", base), "https://evil.example/a");
        assert_eq!(
            resolve_location("/o/r/main/x.json", base),
            "https://raw.githubusercontent.com/o/r/main/x.json"
        );
        assert_eq!(
            resolve_location("other.json", base),
            "https://raw.githubusercontent.com/o/r/HEAD/other.json"
        );
        assert_eq!(resolve_location("x", "https://h"), "https://h/x");
    }

    #[test]
    fn hosts_are_parsed() {
        assert_eq!(
            scheme_and_host("HTTPS://Raw.GitHubUserContent.com:443/x"),
            Some(("https".into(), "raw.githubusercontent.com".into()))
        );
        assert_eq!(
            scheme_and_host("https://u:p@raw.githubusercontent.com/x").unwrap().1,
            "raw.githubusercontent.com"
        );
        assert_eq!(scheme_and_host("https://[::1]:8080/x").unwrap().1, "[::1]");
        assert_eq!(scheme_and_host("no-scheme"), None);
    }
}
