//! Port of lmts `tests/version.test.ts`: parseVersion, compareVersions,
//! isValidSupportedRange, satisfies and countRegExpGroups.

use lmrust::{compare_versions, count_regexp_groups, is_valid_supported_range, parse_version, satisfies};
use std::cmp::Ordering;

// describe('parseVersion')
#[test]
fn parse_version_table() {
    type Expected = Option<(u64, u64, u64)>;
    let table: &[(&str, Expected)] = &[
        ("1.0.0", Some((1, 0, 0))),
        ("v1.2.3", Some((1, 2, 3))),
        ("2.43.0", Some((2, 43, 0))),
        ("2.43.0-rc1", Some((2, 43, 0))),
        ("2.43.0.windows.1", Some((2, 43, 0))),
        ("2.45.0 (2025-07-18 Ubuntu 2.45.0-1ubuntu0.3)", Some((2, 45, 0))),
        ("aws-cli/2.34.48 Python/3.14.4 Linux", None),
        ("2.34.48 Python/3.14.4", Some((2, 34, 48))),
        ("10.200.300", Some((10, 200, 300))),
        ("2.43", Some((2, 43, 0))),
        ("v2.43", Some((2, 43, 0))),
        ("", None),
        ("not_a_version", None),
        ("1", None),
        // `null as unknown as string` -> null is not expressible: parse_version takes &str.
    ];
    for (input, expected) in table {
        let got = parse_version(input).map(|v| (v.major, v.minor, v.patch));
        assert_eq!(got, *expected, "parses {input:?}");
    }
}

// describe('compareVersions')
#[test]
fn compare_versions_correctly_orders_major_minor_patch() {
    let v1 = parse_version("1.0.0").unwrap();
    let v2 = parse_version("2.0.0").unwrap();
    let v1_1 = parse_version("1.1.0").unwrap();
    let v1_0_1 = parse_version("1.0.1").unwrap();

    assert_eq!(compare_versions(&v1, &v2), Ordering::Less);
    assert_eq!(compare_versions(&v2, &v1), Ordering::Greater);
    assert_eq!(compare_versions(&v1, &v1), Ordering::Equal);
    assert_eq!(compare_versions(&v1, &v1_1), Ordering::Less);
    assert_eq!(compare_versions(&v1, &v1_0_1), Ordering::Less);
}

// describe('isValidSupportedRange')
#[test]
fn is_valid_supported_range_table() {
    let table = [
        (">=2.43.0 <3.0.0", true),
        ("=2.43.0", true),
        (">1.0.0 <=2.0.0", true),
        (">=1.0.0 >1.1.0 <=2.0.0 <2.0.0", true),
        ("", false),
        ("   ", false),
        (">=1.0.0 >1.1.0 <=2.0.0 <2.0.0 =1.5.0", false), // 5 comparators (max 4)
        ("2.43.0", false),                               // missing operator
        ("~1.0.0", false),                               // unsupported operator
        ("^1.0.0", false),
        (">=2.43", false),       // must be x.y.z
        (">=2.43.0-rc1", false), // must be numeric x.y.z
    ];
    for (range, valid) in table {
        assert_eq!(is_valid_supported_range(range), valid, "range {range:?}");
    }
}

// describe('satisfies')
#[test]
fn satisfies_table() {
    let table = [
        // Conjunction >=2.43.0 <3.0.0
        ("2.43.0", ">=2.43.0 <3.0.0", true),
        ("2.43.1", ">=2.43.0 <3.0.0", true),
        ("2.43.0-rc1", ">=2.43.0 <3.0.0", true),
        ("2.99.9", ">=2.43.0 <3.0.0", true),
        ("2.42.9", ">=2.43.0 <3.0.0", false),
        ("3.0.0", ">=2.43.0 <3.0.0", false),
        ("3.1.0", ">=2.43.0 <3.0.0", false),
        ("1.9.0", ">=2.43.0 <3.0.0", false),
        // Exact equal =2.0.0
        ("2.0.0", "=2.0.0", true),
        ("2.0.1", "=2.0.0", false),
        ("1.9.9", "=2.0.0", false),
        // Strict greater >1.5.0
        ("1.5.0", ">1.5.0", false),
        ("1.5.1", ">1.5.0", true),
        // Less or equal <=2.10.0
        ("2.10.0", "<=2.10.0", true),
        ("2.9.99", "<=2.10.0", true),
        ("2.10.1", "<=2.10.0", false),
        // Strict less <2.0.0
        ("2.0.0", "<2.0.0", false),
        ("1.99.99", "<2.0.0", true),
        // Invalid ranges or versions
        ("invalid", ">=1.0.0", false),
        ("1.0.0", "invalid_range", false),
        ("1.0.0", "", false),
        ("1.0.0", ">=1.0.0 >=1.0.0 >=1.0.0 >=1.0.0 >=1.0.0", false),
    ];
    for (version, range, expected) in table {
        assert_eq!(satisfies(version, range), expected, "{version:?} satisfies {range:?}");
    }
}

// describe('countRegExpGroups') — lmts returns -1 where lmrust returns None.
#[test]
fn count_regexp_groups_returns_1_for_single_capture_group() {
    assert_eq!(count_regexp_groups(r"(\d+\.\d+\.\d+)"), Some(1));
    assert_eq!(count_regexp_groups(r"version\s+([0-9.]+)"), Some(1));
    assert_eq!(count_regexp_groups(r"(?:git\s+version\s+)(\d+\.\d+\.\d+)"), Some(1));
}

#[test]
fn count_regexp_groups_returns_2_for_two_capture_groups() {
    assert_eq!(count_regexp_groups(r"(\d+)\.(\d+)"), Some(2));
}

#[test]
fn count_regexp_groups_returns_0_for_no_capture_groups() {
    assert_eq!(count_regexp_groups(r"\d+\.\d+\.\d+"), Some(0));
    assert_eq!(count_regexp_groups(r"(?:\d+\.\d+)"), Some(0));
}

#[test]
fn count_regexp_groups_returns_none_for_invalid_regex_syntax_or_over_200_chars() {
    assert_eq!(count_regexp_groups("[invalid"), None);
    assert_eq!(count_regexp_groups(&"a".repeat(201)), None);
}
