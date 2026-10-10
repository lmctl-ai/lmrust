//! Cache layout, path resolution and PATH lookups (lmts `modules/paths.ts`).

use super::types::{Channel, ModuleError, ModuleErrorCode, ModuleSpec};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// `${LMCTL_HOME:-$HOME/.lmctl}/modules` for the process environment.
pub fn cache_root() -> PathBuf {
    let env: HashMap<String, String> = std::env::vars().collect();
    cache_root_from(&env)
}

/// `${LMCTL_HOME:-$HOME/.lmctl}/modules` for an explicit environment (a blank
/// `LMCTL_HOME` counts as unset; without `HOME`, `USERPROFILE` is used, then `.`).
pub fn cache_root_from(env: &HashMap<String, String>) -> PathBuf {
    let home = match env.get("LMCTL_HOME") {
        Some(h) if !crate::js::trim(h).is_empty() => PathBuf::from(h),
        _ => {
            let user_home = env
                .get("HOME")
                .or_else(|| env.get("USERPROFILE"))
                .cloned()
                .unwrap_or_else(|| ".".into());
            PathBuf::from(user_home).join(".lmctl")
        }
    };
    home.join("modules")
}

/// Absolute, lexically normalized path (`.` and `..` resolved; symlinks are not followed).
pub(crate) fn resolve(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `true` when `inner` is `outer` or below it.
pub(crate) fn is_within(outer: &Path, inner: &Path) -> bool {
    inner.starts_with(outer)
}

/// `root/github.com/<owner>/<repo>/<channel>`, refusing anything that escapes the root.
pub fn entry_dir(root: &Path, spec: &ModuleSpec) -> Result<PathBuf, ModuleError> {
    check_segments(spec)?;
    let abs_root = resolve(root);
    let gh_root = abs_root.join("github.com");
    let dir = resolve(&gh_root.join(&spec.owner).join(&spec.repo).join(spec.channel.as_str()));
    if !is_within(&abs_root, &dir) || !is_within(&gh_root, &dir) {
        return Err(ModuleError::new(
            ModuleErrorCode::PathTraversal,
            format!(
                "Path traversal detected: entry directory \"{}\" escapes cache root \"{}\"",
                dir.display(),
                abs_root.display()
            ),
        ));
    }
    Ok(dir)
}

/// Hand-built specs bypass `parse_spec`; refuse owner/repo names it would reject (`.`, `..`,
/// separators). lmts only checks the resolved path, so `@./.` with `all_channels` would remove
/// the whole `github.com` tree there.
pub(crate) fn check_segments(spec: &ModuleSpec) -> Result<(), ModuleError> {
    if super::spec::is_valid_segment(&spec.owner) && super::spec::is_valid_segment(&spec.repo) {
        Ok(())
    } else {
        Err(ModuleError::new(
            ModuleErrorCode::PathTraversal,
            format!("Path traversal detected: invalid owner or repo in \"{}\"", spec.key),
        ))
    }
}

/// `cli.schema.<channel>.json`
pub fn schema_file_name(channel: Channel) -> String {
    format!("cli.schema.{}.json", channel.as_str())
}

/// Search the absolute entries of `path_var` (default: the process `PATH`) for a regular
/// executable `name`. Names must match `^[a-z][a-z0-9_]*$`; relative and empty entries
/// (including the working directory) are ignored.
pub fn find_on_path(name: &str, path_var: Option<&str>) -> Option<PathBuf> {
    let b = name.as_bytes();
    let valid = !b.is_empty()
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_');
    if !valid {
        return None;
    }
    let owned;
    let path_var = match path_var {
        Some(p) => p,
        None => {
            owned = std::env::var("PATH").unwrap_or_default();
            &owned
        }
    };
    crate::server::find_executable_on_path(name, path_var)
}
