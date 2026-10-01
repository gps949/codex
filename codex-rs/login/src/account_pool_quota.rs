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
        primary: merge_window(existing.primary.as_ref(), incoming.primary),
        secondary: merge_window(existing.secondary.as_ref(), incoming.secondary),
        observed_at: incoming.observed_at.or(existing.observed_at),
    }
}

fn merge_window(
    existing: Option<&AccountRateLimitWindow>,
    incoming: Option<AccountRateLimitWindow>,
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
    if matches!(
        (existing.resets_at, incoming.resets_at),
        (Some(previous), Some(current)) if current < previous
    ) || matches!(
        (existing.window_minutes, incoming.window_minutes),
        (Some(previous), Some(current)) if previous != current
    ) {
        return Some(existing.clone());
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
