//! UTC timestamps in the one RFC 3339 shape the feeds use: `YYYY-MM-DDTHH:MM:SSZ`.
//! Civil-date conversion after Howard Hinnant's `days_from_civil` (no date crate needed).

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn format(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        (s / 60) % 60,
        s % 60
    )
}

pub fn parse(s: &str) -> Result<i64, String> {
    let b = s.as_bytes();
    let bad = || format!("`{s}` is not a UTC timestamp like 2026-11-17T00:00:00Z");
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return Err(bad());
    }
    let num = |r: std::ops::Range<usize>| -> Result<i64, String> {
        s.get(r)
            .filter(|t| t.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|t| t.parse().ok())
            .ok_or_else(bad)
    };
    let (y, mo, d, h, mi, se) = (
        num(0..4)?,
        num(5..7)?,
        num(8..10)?,
        num(11..13)?,
        num(14..16)?,
        num(17..19)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 59 {
        return Err(bad());
    }
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se;
    // Reject dates like 02-31 that normalise to another day.
    if format(secs) != s {
        return Err(bad());
    }
    Ok(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        assert_eq!(format(0), "1970-01-01T00:00:00Z");
        assert_eq!(parse("2026-11-17T12:34:56Z"), Ok(1_794_918_896));
        assert_eq!(format(1_794_918_896), "2026-11-17T12:34:56Z");
        assert_eq!(parse("2024-02-29T00:00:00Z").map(format).as_deref(), Ok("2024-02-29T00:00:00Z"));
        assert!(parse("2026-02-30T00:00:00Z").is_err());
        assert!(parse("2026-11-17 00:00:00Z").is_err());
        assert!(parse("2026-11-17T00:00:00+01:00").is_err());
    }
}
