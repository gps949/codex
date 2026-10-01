use chrono::DateTime;
use chrono::Utc;

use super::AccountRateLimitWindow;
use super::AccountRateLimits;

/// Fresh 100% quota makes a profile a last-resort probe rather than a useful preemptive
/// replacement. Cached quota never makes it ineligible; missing, stale, future-dated, or
/// already-reset observations must still be allowed to discover recovered headroom.
pub(super) fn has_fresh_exhausted_window(
    rate_limits: &AccountRateLimits,
    now: &DateTime<Utc>,
) -> bool {
    let Some(observed_at) = rate_limits.observed_at else {
        return false;
    };
    if observed_at > *now || *now - observed_at >= chrono::Duration::minutes(30) {
        return false;
    }
    rate_limits
        .primary
        .iter()
        .chain(rate_limits.secondary.iter())
        .any(|window| {
            window.used_percent.is_finite()
                && window.used_percent >= 100.0
                && window.resets_at.is_none_or(|reset| reset > *now)
        })
}

/// Quota GETs and inference responses can arrive out of order and omit a window. Preserve
/// usage within one backend window; accept lower usage only after that window resets.
pub(crate) fn merge_rate_limits_monotonic(
    existing: &AccountRateLimits,
    incoming: AccountRateLimits,
) -> AccountRateLimits {
    if matches!(
        (incoming.observed_at, existing.observed_at),
        (Some(incoming), Some(existing)) if incoming < existing
    ) {
        return existing.clone();
    }
    AccountRateLimits {
        primary: merge_window(
            existing.primary.as_ref(),
            incoming.primary,
            existing.observed_at,
            incoming.observed_at,
        ),
        secondary: merge_window(
            existing.secondary.as_ref(),
            incoming.secondary,
            existing.observed_at,
            incoming.observed_at,
        ),
        observed_at: incoming.observed_at.or(existing.observed_at),
    }
}

fn merge_window(
    existing: Option<&AccountRateLimitWindow>,
    incoming: Option<AccountRateLimitWindow>,
    previous_observed_at: Option<DateTime<Utc>>,
    incoming_observed_at: Option<DateTime<Utc>>,
) -> Option<AccountRateLimitWindow> {
    let Some(mut incoming) = incoming else {
        return existing.cloned();
    };
    if !incoming.used_percent.is_finite() || incoming.used_percent < 0.0 {
        return existing.cloned();
    }
    let Some(existing) = existing else {
        return Some(incoming);
    };
    // Idle backends report a tentative full-window reset. A newer positive observation
    // may move it slightly earlier as the real clock starts; keep that start evidence.
    let now = Utc::now();
    let confirmed_idle_start = existing.used_percent <= 0.0
        && incoming.used_percent > 0.0
        && existing.window_minutes.is_none_or(|minutes| minutes == 300)
        && incoming.window_minutes.is_none_or(|minutes| minutes == 300)
        && matches!((previous_observed_at, incoming_observed_at),
            (Some(previous), Some(current)) if current >= previous && current <= now
                && now - current < chrono::Duration::minutes(30))
        && matches!((existing.resets_at, incoming.resets_at),
            (Some(previous), Some(current)) if current > now
                && current <= now + chrono::Duration::minutes(305)
                && previous <= now + chrono::Duration::minutes(305)
                && previous - current <= chrono::Duration::minutes(5));
    if confirmed_idle_start {
        return Some(incoming);
    }
    if matches!(
        (existing.resets_at, incoming.resets_at),
        (Some(previous), Some(current)) if current < previous
    ) {
        return Some(existing.clone());
    }
    if matches!(
        (existing.window_minutes, incoming.window_minutes),
        (Some(previous), Some(current)) if previous != current
    ) {
        return if incoming_observed_at
            .is_some_and(|current| previous_observed_at.is_none_or(|previous| current > previous))
        {
            Some(incoming)
        } else {
            Some(existing.clone())
        };
    }
    // An old percentage without a reset cannot prove the current window still has usage.
    // Accept a fresh timed observation rather than inventing usage in a newly idle window.
    if existing.resets_at.is_none()
        && incoming.resets_at.is_some()
        && matches!((previous_observed_at, incoming_observed_at),
            (Some(previous), Some(current)) if current - previous >= chrono::Duration::minutes(30))
    {
        return Some(incoming);
    }
    let previous_window_reset = existing.resets_at.is_some_and(|reset| reset <= Utc::now());
    let changed_window = matches!(
        (existing.resets_at, incoming.resets_at),
        (Some(previous), Some(current)) if current > previous
    );
    if !previous_window_reset && !changed_window {
        incoming.used_percent = incoming.used_percent.max(existing.used_percent);
        incoming.resets_at = incoming.resets_at.or(existing.resets_at);
        incoming.window_minutes = incoming.window_minutes.or(existing.window_minutes);
    }
    Some(incoming)
}

#[cfg(test)]
#[path = "account_pool_quota_tests.rs"]
mod tests;
