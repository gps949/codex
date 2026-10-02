use chrono::DateTime;
use chrono::Utc;
use codex_login::AccountRateLimitWindow;
use codex_login::format_primary_window_reset;
use codex_login::format_relative_reset;

pub(super) enum QuotaWindow {
    Primary,
    Secondary,
}

pub(super) fn observed_at(value: Option<i64>, now: DateTime<Utc>) -> String {
    let Some(observed_at) =
        value.and_then(|value| DateTime::<Utc>::from_timestamp(value, /*nsecs*/ 0))
    else {
        return "unknown".into();
    };
    let absolute = observed_at.format("%Y-%m-%d %H:%M UTC");
    if observed_at > now {
        return format!("{absolute} (clock skew)");
    }
    let minutes = now.signed_duration_since(observed_at).num_minutes();
    let age = match minutes {
        0..=59 => format!("{minutes}m"),
        60..=1439 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    };
    format!("{absolute} ({age} ago)")
}

pub(super) fn reset(
    window: Option<&AccountRateLimitWindow>,
    kind: QuotaWindow,
    now: DateTime<Utc>,
) -> String {
    let Some(window) = window else {
        return "unknown".into();
    };
    if matches!(kind, QuotaWindow::Primary) && window.used_percent <= 0.0 {
        return format_primary_window_reset(
            window.used_percent,
            window.resets_at.map(|at| at.timestamp()),
            now,
        );
    }
    match window.resets_at {
        Some(reset) if reset <= now => "passed; awaiting refresh".into(),
        Some(reset) => format_relative_reset(reset, now),
        None => "unknown".into(),
    }
}

#[cfg(test)]
#[path = "quota_display_tests.rs"]
mod tests;
