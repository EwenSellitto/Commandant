//! Unix timestamps, as stored in the database and sent over the wire.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Human-friendly age of a timestamp: `12s ago`, `3h ago`...
pub fn ago(ts: i64) -> String {
    match (now() - ts).max(0) {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

/// A lifetime as typed: `90` or `90s`, `30m`, `1h`, `7d`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let unit_start = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (amount, unit) = s.split_at(unit_start);
    let amount: u64 = amount
        .parse()
        .map_err(|_| format!("invalid duration {s:?}"))?;
    let seconds_per_unit = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err(format!("invalid duration unit {unit:?} (use s, m, h or d)")),
    };
    Ok(Duration::from_secs(amount * seconds_per_unit))
}

/// A lifetime as [`parse_duration`] reads it, in its largest whole unit.
pub fn duration(d: Duration) -> String {
    match d.as_secs() {
        s if s > 0 && s % 86400 == 0 => format!("{}d", s / 86400),
        s if s > 0 && s % 3600 == 0 => format!("{}h", s / 3600),
        s if s > 0 && s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("90"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_duration("7d"), Ok(Duration::from_secs(7 * 86400)));
        assert!(parse_duration("1w").is_err());
        assert!(parse_duration("h").is_err());
    }

    #[test]
    fn durations_read_back_in_their_largest_unit() {
        for typed in ["90s", "30m", "1h", "36h", "7d"] {
            assert_eq!(duration(parse_duration(typed).unwrap()), typed);
        }
        assert_eq!(duration(Duration::from_secs(3600 + 60)), "61m");
    }
}
