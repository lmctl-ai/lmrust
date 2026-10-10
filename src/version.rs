//! Version parsing and range satisfaction (lmts `version.ts`). A tiny in-house matcher:
//! ranges are conjunctions of 1 to 4 comparators `>=x.y.z`, `>x.y.z`, `<=x.y.z`, `<x.y.z`
//! or `=x.y.z`, which is all `product.supported` allows, so no semver crate is needed.

use std::cmp::Ordering;

/// A parsed `major.minor[.patch]` version (prefix `v` and any suffix tolerated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// The trimmed input.
    pub raw: String,
}

fn leading_digits(s: &str) -> (&str, &str) {
    let end = s.bytes().position(|b| !b.is_ascii_digit()).unwrap_or(s.len());
    (&s[..end], &s[end..])
}

/// Parse `v?MAJOR.MINOR(.PATCH)?` at the start of the trimmed text; patch defaults to 0.
/// Tolerates suffixes such as `-rc1`, `.windows.1` or ` (Ubuntu)`.
pub fn parse_version(version: &str) -> Option<ParsedVersion> {
    let trimmed = crate::js::trim(version);
    let body = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let (major, rest) = leading_digits(body);
    let rest = rest.strip_prefix('.')?;
    let (minor, rest) = leading_digits(rest);
    if major.is_empty() || minor.is_empty() {
        return None;
    }
    let patch = match rest.strip_prefix('.') {
        Some(r) => {
            let (p, _) = leading_digits(r);
            if p.is_empty() {
                "0"
            } else {
                p
            }
        }
        None => "0",
    };
    Some(ParsedVersion {
        major: major.parse().ok()?,
        minor: minor.parse().ok()?,
        patch: patch.parse().ok()?,
        raw: trimmed.to_string(),
    })
}

/// Compare major, then minor, then patch.
pub fn compare_versions(a: &ParsedVersion, b: &ParsedVersion) -> Ordering {
    (a.major, a.minor, a.patch).cmp(&(b.major, b.minor, b.patch))
}

/// `(>=|>|<=|<|=)(\d+\.\d+\.\d+)`
fn parse_comparator(part: &str) -> Option<(&str, ParsedVersion)> {
    let op = ["<=", ">=", "<", ">", "="]
        .into_iter()
        .find(|op| part.starts_with(op))?;
    let rest = &part[op.len()..];
    let mut pieces = rest.split('.');
    let nums: Vec<&str> = pieces.by_ref().take(3).collect();
    if pieces.next().is_some()
        || nums.len() != 3
        || nums
            .iter()
            .any(|n| n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some((op, parse_version(rest)?))
}

fn comparators(range: &str) -> Option<Vec<(&str, ParsedVersion)>> {
    let parts: Vec<&str> = range.split_whitespace().collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    parts.into_iter().map(parse_comparator).collect()
}

/// Whether `range` is a conjunction of 1 to 4 well-formed comparators.
pub fn is_valid_supported_range(range: &str) -> bool {
    comparators(range).is_some()
}

/// Whether `version` satisfies every comparator of `range` (false when either is malformed).
pub fn satisfies(version: &str, range: &str) -> bool {
    let (Some(v), Some(cs)) = (parse_version(version), comparators(range)) else {
        return false;
    };
    cs.iter().all(|(op, target)| {
        let cmp = compare_versions(&v, target);
        match *op {
            ">=" => cmp != Ordering::Less,
            ">" => cmp == Ordering::Greater,
            "<=" => cmp != Ordering::Greater,
            "<" => cmp == Ordering::Less,
            _ => cmp == Ordering::Equal,
        }
    })
}

/// `^[a-z][a-z0-9_.-]{0,63}$`: a valid `product.name`.
pub fn is_valid_product_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-'))
}

/// `^[a-z][a-z0-9_.-]*$`: a valid `product.detect.argv[0]` (no slashes).
pub fn is_valid_product_argv0(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-'))
}

/// Number of capture groups in a detection pattern; `None` when the pattern is longer than
/// 200 characters or is not a valid regular expression for the `regex-lite` engine.
pub fn count_regexp_groups(pattern: &str) -> Option<usize> {
    if pattern.chars().count() > 200 {
        return None;
    }
    regex_lite::Regex::new(pattern).ok().map(|re| re.captures_len() - 1)
}
