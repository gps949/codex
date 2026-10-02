use chrono::DateTime;
use chrono::Utc;

use super::AccountRateLimitWindow;
use super::AccountRateLimits;
use super::AccountWindowObservationTimes;

/// Fresh 100% quota makes a profile a last-resort probe rather than a useful preemptive
/// replacement. Cached quota never makes it ineligible; missing, stale, future-dated, or
/// already-reset observations must still be allowed to discover recovered headroom.
pub(super) fn has_fresh_exhausted_window(
    rate_limits: &AccountRateLimits,
    now: &DateTime<Utc>,
) -> bool {
    [
        (
            rate_limits.primary.as_ref(),
            rate_limits.primary_observed_at(),
        ),
        (
            rate_limits.secondary.as_ref(),
            rate_limits.secondary_observed_at(),
        ),
    ]
    .into_iter()
    .any(|(window, observed_at)| {
        observed_at.is_some_and(|observed| {
            observed <= *now && *now - observed < chrono::Duration::minutes(30)
        }) && window.is_some_and(|window| {
            window.used_percent.is_finite()
                && window.used_percent >= 100.0
                && window.resets_at.is_none_or(|reset| reset > *now)
        })
    })
}

/// Quota GETs and inference responses can arrive out of order and omit a window. Preserve
/// usage within one backend window; accept lower usage only after that window resets.
pub fn merge_rate_limits_monotonic(
    existing: &AccountRateLimits,
    incoming: AccountRateLimits,
) -> AccountRateLimits {
    if incoming.primary.is_none() && incoming.secondary.is_none() {
        return existing.clone();
    }
    let primary_observed_at = incoming.primary_observed_at();
    let secondary_observed_at = incoming.secondary_observed_at();
    let (primary, primary_observed_at) = merge_window(
        existing.primary.as_ref(),
        incoming.primary,
        existing.primary_observed_at(),
        primary_observed_at,
    );
    let (secondary, secondary_observed_at) = merge_window(
        existing.secondary.as_ref(),
        incoming.secondary,
        existing.secondary_observed_at(),
        secondary_observed_at,
    );
    let observed_at = existing
        .observed_at
        .into_iter()
        .chain(primary_observed_at)
        .chain(secondary_observed_at)
        .max();
    let shared_time = primary
        .as_ref()
        .is_none_or(|_| primary_observed_at == observed_at)
        && secondary
            .as_ref()
            .is_none_or(|_| secondary_observed_at == observed_at);
    AccountRateLimits {
        primary,
        secondary,
        observed_at,
        window_observed_at: (!shared_time).then_some(AccountWindowObservationTimes {
            primary: primary_observed_at,
            secondary: secondary_observed_at,
        }),
    }
}

fn merge_window(
    existing: Option<&AccountRateLimitWindow>,
    incoming: Option<AccountRateLimitWindow>,
    previous_observed_at: Option<DateTime<Utc>>,
    incoming_observed_at: Option<DateTime<Utc>>,
) -> (Option<AccountRateLimitWindow>, Option<DateTime<Utc>>) {
    let Some(mut incoming) = incoming else {
        return (existing.cloned(), previous_observed_at);
    };
    if !incoming.used_percent.is_finite() || incoming.used_percent < 0.0 {
        return (existing.cloned(), previous_observed_at);
    }
    if previous_observed_at.is_some() && incoming_observed_at.is_none() {
        return (existing.cloned(), previous_observed_at);
    }
    if matches!((previous_observed_at, incoming_observed_at),
        (Some(previous), Some(current)) if current < previous)
    {
        return (existing.cloned(), previous_observed_at);
    }
    let Some(existing) = existing else {
        return (Some(incoming), incoming_observed_at);
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
        return (Some(incoming), incoming_observed_at);
    }
    if matches!(
        (existing.resets_at, incoming.resets_at),
        (Some(previous), Some(current)) if current < previous
    ) {
        return (Some(existing.clone()), previous_observed_at);
    }
    if matches!(
        (existing.window_minutes, incoming.window_minutes),
        (Some(previous), Some(current)) if previous != current
    ) {
        return if incoming_observed_at
            .is_some_and(|current| previous_observed_at.is_none_or(|previous| current > previous))
        {
            (Some(incoming), incoming_observed_at)
        } else {
            (Some(existing.clone()), previous_observed_at)
        };
    }
    // An old percentage without a reset cannot prove the current window still has usage.
    // Accept a fresh timed observation rather than inventing usage in a newly idle window.
    if existing.resets_at.is_none()
        && incoming.resets_at.is_some()
        && matches!((previous_observed_at, incoming_observed_at),
            (Some(previous), Some(current)) if current - previous >= chrono::Duration::minutes(30))
    {
        return (Some(incoming), incoming_observed_at);
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
    (Some(incoming), incoming_observed_at)
}

#[cfg(test)]
#[path = "account_pool_quota_tests.rs"]
mod tests;
