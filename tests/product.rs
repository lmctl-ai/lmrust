//! Port of lmts `tests/product.test.ts`: product metadata validation in
//! `parsePublishedSchema`, `checkProductVersion` against fake executables, caching, and
//! runner gating (exit 10).
//!
//! lmts prepends a temp dir to `process.env.PATH`; here every check receives an explicit env
//! map (`CheckProductVersionOptions.env` / `run_cli_io`'s env). The detection cache is
//! process-global, so every test in this binary takes `LOCK` (the cache test resets it) and
//! uses product names of its own.

use lmrust::server::{
    check_product_version, find_executable_on_path, reset_product_version_cache, run_cli_io,
    CheckProductVersionOptions, CliSpec, ErrorCode, ExecResult, OpDef, ProductCheckFailure,
};
use lmrust::{
    parse_published_schema, publish_schema, Access, IssueCode, OperationSchema, ProductDetect, ProductInfo,
    PublishOptions,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

static LOCK: Mutex<()> = Mutex::new(());

/// (resolved binary, args) seen by an injected detector.
type ExecCall = (PathBuf, Vec<String>);

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A unique temp bin dir holding fake executables (removed on drop).
struct BinDir(PathBuf);

impl BinDir {
    fn new(prefix: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "{prefix}{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        BinDir(dir)
    }

    fn write_file(&self, name: &str, content: &str, mode: u32) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
        path
    }

    /// `#!/bin/sh` script, mode 0755 (lmts `createFakeExecutable`).
    fn fake_executable(&self, name: &str, script: &str) -> PathBuf {
        self.write_file(name, &format!("#!/bin/sh\n{script}\n"), 0o755)
    }

    fn path_str(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }

    /// Env whose PATH is this dir followed by the system dirs (for `sleep` etc.).
    fn env(&self) -> HashMap<String, String> {
        [("PATH".to_string(), format!("{}:/usr/bin:/bin", self.path_str()))]
            .into_iter()
            .collect()
    }
}

impl Drop for BinDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn product(name: &str, supported: &str, argv: &[&str], pattern: &str) -> ProductInfo {
    ProductInfo {
        name: name.into(),
        supported: supported.into(),
        detect: Some(ProductDetect {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            pattern: pattern.into(),
        }),
    }
}

fn check_with_env(p: &ProductInfo, env: HashMap<String, String>) -> Result<String, ProductCheckFailure> {
    check_product_version(
        p,
        &CheckProductVersionOptions {
            env: Some(env),
            ..CheckProductVersionOptions::default()
        },
    )
}

fn data(f: &ProductCheckFailure) -> Value {
    Value::Object(f.data())
}

// ---------------------------------------------------------------------------
// describe('product schema validation in parsePublishedSchema')
// ---------------------------------------------------------------------------

fn base_doc_with(product: Option<Value>) -> Value {
    let mut doc = json!({
        "schema": "cli.schema.v1",
        "tool": "git2",
        "version": "1.0.0",
        "policy": { "dryrun": true, "readonly": true },
        "inputs": ["flags", "json"],
        "operations": [{ "name": "status", "params": [] }],
    });
    if let Some(p) = product {
        doc["product"] = p;
    }
    doc
}

fn issue_codes(doc: &Value) -> Option<Vec<IssueCode>> {
    match parse_published_schema(doc) {
        Ok(_) => None,
        Err(issues) => Some(issues.into_iter().map(|i| i.code).collect()),
    }
}

#[test]
fn schema_accepts_valid_product_with_detect_block() {
    let doc = base_doc_with(Some(json!({
        "name": "git",
        "supported": ">=2.43.0 <3.0.0",
        "detect": { "argv": ["git", "--version"], "pattern": "(\\d+\\.\\d+\\.\\d+)" },
    })));
    let parsed = parse_published_schema(&doc).expect("valid");
    assert_eq!(
        parsed.product,
        Some(product(
            "git",
            ">=2.43.0 <3.0.0",
            &["git", "--version"],
            "(\\d+\\.\\d+\\.\\d+)"
        ))
    );
}

#[test]
fn schema_accepts_valid_product_without_detect_block() {
    let doc = base_doc_with(Some(json!({ "name": "git", "supported": ">=2.43.0" })));
    assert!(parse_published_schema(&doc).is_ok());
}

#[test]
fn schema_rejects_non_object_product() {
    let codes = issue_codes(&base_doc_with(Some(json!("not-an-object")))).expect("rejected");
    assert!(codes.contains(&IssueCode::TypeMismatch), "{codes:?}");
}

#[test]
fn schema_rejects_unknown_property_in_product() {
    let doc = base_doc_with(Some(
        json!({ "name": "git", "supported": ">=2.43.0", "extra": "not allowed" }),
    ));
    let codes = issue_codes(&doc).expect("rejected");
    assert!(codes.contains(&IssueCode::UnknownParam), "{codes:?}");
}

#[test]
fn schema_rejects_invalid_product_name_with_uppercase_or_symbols() {
    let doc = base_doc_with(Some(json!({ "name": "Git_Tool", "supported": ">=2.43.0" })));
    let codes = issue_codes(&doc).expect("rejected");
    assert!(codes.contains(&IssueCode::DocumentParse), "{codes:?}");
}

#[test]
fn schema_rejects_product_name_starting_with_non_letter() {
    let doc = base_doc_with(Some(json!({ "name": "1git", "supported": ">=2.43.0" })));
    assert!(issue_codes(&doc).is_some());
}

#[test]
fn schema_rejects_invalid_supported_range() {
    let doc = base_doc_with(Some(json!({ "name": "git", "supported": "bad-range" })));
    let codes = issue_codes(&doc).expect("rejected");
    assert!(codes.contains(&IssueCode::DocumentParse), "{codes:?}");
}

#[test]
fn schema_rejects_detect_with_non_object() {
    let doc = base_doc_with(Some(
        json!({ "name": "git", "supported": ">=2.43.0", "detect": "not-an-object" }),
    ));
    assert!(issue_codes(&doc).is_some());
}

#[test]
fn schema_rejects_detect_with_empty_argv_array() {
    let doc = base_doc_with(Some(json!({
        "name": "git", "supported": ">=2.43.0",
        "detect": { "argv": [], "pattern": "(\\d+)" },
    })));
    let codes = issue_codes(&doc).expect("rejected");
    assert!(codes.contains(&IssueCode::DocumentParse), "{codes:?}");
}

#[test]
fn schema_rejects_detect_argv0_with_slashes() {
    let doc = base_doc_with(Some(json!({
        "name": "git", "supported": ">=2.43.0",
        "detect": { "argv": ["/usr/bin/git", "--version"], "pattern": "(\\d+)" },
    })));
    let codes = issue_codes(&doc).expect("rejected");
    assert!(codes.contains(&IssueCode::DocumentParse), "{codes:?}");
}

#[test]
fn schema_rejects_detect_pattern_with_invalid_regex() {
    let doc = base_doc_with(Some(json!({
        "name": "git", "supported": ">=2.43.0",
        "detect": { "argv": ["git", "--version"], "pattern": "[invalid-regex" },
    })));
    let codes = issue_codes(&doc).expect("rejected");
    assert!(codes.contains(&IssueCode::DocumentParse), "{codes:?}");
}

#[test]
fn schema_rejects_detect_pattern_with_two_capture_groups() {
    let doc = base_doc_with(Some(json!({
        "name": "git", "supported": ">=2.43.0",
        "detect": { "argv": ["git", "--version"], "pattern": "(\\d+)\\.(\\d+)" },
    })));
    let codes = issue_codes(&doc).expect("rejected");
    assert!(codes.contains(&IssueCode::DocumentParse), "{codes:?}");
}

#[test]
fn publish_schema_includes_product_option_when_provided() {
    let prod = ProductInfo {
        name: "git".into(),
        supported: ">=2.43.0".into(),
        detect: None,
    };
    let published = publish_schema(
        "git2",
        vec![OperationSchema::new("status")],
        PublishOptions {
            product: Some(prod.clone()),
            ..PublishOptions::default()
        },
    );
    assert_eq!(published.product, Some(prod));
    // serialized without a `detect` key
    assert_eq!(
        serde_json::to_value(&published).unwrap()["product"],
        json!({ "name": "git", "supported": ">=2.43.0" })
    );
}

// ---------------------------------------------------------------------------
// describe('checkProductVersion with realistic fake executables')
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn succeeds_for_realistic_git_output() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("git", "echo \"git version 2.43.0\"");
    let p = product(
        "git-realistic",
        ">=2.43.0 <3.0.0",
        &["git", "--version"],
        "git\\s+version\\s+(\\d+\\.\\d+\\.\\d+)",
    );
    assert_eq!(check_with_env(&p, bin.env()), Ok("2.43.0".to_string()));
}

#[cfg(unix)]
#[test]
fn succeeds_for_realistic_gh_output() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("gh", "echo \"gh version 2.45.0 (2025-07-18 Ubuntu 2.45.0-1ubuntu0.3)\"");
    let p = product(
        "gh-realistic",
        ">=2.40.0 <3.0.0",
        &["gh", "--version"],
        "gh\\s+version\\s+(\\d+\\.\\d+\\.\\d+)",
    );
    assert_eq!(check_with_env(&p, bin.env()), Ok("2.45.0".to_string()));
}

#[cfg(unix)]
#[test]
fn succeeds_for_realistic_aws_output() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("aws", "echo \"aws-cli/2.34.48 Python/3.14.4 Linux/6.6.137 x86_64\"");
    let p = product(
        "aws-realistic",
        ">=2.30.0 <3.0.0",
        &["aws", "--version"],
        "aws-cli\\/(\\d+\\.\\d+\\.\\d+)",
    );
    assert_eq!(check_with_env(&p, bin.env()), Ok("2.34.48".to_string()));
}

#[cfg(unix)]
#[test]
fn version_found_on_stderr_is_accepted() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("errtool", "echo \"errtool version 3.1.4\" >&2; exit 1");
    let p = product("errtool", ">=3.0.0", &["errtool"], "version\\s+(\\d+\\.\\d+\\.\\d+)");
    // stdout + "\n" + stderr is searched; non-zero exit is allowed
    assert_eq!(check_with_env(&p, bin.env()), Ok("3.1.4".to_string()));
}

#[cfg(unix)]
#[test]
fn fails_with_unsupported_version_when_version_is_too_low() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("git", "echo \"git version 1.9.5\"");
    let p = product(
        "git-too-low",
        ">=2.43.0 <3.0.0",
        &["git", "--version"],
        "git\\s+version\\s+(\\d+\\.\\d+\\.\\d+)",
    );
    let f = check_with_env(&p, bin.env()).unwrap_err();
    // a failure always maps to UNSUPPORTED_PRODUCT_VERSION (exit 10) in the runner
    assert_eq!(ErrorCode::UnsupportedProductVersion.exit_code(), 10);
    assert_eq!(f.product, "git-too-low");
    assert_eq!(f.found.as_deref(), Some("1.9.5"));
    assert_eq!(f.supported, ">=2.43.0 <3.0.0");
    assert_eq!(
        data(&f),
        json!({ "product": "git-too-low", "found": "1.9.5", "supported": ">=2.43.0 <3.0.0" })
    );
    assert_eq!(
        f.message,
        "Product \"git-too-low\" version 1.9.5 does not satisfy supported range \">=2.43.0 <3.0.0\""
    );
}

#[test]
fn fails_with_found_null_when_binary_is_missing_on_path() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    let p = product(
        "nonexistent-tool",
        ">=1.0.0",
        &["nonexistent-bin", "--version"],
        "(\\d+\\.\\d+\\.\\d+)",
    );
    let f = check_with_env(&p, bin.env()).unwrap_err();
    assert_eq!(f.found, None);
    assert_eq!(data(&f)["found"], Value::Null);
    assert!(f.message.contains("not found on PATH"), "{}", f.message);
    assert_eq!(
        f.message,
        "Product \"nonexistent-tool\" executable \"nonexistent-bin\" not found on PATH"
    );
}

#[cfg(unix)]
#[test]
fn fails_with_found_null_when_output_does_not_match_pattern() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("badtool", "echo \"unrecognized message with no numbers\"");
    let p = product(
        "badtool",
        ">=1.0.0",
        &["badtool", "--version"],
        "version\\s+(\\d+\\.\\d+\\.\\d+)",
    );
    let f = check_with_env(&p, bin.env()).unwrap_err();
    assert_eq!(f.found, None);
    assert_eq!(
        f.message,
        "Could not parse version for product \"badtool\" from detector output"
    );
}

#[cfg(unix)]
#[test]
fn caches_result_per_process_and_respects_reset_hook() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("counting", "echo \"counting version 2.0.0\"");
    let p = product(
        "counting",
        ">=2.0.0",
        &["counting", "--version"],
        "version\\s+(\\d+\\.\\d+\\.\\d+)",
    );

    // DetectExec is `dyn Fn + Send + Sync` with an implicit 'static bound: share state via Arc.
    let calls = Arc::new(AtomicUsize::new(0));
    let seen: Arc<Mutex<Vec<ExecCall>>> = Arc::new(Mutex::new(Vec::new()));
    let (calls_in, seen_in) = (Arc::clone(&calls), Arc::clone(&seen));
    let custom_exec = move |cmd: &Path, args: &[String], _cwd: &Path, _env: &HashMap<String, String>, _t: Duration| {
        calls_in.fetch_add(1, Ordering::SeqCst);
        seen_in.lock().unwrap().push((cmd.to_path_buf(), args.to_vec()));
        Ok(ExecResult {
            stdout: "version 2.0.0\n".into(),
            stderr: String::new(),
            exit_code: 0,
        })
    };
    let opts = CheckProductVersionOptions {
        exec: Some(&custom_exec),
        env: Some(bin.env()),
        ..CheckProductVersionOptions::default()
    };

    reset_product_version_cache();
    assert_eq!(check_product_version(&p, &opts), Ok("2.0.0".to_string()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // the injected exec receives the resolved binary and the remaining argv
    assert_eq!(
        seen.lock().unwrap()[0],
        (bin.0.join("counting"), vec!["--version".to_string()])
    );

    // Second call uses the cache, exec not invoked again
    assert_eq!(check_product_version(&p, &opts), Ok("2.0.0".to_string()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // After the reset hook, exec is invoked again
    reset_product_version_cache();
    assert_eq!(check_product_version(&p, &opts), Ok("2.0.0".to_string()));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[cfg(unix)]
#[test]
fn failures_are_cached_too() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    let p = product(
        "cached-failure-tool",
        ">=1.0.0",
        &["cachedfail"],
        "(\\d+\\.\\d+\\.\\d+)",
    );
    let first = check_with_env(&p, bin.env()).unwrap_err();
    assert!(first.message.contains("not found on PATH"));
    // The binary appears afterwards, but the cached failure is returned.
    bin.fake_executable("cachedfail", "echo 9.9.9");
    assert_eq!(check_with_env(&p, bin.env()), Err(first));
}

#[cfg(unix)]
#[test]
fn injected_exec_error_fails_with_found_null() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("brokenexec", "echo 1.0.0");
    let p = product("brokenexec", ">=1.0.0", &["brokenexec"], "(\\d+\\.\\d+\\.\\d+)");
    let failing = |_: &Path, _: &[String], _: &Path, _: &HashMap<String, String>, _: Duration| Err("boom".to_string());
    let f = check_product_version(
        &p,
        &CheckProductVersionOptions {
            exec: Some(&failing),
            env: Some(bin.env()),
            ..CheckProductVersionOptions::default()
        },
    )
    .unwrap_err();
    assert_eq!(f.found, None);
    assert_eq!(f.message, "Failed to execute product detector for \"brokenexec\": boom");
}

#[cfg(unix)]
#[test]
fn fails_with_found_null_when_execution_times_out() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("slowtool", "sleep 5\necho \"done\"");
    let p = product("slowtool", ">=1.0.0", &["slowtool"], "(\\d+)");
    let started = std::time::Instant::now();
    let f = check_product_version(
        &p,
        &CheckProductVersionOptions {
            timeout_ms: Some(50),
            env: Some(bin.env()),
            ..CheckProductVersionOptions::default()
        },
    )
    .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(4));
    assert_eq!(f.found, None);
    assert!(
        f.message.contains("Failed to execute product detector"),
        "{}",
        f.message
    );
}

#[test]
fn fails_when_detect_or_detect_argv_is_missing_or_empty() {
    let _g = lock();
    let p1 = ProductInfo {
        name: "no-detect".into(),
        supported: ">=1.0.0".into(),
        detect: None,
    };
    let f1 = check_with_env(&p1, HashMap::new()).unwrap_err();
    assert!(f1.message.contains("no detection configuration"), "{}", f1.message);
    assert_eq!(f1.found, None);

    let p2 = product("empty-argv", ">=1.0.0", &[], "");
    assert!(check_with_env(&p2, HashMap::new()).is_err());
}

#[cfg(unix)]
#[test]
fn fails_when_argv0_contains_invalid_characters_or_slashes() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    // Even an existing executable reached through a relative path is refused.
    std::fs::create_dir_all(bin.0.join("bin")).unwrap();
    bin.fake_executable("bin/mytool", "echo 1.2.3");
    let p = product("slash-bin", ">=1.0.0", &["../bin/mytool", "--version"], "(\\d+)");
    let f = check_with_env(&p, bin.env()).unwrap_err();
    assert_eq!(f.found, None);
    assert_eq!(data(&f)["found"], Value::Null);
}

#[cfg(unix)]
#[test]
fn fails_when_extracted_version_is_not_a_valid_semver() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("notsemver", "echo \"version abc.def\"");
    let p = product("notsemver", ">=1.0.0", &["notsemver"], "version\\s+(\\S+)");
    let f = check_with_env(&p, bin.env()).unwrap_err();
    assert!(f.message.contains("not a valid semver version"), "{}", f.message);
    assert_eq!(f.found, None);
}

#[cfg(unix)]
#[test]
fn max_output_bytes_limits_the_searched_output() {
    let _g = lock();
    let bin = BinDir::new("lmrust-product-test-");
    bin.fake_executable("longout", "echo \"xxxxxxxxxxxxxxxxxxxx version 1.2.3\"");
    let p = product("longout", ">=1.0.0", &["longout"], "version\\s+(\\d+\\.\\d+\\.\\d+)");
    let f = check_product_version(
        &p,
        &CheckProductVersionOptions {
            max_output_bytes: Some(10),
            env: Some(bin.env()),
            ..CheckProductVersionOptions::default()
        },
    )
    .unwrap_err();
    assert!(f.message.contains("Could not parse version"), "{}", f.message);
}

// lmts: `pathVar = options?.env?.PATH ?? process.env.PATH` -- an env map without PATH
// still resolves the detector through the process PATH.
#[cfg(unix)]
#[test]
fn env_without_path_falls_back_to_process_path() {
    let _g = lock();
    let p = product(
        "sh-fallback",
        ">=0.0.0",
        &["sh", "-c", "echo 1.0.0"],
        "(\\d+\\.\\d+\\.\\d+)",
    );
    let env: HashMap<String, String> = [("LMRUST_UNRELATED".to_string(), "1".to_string())]
        .into_iter()
        .collect();
    assert_eq!(check_with_env(&p, env), Ok("1.0.0".to_string()));
}

// ---------------------------------------------------------------------------
// find_executable_on_path (lmts `findExecutableOnPath`, exercised indirectly in lmts)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn find_executable_on_path_resolves_executables_in_path_order() {
    let a = BinDir::new("lmrust-find-a-");
    let b = BinDir::new("lmrust-find-b-");
    let in_b = b.fake_executable("tool-x", "true");
    let path = format!("{}:{}", a.path_str(), b.path_str());
    assert_eq!(find_executable_on_path("tool-x", &path), Some(in_b));

    let in_a = a.fake_executable("tool-x", "true");
    assert_eq!(find_executable_on_path("tool-x", &path), Some(in_a));
}

#[cfg(unix)]
#[test]
fn find_executable_on_path_skips_non_executables_and_directories() {
    let a = BinDir::new("lmrust-find-a-");
    let b = BinDir::new("lmrust-find-b-");
    a.write_file("tool-y", "#!/bin/sh\ntrue\n", 0o644);
    std::fs::create_dir_all(a.0.join("tool-z")).unwrap();
    let path = format!("{}:{}", a.path_str(), b.path_str());
    assert_eq!(find_executable_on_path("tool-y", &path), None);
    assert_eq!(find_executable_on_path("tool-z", &path), None);

    let y = b.fake_executable("tool-y", "true");
    let z = b.fake_executable("tool-z", "true");
    assert_eq!(find_executable_on_path("tool-y", &path), Some(y));
    assert_eq!(find_executable_on_path("tool-z", &path), Some(z));
}

#[cfg(unix)]
#[test]
fn find_executable_on_path_ignores_relative_and_empty_entries() {
    let a = BinDir::new("lmrust-find-rel-");
    a.fake_executable("tool-r", "true");
    // Relative entries (including "." and "") are never searched, even when they point at it.
    let rel = a.0.strip_prefix("/").unwrap().to_string_lossy().into_owned();
    let cwd_rel = format!(".:{rel}::");
    assert_eq!(find_executable_on_path("tool-r", &cwd_rel), None);
    assert_eq!(find_executable_on_path("tool-r", ""), None);
    // Surrounding whitespace on an absolute entry is trimmed.
    let padded = format!(":  {}  :", a.path_str());
    assert_eq!(find_executable_on_path("tool-r", &padded), Some(a.0.join("tool-r")));
}

#[cfg(unix)]
#[test]
fn find_executable_on_path_rejects_invalid_names() {
    let a = BinDir::new("lmrust-find-name-");
    std::fs::create_dir_all(a.0.join("sub")).unwrap();
    a.fake_executable("sub/tool", "true");
    a.fake_executable("Tool", "true");
    a.fake_executable("1tool", "true");
    let path = a.path_str();
    assert_eq!(find_executable_on_path("sub/tool", &path), None);
    assert_eq!(find_executable_on_path("Tool", &path), None);
    assert_eq!(find_executable_on_path("1tool", &path), None);
    assert_eq!(find_executable_on_path("", &path), None);
    assert_eq!(find_executable_on_path("../tool", &path), None);
}

// ---------------------------------------------------------------------------
// describe('runCli integration with product compatibility')
// ---------------------------------------------------------------------------

struct Run {
    code: i32,
    stdout: String,
}

fn run(spec: &CliSpec<()>, argv: &[&str], env: &HashMap<String, String>) -> Run {
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    let code = run_cli_io(spec, &argv, &mut out, env);
    Run {
        code,
        stdout: String::from_utf8(out).unwrap(),
    }
}

fn dummy_spec(p: ProductInfo) -> CliSpec<()> {
    CliSpec::new("git2", |_| ())
        .version("1.0.0")
        .op(OpDef::new("status", Access::Read, "Check status", |_, _: &()| {
            Ok(json!({ "status": "clean" }))
        })
        .schema(OperationSchema::new("status")))
        .product(p)
}

#[test]
fn run_cli_schema_and_list_succeed_even_when_product_check_fails() {
    let _g = lock();
    let bin = BinDir::new("lmrust-cli-product-test-");
    let spec = dummy_spec(product(
        "git-cli-missing",
        ">=2.43.0",
        &["nonexistent-git", "--version"],
        "(\\d+\\.\\d+\\.\\d+)",
    ));
    let env = bin.env();

    let schema = run(&spec, &["--schema"], &env);
    assert_eq!(schema.code, 0);
    assert!(schema.stdout.contains("\"schema\": \"cli.schema.v1\""));
    assert!(schema.stdout.contains("\"product\""));

    let list = run(&spec, &["--list"], &env);
    assert_eq!(list.code, 0);
    assert!(list.stdout.contains("status"));

    let help = run(&spec, &["--help"], &env);
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains("Operations"));

    let explain = run(&spec, &["--explain", "status"], &env);
    assert_eq!(explain.code, 0);

    // ...while an op invocation is refused
    let op = run(&spec, &["status"], &env);
    assert_eq!(op.code, 10);
    let j: Value = serde_json::from_str(&op.stdout).unwrap();
    assert_eq!(j["error"]["code"], "UNSUPPORTED_PRODUCT_VERSION");
    assert_eq!(j["error"]["data"]["found"], Value::Null);
}

#[cfg(unix)]
#[test]
fn run_cli_refuses_operation_with_exit_code_10_when_product_version_is_unsupported() {
    let _g = lock();
    let bin = BinDir::new("lmrust-cli-product-test-");
    bin.fake_executable("mygit", "echo \"mygit version 1.8.0\"");
    let spec = dummy_spec(product(
        "mygit-unsupported",
        ">=2.43.0",
        &["mygit", "--version"],
        "version\\s+(\\d+\\.\\d+\\.\\d+)",
    ));
    let r = run(&spec, &["status"], &bin.env());
    assert_eq!(r.code, 10);
    let j: Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(j["success"], false);
    assert_eq!(j["error"]["code"], "UNSUPPORTED_PRODUCT_VERSION");
    assert_eq!(j["error"]["data"]["product"], "mygit-unsupported");
    assert_eq!(j["error"]["data"]["found"], "1.8.0");
    assert_eq!(j["error"]["data"]["supported"], ">=2.43.0");
}

#[cfg(unix)]
#[test]
fn run_cli_executes_operation_successfully_when_product_check_passes() {
    let _g = lock();
    let bin = BinDir::new("lmrust-cli-product-test-");
    bin.fake_executable("mygit", "echo \"mygit version 2.45.0\"");
    let spec = dummy_spec(product(
        "mygit-supported",
        ">=2.43.0",
        &["mygit", "--version"],
        "version\\s+(\\d+\\.\\d+\\.\\d+)",
    ));
    let r = run(&spec, &["status"], &bin.env());
    assert_eq!(r.code, 0, "{}", r.stdout);
    let j: Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(j["status"], "clean");
}

#[test]
fn run_cli_product_checker_override_and_env_passthrough() {
    let _g = lock();
    let p = product("checker-override", ">=1.0.0", &["nothing-here"], "(\\d+)");
    let env: HashMap<String, String> = [("ALLOW".to_string(), "1".to_string())].into_iter().collect();
    let spec = dummy_spec(p).product_checker(|prod, env| {
        if env.get("ALLOW").map(String::as_str) == Some("1") {
            Ok("1.0.0".into())
        } else {
            Err(ProductCheckFailure {
                message: format!("denied {}", prod.name),
                product: prod.name.clone(),
                found: Some("0.1.0".into()),
                supported: prod.supported.clone(),
            })
        }
    });
    let ok = run(&spec, &["status"], &env);
    assert_eq!(ok.code, 0, "{}", ok.stdout);

    let denied = run(&spec, &["status"], &HashMap::new());
    assert_eq!(denied.code, 10);
    let j: Value = serde_json::from_str(&denied.stdout).unwrap();
    assert_eq!(
        j,
        json!({
            "success": false,
            "error": {
                "code": "UNSUPPORTED_PRODUCT_VERSION",
                "message": "denied checker-override",
                "data": { "product": "checker-override", "found": "0.1.0", "supported": ">=1.0.0" }
            }
        })
    );

    // The product check runs before op lookup (lmts runOp order): unknown op -> exit 10.
    let unknown = run(&spec, &["no_such_op"], &HashMap::new());
    assert_eq!(unknown.code, 10);
}
