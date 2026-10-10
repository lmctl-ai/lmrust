//! Product compatibility check (lmts `server/product.ts`): resolve `detect.argv[0]` on the
//! absolute entries of `PATH`, run it, extract the version with `detect.pattern` and check
//! it against `supported`. Results are cached per product name for the process.

use super::exec::{spawn_command, ExecOptions, ExecResult};
use crate::types::{ProductInfo, Record};
use crate::version::{parse_version, satisfies};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Runs the detector: `(binary, args, cwd, env, timeout)`.
pub type DetectExec =
    dyn Fn(&Path, &[String], &Path, &HashMap<String, String>, Duration) -> Result<ExecResult, String> + Send + Sync;

/// Options of [`check_product_version`]; every field has a production default.
#[derive(Default)]
pub struct CheckProductVersionOptions<'a> {
    /// Replace process execution (tests).
    pub exec: Option<&'a DetectExec>,
    /// Detector deadline (default 3000 ms).
    pub timeout_ms: Option<u64>,
    /// Characters of combined output searched (default 4096).
    pub max_output_bytes: Option<usize>,
    /// Environment for the detector and its `PATH` lookup (default: the process environment).
    pub env: Option<HashMap<String, String>>,
}

/// Why a product check failed; maps to `UNSUPPORTED_PRODUCT_VERSION` (exit 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductCheckFailure {
    pub message: String,
    pub product: String,
    /// The detected version, when one was extracted.
    pub found: Option<String>,
    pub supported: String,
}

impl ProductCheckFailure {
    /// `{product, found, supported}` for the error envelope's `data`.
    pub fn data(&self) -> Record {
        let mut d = Record::new();
        d.insert("product".into(), Value::String(self.product.clone()));
        d.insert(
            "found".into(),
            self.found.clone().map(Value::String).unwrap_or(Value::Null),
        );
        d.insert("supported".into(), Value::String(self.supported.clone()));
        d
    }
}

/// `Ok(found_version)` or the failure.
pub type ProductCheckResult = Result<String, ProductCheckFailure>;

static CACHE: Mutex<Option<HashMap<String, ProductCheckResult>>> = Mutex::new(None);

/// Clear the per-process detection cache (test hook).
pub fn reset_product_version_cache() {
    if let Ok(mut c) = CACHE.lock() {
        *c = None;
    }
}

fn is_tool_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-'))
}

/// Search the absolute entries of `path_var` for a regular executable file `name`
/// (relative and empty entries, including the working directory, are ignored).
pub fn find_executable_on_path(name: &str, path_var: &str) -> Option<PathBuf> {
    if !is_tool_name(name) {
        return None;
    }
    let delimiter = if cfg!(windows) { ';' } else { ':' };
    for part in path_var.split(delimiter) {
        let trimmed = part.trim();
        if trimmed.is_empty() || !Path::new(trimmed).is_absolute() {
            continue;
        }
        let candidate = Path::new(trimmed).join(name);
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        return Some(candidate);
    }
    None
}

/// Check the host's product version against `product.supported` (cached per product name).
pub fn check_product_version(product: &ProductInfo, options: &CheckProductVersionOptions<'_>) -> ProductCheckResult {
    if let Ok(cache) = CACHE.lock() {
        if let Some(hit) = cache.as_ref().and_then(|c| c.get(&product.name)) {
            return hit.clone();
        }
    }
    let result = detect(product, options);
    if let Ok(mut cache) = CACHE.lock() {
        cache
            .get_or_insert_with(HashMap::new)
            .insert(product.name.clone(), result.clone());
    }
    result
}

fn detect(product: &ProductInfo, options: &CheckProductVersionOptions<'_>) -> ProductCheckResult {
    let fail = |message: String, found: Option<String>| ProductCheckFailure {
        message,
        product: product.name.clone(),
        found,
        supported: product.supported.clone(),
    };
    let Some(detect) = product.detect.as_ref().filter(|d| !d.argv.is_empty()) else {
        return Err(fail(
            format!("Product \"{}\" has no detection configuration", product.name),
            None,
        ));
    };
    let env: HashMap<String, String> = options.env.clone().unwrap_or_else(|| std::env::vars().collect());
    let cmd_name = &detect.argv[0];
    let path_var = env.get("PATH").cloned().unwrap_or_default();
    let Some(bin) = find_executable_on_path(cmd_name, &path_var) else {
        return Err(fail(
            format!(
                "Product \"{}\" executable \"{}\" not found on PATH",
                product.name, cmd_name
            ),
            None,
        ));
    };
    let timeout = Duration::from_millis(options.timeout_ms.unwrap_or(3000));
    let max_chars = options.max_output_bytes.unwrap_or(4096);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let args = &detect.argv[1..];
    let ran = match options.exec {
        Some(exec) => exec(&bin, args, &cwd, &env, timeout),
        None => spawn_command(
            &bin.to_string_lossy(),
            args,
            &cwd,
            &env,
            ExecOptions {
                timeout,
                allow_non_zero_exit: true,
                cancel: None,
            },
        )
        .map_err(|e| e.to_string()),
    };
    let res = match ran {
        Ok(r) => r,
        Err(e) => {
            return Err(fail(
                format!("Failed to execute product detector for \"{}\": {}", product.name, e),
                None,
            ))
        }
    };
    let combined: String = format!("{}\n{}", res.stdout, res.stderr)
        .chars()
        .take(max_chars)
        .collect();
    let extracted = regex_lite::Regex::new(&detect.pattern)
        .ok()
        .and_then(|re| {
            re.captures(&combined)
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        })
        .filter(|s| !s.is_empty());
    let Some(found) = extracted else {
        return Err(fail(
            format!(
                "Could not parse version for product \"{}\" from detector output",
                product.name
            ),
            None,
        ));
    };
    if parse_version(&found).is_none() {
        return Err(fail(
            format!(
                "Extracted version \"{}\" for product \"{}\" is not a valid semver version",
                found, product.name
            ),
            None,
        ));
    }
    if !satisfies(&found, &product.supported) {
        return Err(fail(
            format!(
                "Product \"{}\" version {} does not satisfy supported range \"{}\"",
                product.name, found, product.supported
            ),
            Some(found),
        ));
    }
    Ok(found)
}
