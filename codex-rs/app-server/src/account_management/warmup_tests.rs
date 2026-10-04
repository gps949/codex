use super::*;
use codex_login::AccountRateLimitWindow;
use pretty_assertions::assert_eq;

#[test]
fn completion_and_zero_usage_do_not_claim_window_start() {
    let now = Utc::now();
    let observation = WindowWarmupObservation::current(
        WindowWarmupOutcome::Succeeded,
        now - Duration::minutes(1),
        /*retry_after*/ None,
        /*consecutive_failures*/ 0,
    );
    let limits = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 0.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now),
        ..Default::default()
    };
    assert_eq!(
        view(&observation, &limits, now).status,
        "completedUnconfirmed"
    );
}

#[test]
fn active_quota_supersedes_old_failure_but_expired_evidence_is_marked() {
    let now = Utc::now();
    let observation = WindowWarmupObservation::current(
        WindowWarmupOutcome::Failed,
        now - Duration::minutes(1),
        Some(now + Duration::minutes(1)),
        /*consecutive_failures*/ 1,
    );
    let limits = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 0.2,
            resets_at: Some(now + Duration::hours(4)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now),
        ..Default::default()
    };
    assert_eq!(view(&observation, &limits, now).status, "windowActive");
    assert_eq!(
        view(&observation, &limits, now + Duration::hours(5)).status,
        "expired"
    );
}
