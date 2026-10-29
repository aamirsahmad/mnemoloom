//! Timestamp helpers with Python-style (`datetime`) formatting.

use std::time::SystemTime;

use chrono::{DateTime, FixedOffset, Local, NaiveDateTime, TimeZone, Timelike};

/// Current Unix time in seconds, or zero for a clock before the epoch.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Python `datetime.fromtimestamp(st_mtime)`: the float timestamp rounded
/// half-even to whole microseconds.
fn py_local(t: SystemTime) -> DateTime<Local> {
    let f = match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    };
    let mut secs = f.trunc();
    let mut us = ((f - secs) * 1e6).round_ties_even();
    if us >= 1e6 {
        secs += 1.0;
        us -= 1e6;
    } else if us < 0.0 {
        secs -= 1.0;
        us += 1e6;
    }
    Local
        .timestamp_opt(secs as i64, (us as u32) * 1000)
        .single()
        .unwrap_or_else(|| t.into())
}

/// Python `datetime.fromtimestamp(t).astimezone().isoformat()`:
/// `2026-10-28T23:19:59.322000-04:00` (fraction only when non-zero).
pub fn iso_local_offset(t: SystemTime) -> String {
    let dt = py_local(t);
    let base = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
    format!(
        "{base}{}{}",
        micros_suffix(dt.nanosecond()),
        dt.format("%:z")
    )
}

/// Python `datetime.fromtimestamp(t).isoformat()` (naive local, no offset).
pub fn iso_local_naive(t: SystemTime) -> String {
    let dt = py_local(t);
    format!(
        "{}{}",
        dt.format("%Y-%m-%dT%H:%M:%S"),
        micros_suffix(dt.nanosecond())
    )
}

/// `%Y-%m-%d %H:%M:%S` in local time.
pub fn local_display(t: SystemTime) -> String {
    let dt = py_local(t);
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn micros_suffix(nanos: u32) -> String {
    let micros = (nanos % 1_000_000_000) / 1000;
    if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    }
}

/// Parse an ISO timestamp as written in session files (`...Z`, `±HH:MM`,
/// or naive local). Naive values are interpreted in local time.
pub fn parse_iso(s: &str) -> Option<DateTime<FixedOffset>> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt);
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Local
                .from_local_datetime(&naive)
                .earliest()
                .map(|d| d.fixed_offset());
        }
    }
    None
}

/// Python `datetime.fromisoformat(ts.replace('Z','+00:00')).strftime('%Y-%m-%d %H:%M:%S')`:
/// wall-clock time in the timestamp's own offset; unparsable input is returned as-is.
pub fn display_in_own_offset(ts: &str) -> String {
    parse_iso(ts)
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ts.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn fraction_only_when_nonzero() {
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert!(!iso_local_naive(t).contains('.'));
        let t2 = t + Duration::from_micros(322_000);
        assert!(iso_local_naive(t2).ends_with(".322000"));
        let with_off = iso_local_offset(t2);
        assert!(with_off.contains(".322000"));
        // ...T..:..:...322000±HH:MM
        assert_eq!(with_off.len(), "2023-11-14T22:13:20.322000+00:00".len());
    }

    #[test]
    fn parse_and_display() {
        assert_eq!(
            display_in_own_offset("2026-10-01T10:00:00.123Z"),
            "2026-10-01 10:00:00"
        );
        assert_eq!(
            display_in_own_offset("2026-10-01T10:00:00-04:00"),
            "2026-10-01 10:00:00"
        );
        assert_eq!(display_in_own_offset("garbage"), "garbage");
        assert!(parse_iso("2026-10-01T10:00:00").is_some());
    }
}
