//! Number, date and file-name formatting for the dashboard. Pure functions.

/// Civil date for a day count since the Unix epoch (proleptic Gregorian, UTC).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `2026-08-15`.
pub fn date(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `10-13`, for chart axes.
pub fn short_date(day: i64) -> String {
    let (_, m, d) = civil_from_days(day);
    format!("{m:02}-{d:02}")
}

/// `2026-08-15 08:05 UTC`.
pub fn datetime(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let day = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    format!("{} {:02}:{:02} UTC", date(day), rem / 3600, (rem % 3600) / 60)
}

/// `12345` as `12,345`.
pub fn int(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if value < 0 {
        out.push('-');
    }
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A ratio in `0..=1` as `99.25%`, or a dash when unknown.
pub fn pct(value: Option<f64>) -> String {
    match value {
        Some(v) if v.is_finite() => format!("{:.2}%", v * 100.0),
        _ => "\u{2013}".to_string(),
    }
}

/// A decimal with one fractional digit, or a dash.
pub fn dec1(value: Option<f64>) -> String {
    match value {
        Some(v) if v.is_finite() => format!("{v:.1}"),
        _ => "\u{2013}".to_string(),
    }
}

/// Microseconds as milliseconds: `16.7 ms`.
pub fn us_as_ms(us: Option<f64>) -> String {
    match us {
        Some(v) if v.is_finite() => format!("{:.1} ms", v / 1000.0),
        _ => "\u{2013}".to_string(),
    }
}

/// Seconds as `1h 05m`, `3m 20s` or `45s`.
pub fn seconds(value: Option<f64>) -> String {
    match value {
        Some(v) if v.is_finite() && v >= 0.0 => {
            let total = v.round() as u64;
            if total >= 3600 {
                format!("{}h {:02}m", total / 3600, (total % 3600) / 60)
            } else if total >= 60 {
                format!("{}m {:02}s", total / 60, total % 60)
            } else {
                format!("{total}s")
            }
        }
        _ => "\u{2013}".to_string(),
    }
}

/// `a / b`, or `None` when `b` is zero.
pub fn ratio(a: i64, b: i64) -> Option<f64> {
    (b > 0).then(|| a as f64 / b as f64)
}

/// The first `n` characters, for showing an identifier compactly.
pub fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// A file-name fragment containing only `A-Za-z0-9._-`.
pub fn file_safe(text: &str) -> String {
    let cleaned: String = text.chars().take(64).map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() { "report".to_string() } else { trimmed.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_match_known_days() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(19_723), "2024-01-01");
        assert_eq!(date(-1), "1969-12-31");
        assert_eq!(date(11_016), "2000-02-29");
        assert_eq!(short_date(19_723), "01-01");
        assert_eq!(datetime(86_400_000 + 3_660_000), "1970-01-02 01:01 UTC");
    }

    #[test]
    fn numbers_format() {
        assert_eq!(int(0), "0");
        assert_eq!(int(999), "999");
        assert_eq!(int(1_000), "1,000");
        assert_eq!(int(-1_234_567), "-1,234,567");
        assert_eq!(int(i64::MIN).len(), 26);
        assert_eq!(pct(Some(0.5)), "50.00%");
        assert_eq!(pct(None), "\u{2013}");
        assert_eq!(pct(Some(f64::NAN)), "\u{2013}");
        assert_eq!(us_as_ms(Some(16_667.0)), "16.7 ms");
        assert_eq!(seconds(Some(200.0)), "3m 20s");
        assert_eq!(seconds(Some(3_900.0)), "1h 05m");
        assert_eq!(seconds(Some(-1.0)), "\u{2013}");
        assert_eq!(ratio(1, 0), None);
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_safe("1.2.0"), "1.2.0");
        assert_eq!(file_safe("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(file_safe("a\"b\r\nc"), "a_b__c");
        assert_eq!(file_safe(""), "report");
        assert_eq!(head("héllo", 2), "hé");
    }
}
