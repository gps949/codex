use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;

/// Formats a quota cooldown reset for human-facing account-pool status surfaces.
pub fn format_exhausted_reset(reset: DateTime<Utc>) -> String {
    let formatted = reset.format("%Y-%m-%d %H:%M UTC");
    let remaining = reset - Utc::now();
    if remaining <= Duration::zero() {
        return formatted.to_string();
    }
    let hours = remaining.num_hours();
    let minutes = remaining.num_minutes().rem_euclid(60);
    let relative = if hours > 0 {
        format!("in about {hours}h {minutes}m")
    } else {
        format!("in about {minutes}m")
    };
    format!("{formatted} ({relative})")
}

/// Formats a Unix-seconds cooldown reset for app-server account pool payloads.
pub fn format_exhausted_reset_unix(unix: i64) -> String {
    match DateTime::from_timestamp(unix, 0) {
        Some(reset) => format_exhausted_reset(reset),
        None => unix.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn future_reset_includes_relative_hint() {
        let reset = Utc::now() + Duration::hours(2) + Duration::minutes(15);
        let formatted = format_exhausted_reset(reset);
        assert!(formatted.contains("UTC"));
        assert!(formatted.contains("in about"));
        assert!(formatted.contains('h'));
    }
}
