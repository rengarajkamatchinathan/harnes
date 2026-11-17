//! RFC 3339 timestamps without a date crate.

/// `2026-11-17T10:00:00.123Z` from seconds since the Unix epoch.
pub fn rfc3339_from_unix(secs: f64) -> String {
    let ms_total = (secs * 1000.0).round().max(0.0) as i64;
    let (days, ms_of_day) = (
        ms_total.div_euclid(86_400_000),
        ms_total.rem_euclid(86_400_000),
    );
    // Civil from days (H. Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let (h, rest) = (ms_of_day / 3_600_000, ms_of_day % 3_600_000);
    let (mi, rest) = (rest / 60_000, rest % 60_000);
    let (s, ms) = (rest / 1000, rest % 1000);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
}

/// Now, as RFC 3339.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    rfc3339_from_unix(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(rfc3339_from_unix(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            rfc3339_from_unix(1_795_000_000.5),
            "2026-11-18T11:06:40.500Z"
        );
        assert_eq!(rfc3339_from_unix(951_782_400.0), "2000-02-29T00:00:00.000Z");
    }
}
