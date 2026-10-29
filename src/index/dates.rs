//! `--after` / `--before` parsing and comparison.
//!
//! Date formats with an optional time suffix (`T16:45`, ` 16:45`,
//! `T16:45:30`). Both forms compare against `modified` in local time (a
//! date alone by local calendar day, not by the timestamp's own offset).

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime};

use crate::session::timefmt::parse_iso;

/// A parsed date filter bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateBound {
    Day(NaiveDate),
    At(NaiveDateTime),
}

const DATE_FORMATS: &[&str] = &[
    "%Y%m%d", "%Y-%m-%d", "%m/%d/%y", "%m-%d-%y", "%m/%d/%Y", "%m-%d-%Y", "%Y/%m/%d",
];

fn parse_date(s: &str, today: NaiveDate) -> Option<NaiveDate> {
    if let Some(d) = DATE_FORMATS.iter().find_map(|f| {
        NaiveDate::parse_from_str(s, f)
            .ok()
            .filter(|d| d.year() >= 1000)
    }) {
        return Some(d);
    }
    let (m, d) = s.split_once(['/', '-'])?;
    let all_digits =
        |p: &str| !p.is_empty() && p.len() <= 2 && p.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(m) || !all_digits(d) {
        return None;
    }
    NaiveDate::from_ymd_opt(today.year(), m.parse().ok()?, d.parse().ok()?)
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    ["%H:%M", "%H:%M:%S"]
        .iter()
        .find_map(|f| NaiveTime::parse_from_str(s, f).ok())
}

/// Parse a flexible date (optionally with a time); `today` supplies the
/// year for `MM/DD`.
pub fn parse_flexible_date(input: &str, today: NaiveDate) -> Option<DateBound> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(d) = parse_date(s, today) {
        return Some(DateBound::Day(d));
    }
    let (date, time) = s.rsplit_once(['T', 't', ' '])?;
    let d = parse_date(date.trim(), today)?;
    Some(DateBound::At(d.and_time(parse_time(time.trim())?)))
}

/// Parse relative to the local current date.
pub fn parse_date_now(input: &str) -> Option<DateBound> {
    parse_flexible_date(input, Local::now().date_naive())
}

impl DateBound {
    /// `MM/DD/YY` or `MM/DD/YY HH:MM` (status-bar chips).
    pub fn display(&self) -> String {
        match self {
            DateBound::Day(d) => d.format("%m/%d/%y").to_string(),
            DateBound::At(t) => t.format("%m/%d/%y %H:%M").to_string(),
        }
    }

    /// Text that re-parses to this bound (filter-state round trip).
    pub fn to_input(&self) -> String {
        match self {
            DateBound::Day(d) => d.format("%Y-%m-%d").to_string(),
            DateBound::At(t) => t.format("%Y-%m-%dT%H:%M:%S").to_string(),
        }
    }

    /// `modified >= self` (inclusive). Unparseable timestamps pass.
    pub fn allows_after(&self, modified: &str) -> bool {
        self.compare(modified).is_none_or(|o| o.is_ge())
    }

    /// `modified <= self` (inclusive). Unparseable timestamps pass.
    pub fn allows_before(&self, modified: &str) -> bool {
        self.compare(modified).is_none_or(|o| o.is_le())
    }

    fn compare(&self, modified: &str) -> Option<std::cmp::Ordering> {
        let ts = parse_iso(modified);
        match self {
            DateBound::Day(d) => {
                let day = match ts {
                    Some(t) => t.with_timezone(&Local).date_naive(),
                    None => NaiveDate::parse_from_str(modified.get(..10)?, "%Y-%m-%d").ok()?,
                };
                Some(day.cmp(d))
            }
            DateBound::At(t) => Some(ts?.with_timezone(&Local).naive_local().cmp(t)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn formats() {
        let today = d(2026, 10, 29);
        let day = Some(DateBound::Day(d(2026, 10, 1)));
        for s in [
            "20261001",
            "2026-10-01",
            "10/01/26",
            "10-01-26",
            "10/01/2026",
            "10-01-2026",
            "2026/10/01",
            "10/01",
            "10-1",
        ] {
            assert_eq!(parse_flexible_date(s, today), day, "{s}");
        }
        let at = d(2026, 10, 1).and_hms_opt(16, 45, 0).unwrap();
        for s in [
            "2026-10-01T16:45",
            "2026-10-01 16:45",
            "10/01/26 16:45",
            "20261001T16:45:00",
        ] {
            assert_eq!(
                parse_flexible_date(s, today),
                Some(DateBound::At(at)),
                "{s}"
            );
        }
        for s in ["", "yesterday", "2026-13-01", "10/01 25:00", "1/2/3/4"] {
            assert_eq!(parse_flexible_date(s, today), None, "{s}");
        }
    }

    #[test]
    fn comparisons() {
        let local = |t: NaiveDateTime| {
            t.and_local_timezone(Local)
                .unwrap()
                .with_timezone(&chrono::Utc)
                .to_rfc3339()
        };
        let hms = |day: u32, h: u32, m: u32| d(2026, 10, day).and_hms_opt(h, m, 0).unwrap();
        // Local calendar day, whatever offset the timestamp is written in.
        let b = DateBound::Day(d(2026, 10, 1));
        assert!(b.allows_after(&local(hms(1, 0, 0))));
        assert!(!b.allows_after(&local(hms(30, 23, 59) - chrono::Duration::days(30))));
        assert!(b.allows_before(&local(hms(1, 23, 59))));
        assert!(!b.allows_before(&local(hms(2, 0, 0))));
        assert!(b.allows_after("garbage"));
        // Date-only and midnight date-time agree.
        let midnight = DateBound::At(hms(1, 0, 0));
        for t in [hms(1, 0, 0), hms(1, 2, 0)] {
            assert_eq!(b.allows_after(&local(t)), midnight.allows_after(&local(t)));
        }
        let at = hms(1, 16, 45);
        let b = DateBound::At(at);
        assert!(b.allows_after(&local(at)));
        assert!(!b.allows_after(&local(at - chrono::Duration::minutes(1))));
        assert!(b.allows_before(&local(at)));
        assert!(!b.allows_before(&local(at + chrono::Duration::minutes(1))));
        assert_eq!(b.display(), "10/01/26 16:45");
        assert_eq!(parse_flexible_date(&b.to_input(), d(2000, 1, 1)), Some(b));
    }
}
