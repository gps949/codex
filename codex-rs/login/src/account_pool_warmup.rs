//! Fair, window-scoped scheduling of identity-preserving standby warmup.

use chrono::DateTime;
use chrono::Utc;

use super::AccountRateLimits;
use super::CURRENT_WARMUP_REQUEST_GENERATION;
use super::ManagedAccount;
use super::WindowWarmupObservation;
use super::WindowWarmupOutcome;

fn warmup_success_is_fresh(
    observation: &WindowWarmupObservation,
    rate_limits: &AccountRateLimits,
    now: DateTime<Utc>,
) -> bool {
    observation.outcome == WindowWarmupOutcome::Succeeded
        && now - observation.attempted_at < chrono::Duration::minutes(FIVE_HOUR_WINDOW_MINUTES)
        && rate_limits
            .primary
            .as_ref()
            .and_then(|window| window.resets_at)
            .is_none_or(|reset| reset > now)
}

pub(super) fn standby_needs_window_warmup(account: &ManagedAccount, now: DateTime<Utc>) -> bool {
    if let Some(window) = account.rate_limits.primary.as_ref()
        && (window
            .window_minutes
            .is_some_and(|minutes| minutes != FIVE_HOUR_WINDOW_MINUTES)
            || (window.used_percent > 0.0 && window.resets_at.is_none_or(|reset| reset > now)))
    {
        return false;
    }
    if let Some(window) = account.rate_limits.secondary.as_ref()
        && window.used_percent >= 100.0
        && window.resets_at.is_none_or(|reset| reset > now)
    {
        return false;
    }
    account.window_warmup.as_ref().is_none_or(|observation| {
        observation.request_generation != CURRENT_WARMUP_REQUEST_GENERATION
            || (!warmup_success_is_fresh(observation, &account.rate_limits, now)
                && observation.retry_after.is_none_or(|retry| retry <= now))
    })
}

pub(super) fn clear_started_window_warmup(account: &mut ManagedAccount) -> bool {
    let now = Utc::now();
    let preserve = account.window_warmup.as_ref().is_none_or(|observation| {
        if observation.outcome == WindowWarmupOutcome::Succeeded {
            warmup_success_is_fresh(observation, &account.rate_limits, now)
        } else {
            !account.rate_limits.primary.as_ref().is_some_and(|window| {
                window.used_percent > 0.0 && window.resets_at.is_none_or(|reset| reset > now)
            })
        }
    });
    !preserve && account.window_warmup.take().is_some()
}

const FIVE_HOUR_WINDOW_MINUTES: i64 = 300;
