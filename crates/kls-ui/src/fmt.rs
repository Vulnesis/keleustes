//! Display formatting helpers.

/// Local wall-clock time for a Unix-ms timestamp.
pub fn when(ms: i64) -> String {
    match jiff::Timestamp::from_millisecond(ms) {
        Ok(ts) => ts
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => "?".into(),
    }
}

/// "12s ago", "5m ago", "3h ago", "2d ago".
pub fn ago(now_ms: i64, ms: i64) -> String {
    let s = ((now_ms - ms) / 1000).max(0);
    match s {
        0..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ago() {
        assert_eq!(super::ago(10_000, 5_000), "5s ago");
        assert_eq!(super::ago(0, 5_000), "0s ago");
        assert_eq!(super::ago(7_200_000, 0), "2h ago");
    }
}
