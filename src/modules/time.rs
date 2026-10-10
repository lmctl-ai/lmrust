//! ISO-8601 UTC timestamps without a date crate (`Date.prototype.toISOString` and the
//! subset of `Date.parse` the store reads back).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Milliseconds since the Unix epoch (negative before 1970).
pub fn epoch_ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

/// `YYYY-MM-DDTHH:MM:SS.sssZ`
pub fn to_iso_string(t: SystemTime) -> String {
    let ms = epoch_ms(t);
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        rem / 3_600_000,
        (rem / 60_000) % 60,
        (rem / 1000) % 60,
        rem % 1000
    )
}

fn num(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Parse `YYYY-MM-DD[THH:MM[:SS[.fff]]](Z|±HH:MM)?` to epoch milliseconds (date-only and
/// zone-less date-times are UTC). `None` for anything else (treated as "never checked").
pub fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let mut dp = date.split('-');
    let (y, mo, d) = (num(dp.next()?)?, num(dp.next()?)?, num(dp.next()?)?);
    if dp.next().is_some() || date.len() != 10 || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut ms = days_from_civil(y, mo, d) * 86_400_000;
    if let Some(t) = time {
        let (clock, offset_ms) = if let Some(c) = t.strip_suffix('Z') {
            (c, 0)
        } else if let Some(i) = t.rfind(['+', '-']) {
            let (c, off) = t.split_at(i);
            let sign = if off.starts_with('-') { -1 } else { 1 };
            let (oh, om) = off[1..].split_once(':')?;
            (c, sign * (num(oh)? * 60 + num(om)?) * 60_000)
        } else {
            (t, 0)
        };
        let mut cp = clock.split(':');
        let h = num(cp.next()?)?;
        let mi = num(cp.next()?)?;
        let (sec, frac) = match cp.next() {
            Some(sv) => match sv.split_once('.') {
                Some((s, f)) => (num(s)?, f),
                None => (num(sv)?, ""),
            },
            None => (0, ""),
        };
        if cp.next().is_some() || h > 24 || mi > 59 || sec > 59 {
            return None;
        }
        let frac_ms = if frac.is_empty() {
            0
        } else {
            let digits: String = frac.chars().chain("000".chars()).take(3).collect();
            num(&digits)?
        };
        ms += ((h * 60 + mi) * 60 + sec) * 1000 + frac_ms - offset_ms;
    }
    Some(ms)
}

/// SystemTime for epoch milliseconds (tests and clocks).
pub fn from_epoch_ms(ms: i64) -> SystemTime {
    if ms >= 0 {
        UNIX_EPOCH + Duration::from_millis(ms as u64)
    } else {
        UNIX_EPOCH - Duration::from_millis(ms.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for ms in [
            0i64,
            1_791_634_464_364,
            951_782_400_000,
            -86_400_000,
            253_402_300_799_999,
        ] {
            let iso = to_iso_string(from_epoch_ms(ms));
            assert_eq!(parse_iso_ms(&iso), Some(ms), "{iso}");
        }
        assert_eq!(
            to_iso_string(from_epoch_ms(1_791_648_864_364)),
            "2026-10-10T16:14:24.364Z"
        );
        assert_eq!(to_iso_string(from_epoch_ms(-1)), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn parses_variants() {
        let base = parse_iso_ms("2026-10-10T12:00:00.000Z").unwrap();
        assert_eq!(parse_iso_ms("2026-10-10T12:00:00Z"), Some(base));
        assert_eq!(parse_iso_ms("2026-10-10T12:00Z"), Some(base));
        assert_eq!(parse_iso_ms("2026-10-10T14:00:00+02:00"), Some(base));
        assert_eq!(parse_iso_ms("2026-10-10T11:30:00.5-00:30"), Some(base + 500));
        assert_eq!(parse_iso_ms("2026-10-10"), Some(base - 12 * 3_600_000));
        assert_eq!(parse_iso_ms("2026-10-10T12:00:00"), Some(base));
        for bad in [
            "",
            "not a date",
            "2026-13-01",
            "2026-1-01",
            "2026-10-10T25:00Z",
            "2026-10-10T12",
            "2026-10-10T12:00:61Z",
            "2026-10-10T12:00+0200",
        ] {
            assert_eq!(parse_iso_ms(bad), None, "{bad}");
        }
    }
}
