use chrono::DateTime;
use chrono::Utc;

/// Formats an exhausted-until / reset timestamp for CLI-style surfaces.
///
/// Prefer [`format_primary_window_reset`] when rendering primary quota windows so idle
/// `0%` accounts are not shown with a fake countdown.
pub fn format_exhausted_reset(reset: DateTime<Utc>) -> String {
    let absolute = reset.format("%Y-%m-%d %H:%M UTC");
    let remaining = reset.signed_duration_since(Utc::now());
    if remaining.num_seconds() <= 0 {
        return absolute.to_string();
    }

    format!(
        "{absolute} (in {})",
        format_reset_countdown(remaining.num_seconds() as u64)
    )
}

pub fn format_exhausted_reset_unix(unix: i64) -> String {
    match DateTime::<Utc>::from_timestamp(unix, 0) {
        Some(reset) => format_exhausted_reset(reset),
        None => format!("unix:{unix}"),
    }
}

/// Compact remaining-time countdown: `H:MM`, or `D:HH:MM` when ≥ 24h.
pub fn format_reset_countdown(remaining_seconds: u64) -> String {
    let total_minutes = remaining_seconds.div_ceil(60).max(1);
    let days = total_minutes / (24 * 60);
    let hours = (total_minutes / 60) % 24;
    let minutes = total_minutes % 60;
    if days > 0 {
        format!("{days}:{hours:02}:{minutes:02}")
    } else {
        format!("{hours}:{minutes:02}")
    }
}

/// Formats the primary (5h) window reset line.
///
/// Idle accounts at ≤0% used have not started their window yet, so we avoid showing a
/// fake ~5h countdown and instead say the window is not started.
pub fn format_primary_window_reset(
    used_percent: f64,
    reset_at: Option<i64>,
    now: DateTime<Utc>,
) -> String {
    if used_percent <= 0.0 {
        return "not started".to_string();
    }

    let Some(reset_at) = reset_at else {
        return "unknown".to_string();
    };
    let remaining_seconds = reset_at - now.timestamp();
    if remaining_seconds <= 0 {
        return "due".to_string();
    }

    format!("in {}", format_reset_countdown(remaining_seconds as u64))
}

/// Relative-only exhausted/cooldown countdown for compact UIs.
pub fn format_relative_reset(reset: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let remaining_seconds = reset.signed_duration_since(now).num_seconds();
    if remaining_seconds <= 0 {
        return "due".to_string();
    }
    format!("in {}", format_reset_countdown(remaining_seconds as u64))
}

pub fn format_plan_type_label(plan_type: Option<&str>) -> String {
    match plan_type {
        Some(plan) if !plan.trim().is_empty() => plan.to_string(),
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    #[test]
    fn format_exhausted_reset_includes_absolute_and_relative() {
        let reset = Utc.with_ymd_and_hms(2099, 1, 2, 3, 4, 0).unwrap();
        let formatted = format_exhausted_reset(reset);
        assert!(formatted.starts_with("2099-01-02 03:04 UTC (in "));
        assert!(formatted.ends_with(')'));
    }

    #[test]
    fn format_primary_window_reset_marks_idle_zero_percent() {
        let now = Utc.with_ymd_and_hms(2026, 3, 17, 12, 0, 0).unwrap();
        assert_eq!(
            format_primary_window_reset(0.0, Some(now.timestamp() + 5 * 3600), now),
            "not started"
        );
        assert_eq!(
            format_primary_window_reset(38.0, Some(now.timestamp() + 3 * 3600 + 46 * 60), now),
            "in 3:46"
        );
        assert_eq!(format_primary_window_reset(10.0, None, now), "unknown");
    }

    #[test]
    fn format_reset_countdown_uses_colon_style() {
        assert_eq!(format_reset_countdown(/*remaining_seconds*/ 1), "0:01");
        assert_eq!(
            format_reset_countdown(/*remaining_seconds*/ 3 * 60 * 60 + 46 * 60),
            "3:46"
        );
        assert_eq!(
            format_reset_countdown(
                /*remaining_seconds*/ 3 * 24 * 60 * 60 + 21 * 60 * 60 + 2 * 60
            ),
            "3:21:02"
        );
    }
}
