use super::*;

#[test]
fn quota_observations_keep_independent_ages_and_truthful_reset_labels() {
    let now = DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let expired = AccountRateLimitWindow {
        used_percent: 37.0,
        resets_at: Some(now - chrono::Duration::minutes(1)),
        window_minutes: Some(300),
    };
    let idle = AccountRateLimitWindow {
        used_percent: 0.0,
        resets_at: Some(now + chrono::Duration::hours(5)),
        ..expired
    };
    let expired_idle = AccountRateLimitWindow {
        resets_at: Some(now - chrono::Duration::minutes(1)),
        ..idle
    };
    let custom_idle = AccountRateLimitWindow {
        resets_at: Some(now + chrono::Duration::hours(1)),
        window_minutes: Some(60),
        ..idle
    };
    insta::assert_snapshot!(format!(
        "Primary observed: {}\nSecondary observed: {}\nExpired reset: {}\nIdle reset: {}\nExpired idle reset: {}\nCustom idle reset: {}",
        observed_at(Some(now.timestamp() - 300), now),
        observed_at(Some(now.timestamp() - 3 * 86400), now),
        reset(Some(&expired), QuotaWindow::Primary, now),
        reset(Some(&idle), QuotaWindow::Primary, now),
        reset(Some(&expired_idle), QuotaWindow::Primary, now),
        reset(Some(&custom_idle), QuotaWindow::Primary, now),
    ), @"
    Primary observed: 2026-01-01 11:55 UTC (5m ago)
    Secondary observed: 2025-12-29 12:00 UTC (3d ago)
    Expired reset: passed; awaiting refresh
    Idle reset: start unconfirmed
    Expired idle reset: passed; awaiting refresh
    Custom idle reset: in 1:00
    ");
}
