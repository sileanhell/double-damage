//! Small helpers shared across the crate.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn err_str(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Unix time in milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Random bytes as lowercase hex (`bytes` long before encoding).
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("system random number generator unavailable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Unix seconds as an ISO 8601 UTC timestamp, e.g. `2026-10-01T00:00:00Z`.
pub fn iso(secs: i64) -> String {
    // Days-from-civil inverse (H. Hinnant), valid for the whole proleptic Gregorian range.
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso(1_793_012_228), "2026-10-26T10:57:08Z");
    }
}
