use chrono::Duration;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn late_same_window_quota_cannot_replace_usage_or_omit_weekly_quota() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 99.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        secondary: Some(AccountRateLimitWindow {
            used_percent: 65.0,
            resets_at: Some(now + Duration::days(2)),
            window_minutes: Some(10080),
        }),
        observed_at: Some(now),
    };
    let incoming = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 10.0,
            resets_at: existing
                .primary
                .as_ref()
                .and_then(|window| window.resets_at),
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(now + Duration::seconds(1)),
    };
    assert_eq!(
        merge_rate_limits_monotonic(&existing, incoming),
        AccountRateLimits {
            observed_at: Some(now + Duration::seconds(1)),
            ..existing.clone()
        }
    );
}

#[test]
fn quota_decrease_is_allowed_when_backend_reports_a_new_window() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 100.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now),
        ..AccountRateLimits::default()
    };
    let incoming = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 0.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now + Duration::seconds(1)),
        ..AccountRateLimits::default()
    };
    assert_eq!(
        merge_rate_limits_monotonic(&existing, incoming.clone()),
        incoming
    );
}

#[test]
fn old_probe_cannot_overwrite_a_more_recent_window() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 99.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now),
        ..AccountRateLimits::default()
    };
    let incoming = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 1.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now - Duration::seconds(1)),
        ..AccountRateLimits::default()
    };
    assert_eq!(merge_rate_limits_monotonic(&existing, incoming), existing);
}

#[test]
fn late_observation_of_an_older_window_preserves_the_current_window() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 75.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now),
        ..AccountRateLimits::default()
    };
    let incoming = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 1.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now + Duration::seconds(1)),
        ..AccountRateLimits::default()
    };
    assert_eq!(
        merge_rate_limits_monotonic(&existing, incoming),
        AccountRateLimits {
            observed_at: Some(now + Duration::seconds(1)),
            ..existing
        }
    );
}
