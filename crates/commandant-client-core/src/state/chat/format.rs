//! Numbers as a turn's summary and the footer show them.

use std::time::Duration;

/// `184`, `12.6k`, `1.2M`.
pub fn count(n: u64) -> String {
    let (value, unit) = match n {
        0..1_000 => return n.to_string(),
        1_000..1_000_000 => (n as f64 / 1e3, "k"),
        _ => (n as f64 / 1e6, "M"),
    };
    let value = format!("{value:.1}");
    format!("{}{unit}", value.trim_end_matches(".0"))
}

/// `$0.0042`, `$1.23`: more digits for small amounts.
pub fn dollars(cost: f64) -> String {
    if cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

/// `4.2s`, `2m 03s`.
pub fn elapsed(took: Duration) -> String {
    let secs = took.as_secs();
    if secs < 60 {
        format!("{:.1}s", took.as_secs_f64())
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}
