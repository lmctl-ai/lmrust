//! JavaScript-compatible primitives the reference SDK (lmts) relies on implicitly:
//! `Number(string)`, `String(number)`, `JSON.stringify` number formatting, strict
//! equality and `typeof`. Keeping them in one place makes the Rust port produce the
//! same values and the same JSON text as lmts.

use serde::Serialize;
use serde_json::ser::{Formatter, PrettyFormatter};
use serde_json::{Number, Value};
use std::io;

/// Largest integer JavaScript represents exactly (2^53 - 1).
const MAX_SAFE: f64 = 9_007_199_254_740_991.0;

/// A JSON number for `n`, stored as an integer when it is integral and safe
/// (so `1.0` compares and prints like JavaScript's `1`). `None` for NaN/Infinity.
pub fn number_value(n: f64) -> Option<Value> {
    if !n.is_finite() {
        return None;
    }
    if n.fract() == 0.0 && n.abs() <= MAX_SAFE {
        return Some(Value::Number(Number::from(n as i64)));
    }
    Number::from_f64(n).map(Value::Number)
}

/// `Number.prototype.toString()` for a finite or non-finite double.
pub fn number_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n == 0.0 {
        return "0".into();
    }
    let negative = n < 0.0;
    // Shortest round-trip digits in scientific form, e.g. "1.2345e-7".
    let sci = format!("{:e}", n.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exp: i32 = exp.parse().unwrap_or(0);
    let k = digits.len() as i32;
    let point = exp + 1; // position of the decimal point relative to the digits ("n" in ECMA-262)
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if k <= point && point <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat('0').take((point - k) as usize));
    } else if 0 < point && point <= 21 {
        out.push_str(&digits[..point as usize]);
        out.push('.');
        out.push_str(&digits[point as usize..]);
    } else if -6 < point && point <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat('0').take((-point) as usize));
        out.push_str(&digits);
    } else {
        let e = point - 1;
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if e >= 0 { '+' } else { '-' });
        out.push_str(&e.abs().to_string());
    }
    out
}

fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{A}' | '\u{B}' | '\u{C}' | '\u{D}' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `String.prototype.trim()`.
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// `Number(string)`: NaN when the text is not a JavaScript numeric literal.
pub fn parse_number(s: &str) -> f64 {
    let t = trim(s);
    if t.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(rest) = t.strip_prefix(prefix) {
            if rest.is_empty() || !rest.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return rest
                .chars()
                .fold(0.0, |acc, c| acc * radix as f64 + c.to_digit(radix).unwrap_or(0) as f64);
        }
    }
    let unsigned = t.strip_prefix(['+', '-']).unwrap_or(t);
    if unsigned == "Infinity" {
        return if t.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    if !is_decimal_literal(unsigned) {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// `digits [. digits?] | . digits` followed by an optional exponent.
fn is_decimal_literal(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        frac_digits = i - start;
    }
    if int_digits == 0 && frac_digits == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    i == b.len()
}

/// `typeof value` (null, arrays and objects are all `"object"`).
pub fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Null | Value::Array(_) | Value::Object(_) => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
    }
}

/// Strict equality (`===`) for JSON values: primitives by value, numbers as doubles,
/// arrays and objects never equal (they would be distinct references).
pub fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => false,
    }
}

/// Structural equality with JavaScript number semantics (`1` equals `1.0`).
pub fn deep_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| deep_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| deep_eq(v, w)))
        }
        _ => a == b,
    }
}

/// `String(value)` for a JSON value.
pub fn to_display(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number_text(n),
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|i| if i.is_null() { String::new() } else { to_display(i) })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// JavaScript text of a JSON number.
pub fn number_text(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        if (i as f64).abs() <= MAX_SAFE {
            return i.to_string();
        }
    }
    if let Some(u) = n.as_u64() {
        if (u as f64) <= MAX_SAFE {
            return u.to_string();
        }
    }
    number_to_string(n.as_f64().unwrap_or(f64::NAN))
}

/// `JSON.stringify(value)`.
pub fn stringify<T: Serialize + ?Sized>(value: &T) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, JsCompact);
    value.serialize(&mut ser).expect("JSON values always serialize");
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

/// `JSON.stringify(value, null, 2)`.
pub fn stringify_pretty<T: Serialize + ?Sized>(value: &T) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, JsPretty(PrettyFormatter::with_indent(b"  ")));
    value.serialize(&mut ser).expect("JSON values always serialize");
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

/// Writes doubles the way JavaScript does (`1` not `1.0`, `1e+21` not `1e21`).
fn write_js_f64<W: ?Sized + io::Write>(writer: &mut W, value: f64) -> io::Result<()> {
    if value.is_finite() {
        writer.write_all(number_to_string(value).as_bytes())
    } else {
        writer.write_all(b"null")
    }
}

struct JsCompact;

impl Formatter for JsCompact {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        write_js_f64(writer, value)
    }
    fn write_f32<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        write_js_f64(writer, value as f64)
    }
}

struct JsPretty<'a>(PrettyFormatter<'a>);

impl Formatter for JsPretty<'_> {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        write_js_f64(writer, value)
    }
    fn write_f32<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        write_js_f64(writer, value as f64)
    }
    fn begin_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_array(writer)
    }
    fn end_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array(writer)
    }
    fn begin_array_value<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        self.0.begin_array_value(writer, first)
    }
    fn end_array_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array_value(writer)
    }
    fn begin_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object(writer)
    }
    fn end_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object(writer)
    }
    fn begin_object_key<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        self.0.begin_object_key(writer, first)
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object_value(writer)
    }
    fn end_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object_value(writer)
    }
}

/// Edit distance over Unicode scalar values (lmts measures UTF-16 units; identical for ASCII).
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut dp: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = dp[0];
        dp[0] = i;
        for j in 1..=b.len() {
            let tmp = dp[j];
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            dp[j] = (dp[j] + 1).min(dp[j - 1] + 1).min(prev + cost);
            prev = tmp;
        }
    }
    dp[b.len()]
}

/// `String.prototype.localeCompare` for identifiers, approximating the ICU root
/// collation JavaScript engines use: punctuation sorts before digits, digits before
/// letters, letters compare case-insensitively first and lowercase before uppercase
/// on a tie. Exact for the ASCII identifier alphabet published schemas use.
pub fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let primary = |s: &str| s.chars().map(primary_weight).collect::<Vec<_>>();
    primary(a).cmp(&primary(b)).then_with(|| {
        let tertiary = |s: &str| s.chars().map(|c| u32::from(c.is_ascii_uppercase())).collect::<Vec<_>>();
        tertiary(a).cmp(&tertiary(b))
    })
}

fn primary_weight(c: char) -> u32 {
    // CLDR root order for the ASCII punctuation and symbols that can appear in names.
    const PUNCT: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";
    if c == ' ' {
        return 0;
    }
    if let Some(i) = PUNCT.find(c) {
        return 1 + i as u32;
    }
    if c.is_ascii_digit() {
        return 100 + (c as u32 - '0' as u32);
    }
    if c.is_ascii_alphabetic() {
        return 200 + (c.to_ascii_lowercase() as u32 - 'a' as u32);
    }
    1000 + c as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn number_to_string_matches_javascript() {
        let cases = [
            (1.0, "1"),
            (-1.5, "-1.5"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1e-7, "1e-7"),
            (123456789012345680000.0, "123456789012345680000"),
            (0.000001, "0.000001"),
            (1.5e-10, "1.5e-10"),
            (2.5e30, "2.5e+30"),
            (-0.0, "0"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (n, s) in cases {
            assert_eq!(number_to_string(n), s, "{n}");
        }
    }

    #[test]
    fn parse_number_matches_javascript() {
        assert_eq!(parse_number("12"), 12.0);
        assert_eq!(parse_number(" 1.5 "), 1.5);
        assert_eq!(parse_number(""), 0.0);
        assert_eq!(parse_number("0x10"), 16.0);
        assert_eq!(parse_number("0b11"), 3.0);
        assert_eq!(parse_number("0o17"), 15.0);
        assert_eq!(parse_number("1e3"), 1000.0);
        assert_eq!(parse_number(".5"), 0.5);
        assert_eq!(parse_number("5."), 5.0);
        assert_eq!(parse_number("-Infinity"), f64::NEG_INFINITY);
        assert_eq!(parse_number("+Infinity"), f64::INFINITY);
        for bad in ["abc", "1e", "0x", "-0x1", "inf", "NaN", "1.2.3", ".", "e5", "0xZ"] {
            assert!(parse_number(bad).is_nan(), "{bad}");
        }
    }

    #[test]
    fn stringify_uses_javascript_numbers() {
        assert_eq!(
            stringify(&json!({"a": 1.0, "b": [1e21, 0.5]})),
            r#"{"a":1,"b":[1e+21,0.5]}"#
        );
        assert_eq!(
            stringify_pretty(&json!({"a": [1, {}], "b": []})),
            "{\n  \"a\": [\n    1,\n    {}\n  ],\n  \"b\": []\n}"
        );
        assert_eq!(stringify(&1.5f32), "1.5");
        assert_eq!(stringify_pretty(&2.0f32), "2");
    }

    #[test]
    fn equality_and_display() {
        assert!(strict_eq(&json!(1), &json!(1.0)));
        assert!(!strict_eq(&json!("1"), &json!(1)));
        assert!(!strict_eq(&json!([1]), &json!([1])));
        assert!(strict_eq(&json!(null), &json!(null)));
        assert!(strict_eq(&json!(true), &json!(true)));
        assert!(deep_eq(&json!({"a": [1.0]}), &json!({"a": [1]})));
        assert!(!deep_eq(&json!({"a": 1}), &json!({"b": 1})));
        assert_eq!(to_display(&json!([1, null, "x"])), "1,,x");
        assert_eq!(to_display(&json!({})), "[object Object]");
        assert_eq!(to_display(&json!(null)), "null");
        assert_eq!(to_display(&json!(false)), "false");
        assert_eq!(type_of(&json!(null)), "object");
        assert_eq!(type_of(&json!(true)), "boolean");
        assert_eq!(number_text(&Number::from(u64::MAX)), "18446744073709552000");
        assert_eq!(number_text(&Number::from(i64::MIN)), "-9223372036854776000");
        assert_eq!(number_value(f64::NAN), None);
        assert_eq!(number_value(2.0), Some(json!(2)));
        assert_eq!(number_value(1e300), Some(json!(1e300)));
    }

    #[test]
    fn locale_compare_orders_like_icu() {
        use std::cmp::Ordering::*;
        assert_eq!(locale_compare("pr_list", "prlist"), Less);
        assert_eq!(locale_compare("a_b", "a1"), Less);
        assert_eq!(locale_compare("a-b", "a_b"), Greater);
        assert_eq!(locale_compare("B", "a"), Greater);
        assert_eq!(locale_compare("a", "A"), Less);
        assert_eq!(locale_compare("abc", "abc"), Equal);
        assert_eq!(locale_compare("a b", "a_b"), Less);
        assert_eq!(locale_compare("é", "z"), Greater);
    }

    #[test]
    fn levenshtein_counts_edits() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("same", "same"), 0);
    }

    #[test]
    fn trim_handles_unicode_space() {
        assert_eq!(trim("\u{feff} x \u{3000}"), "x");
    }
}
