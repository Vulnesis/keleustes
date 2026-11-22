//! Cron expressions: five fields (minute hour day-of-month month
//! day-of-week) evaluated in an IANA time zone, with croner and jiff.

use croner::Cron;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::str::FromStr;

/// Why an expression or zone is refused (the validator's codes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CronError {
    /// Not a cron expression.
    Invalid,
    /// Both day of month and day of week are restricted.
    Unsupported,
    /// A seconds field (finer than one minute).
    Granularity,
}

impl CronError {
    pub fn code(self) -> &'static str {
        match self {
            CronError::Invalid => "cron_invalid",
            CronError::Unsupported => "cron_unsupported",
            CronError::Granularity => "cron_granularity",
        }
    }
}

/// Parses a five-field expression (or a nickname such as `@daily`).
pub fn parse(expr: &str) -> Result<Cron, CronError> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err(CronError::Invalid);
    }
    if !expr.starts_with('@') {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        match fields.len() {
            5 => {}
            6 | 7 => return Err(CronError::Granularity),
            _ => return Err(CronError::Invalid),
        }
        let restricted = |f: &str| f != "*" && f != "?";
        if restricted(fields[2]) && restricted(fields[4]) {
            return Err(CronError::Unsupported);
        }
    }
    Cron::from_str(expr).map_err(|_| CronError::Invalid)
}

/// The zone, or `None` when it is not an IANA name the tz database has.
pub fn zone(tz: &str) -> Option<TimeZone> {
    let tz = if tz.trim().is_empty() {
        "UTC"
    } else {
        tz.trim()
    };
    TimeZone::get(tz).ok()
}

/// The first fire strictly after `after_ms`.
pub fn next_after(expr: &str, tz: &str, after_ms: i64) -> Option<i64> {
    let cron = parse(expr).ok()?;
    let zone = zone(tz)?;
    next_with(&cron, &zone, after_ms)
}

fn next_with(cron: &Cron, zone: &TimeZone, after_ms: i64) -> Option<i64> {
    let start = Timestamp::from_millisecond(after_ms)
        .ok()?
        .to_zoned(zone.clone());
    let next = cron.find_next_occurrence(&start, false).ok()?;
    let ms = next.timestamp().as_millisecond();
    // Croner works in whole seconds; never return the start itself.
    if ms <= after_ms {
        let again = Timestamp::from_millisecond(after_ms + 1000)
            .ok()?
            .to_zoned(zone.clone());
        return Some(
            cron.find_next_occurrence(&again, false)
                .ok()?
                .timestamp()
                .as_millisecond(),
        );
    }
    Some(ms)
}

/// The next `n` fires after `after_ms`.
pub fn upcoming(expr: &str, tz: &str, after_ms: i64, n: usize) -> Vec<i64> {
    let (Ok(cron), Some(zone)) = (parse(expr), zone(tz)) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(n);
    let mut at = after_ms;
    while out.len() < n {
        match next_with(&cron, &zone, at) {
            Some(t) => {
                out.push(t);
                at = t;
            }
            None => break,
        }
    }
    out
}

/// Fires in `(from_ms, to_ms]`, at most `cap` (the earliest ones).
pub fn between(expr: &str, tz: &str, from_ms: i64, to_ms: i64, cap: usize) -> Vec<i64> {
    let (Ok(cron), Some(zone)) = (parse(expr), zone(tz)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut at = from_ms;
    while out.len() < cap {
        match next_with(&cron, &zone, at) {
            Some(t) if t <= to_ms => {
                out.push(t);
                at = t;
            }
            _ => break,
        }
    }
    out
}

/// "At 03:00 on Monday through Friday (Australia/Melbourne)", or why the
/// expression cannot be read.
pub fn describe(expr: &str, tz: &str) -> String {
    let tz = if tz.trim().is_empty() {
        "UTC"
    } else {
        tz.trim()
    };
    match parse(expr) {
        Ok(c) => format!("{} ({tz})", c.describe()),
        Err(CronError::Granularity) => "Not valid: a schedule has no seconds field".into(),
        Err(CronError::Unsupported) => {
            "Not valid: set a day of the month or a day of the week, not both".into()
        }
        Err(CronError::Invalid) => format!("Not a valid schedule: {expr}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> i64 {
        s.parse::<Timestamp>().unwrap().as_millisecond()
    }

    #[test]
    fn checks() {
        assert!(parse("0 3 * * 1-5").is_ok());
        assert!(parse("*/15 * * * *").is_ok());
        assert!(parse("0 7 * * FRI").is_ok());
        assert!(parse("@daily").is_ok());
        assert_eq!(parse("").unwrap_err(), CronError::Invalid);
        assert_eq!(parse("0 3 * *").unwrap_err(), CronError::Invalid);
        assert_eq!(parse("61 3 * * *").unwrap_err(), CronError::Invalid);
        assert_eq!(parse("0 0 3 * * *").unwrap_err(), CronError::Granularity);
        assert_eq!(parse("0 3 1 * 1").unwrap_err(), CronError::Unsupported);
        assert!(zone("Australia/Melbourne").is_some());
        assert!(zone("").is_some());
        assert!(zone("Mars/Olympus").is_none());
    }

    #[test]
    fn next_fire_in_zone() {
        // 2026-11-20 is a Friday. 03:00 Melbourne (UTC+11 in summer) is 16:00 UTC the day before.
        let after = ms("2026-11-20T12:00:00Z");
        let next = next_after("0 3 * * 1-5", "Australia/Melbourne", after).unwrap();
        // Saturday 03:00 local is skipped; Monday 03:00 local = Sunday 16:00 UTC.
        assert_eq!(next, ms("2026-11-22T16:00:00Z"));
        let utc = next_after("0 3 * * *", "UTC", after).unwrap();
        assert_eq!(utc, ms("2026-11-21T03:00:00Z"));
        // Strictly after: a start exactly on a fire returns the following one.
        assert_eq!(
            next_after("0 3 * * *", "UTC", utc).unwrap(),
            ms("2026-11-22T03:00:00Z")
        );
        assert_eq!(next_after("0 3 * * *", "Nope/Zone", after), None);
    }

    #[test]
    fn upcoming_and_between() {
        let after = ms("2026-11-21T00:00:00Z");
        let u = upcoming("*/15 * * * *", "UTC", after, 3);
        assert_eq!(
            u,
            vec![
                ms("2026-11-21T00:15:00Z"),
                ms("2026-11-21T00:30:00Z"),
                ms("2026-11-21T00:45:00Z")
            ]
        );
        let b = between("0 * * * *", "UTC", after, ms("2026-11-21T05:30:00Z"), 100);
        assert_eq!(b.len(), 5);
        assert_eq!(b[0], ms("2026-11-21T01:00:00Z"));
        let capped = between("0 * * * *", "UTC", after, ms("2026-11-22T00:00:00Z"), 3);
        assert_eq!(capped.len(), 3);
    }

    #[test]
    fn dst_change() {
        // Melbourne leaves DST on 2026-04-05 at 03:00 local (back to 02:00).
        let after = ms("2026-04-04T00:00:00Z");
        let next = next_after("30 9 * * *", "Australia/Melbourne", after).unwrap();
        // 2026-04-04T00:00Z is 11:00 local; the next 09:30 is on 2026-04-05,
        // after the change, so UTC+10.
        assert_eq!(next, ms("2026-04-04T23:30:00Z"));
        let after2 = ms("2026-04-05T12:00:00Z");
        let next2 = next_after("30 9 * * *", "Australia/Melbourne", after2).unwrap();
        // 2026-04-06 09:30 local is UTC+10.
        assert_eq!(next2, ms("2026-04-05T23:30:00Z"));
    }

    #[test]
    fn describes() {
        assert!(describe("0 3 * * 1-5", "Australia/Melbourne").ends_with("(Australia/Melbourne)"));
        assert!(describe("0 3 1 * 1", "UTC").starts_with("Not valid"));
    }
}
