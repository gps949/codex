//! Fair, window-scoped scheduling of identity-preserving standby warmup.

use std::cmp::Ordering;

use chrono::DateTime;
use chrono::Utc;

use super::AccountPool;
use super::AccountPoolState;
use super::AccountProfileId;
use super::AccountRateLimits;
use super::CURRENT_WARMUP_REQUEST_GENERATION;
use super::ManagedAccount;
use super::WindowWarmupObservation;
use super::WindowWarmupOutcome;
use super::WindowWarmupPhase;

const FIVE_HOUR_WINDOW_MINUTES: i64 = 300;
const CONFIRM_INTERRUPTED_AFTER_MINUTES: i64 = 3;
const QUOTA_FRESH_MINUTES: i64 = 30;

impl WindowWarmupObservation {
    /// Records a potentially billable attempt before it starts, including interruption protection.
    pub fn in_progress(attempted_at: DateTime<Utc>) -> Self {
        Self {
            phase: Some(WindowWarmupPhase::InProgress),
            ..Self::current(
                WindowWarmupOutcome::Failed,
                attempted_at,
                Some(attempted_at + chrono::Duration::minutes(FIVE_HOUR_WINDOW_MINUTES)),
                /*consecutive_failures*/ 0,
            )
        }
    }

    /// A generating request ran, but its 5h window has not been confirmed by quota observations.
    pub fn unconfirmed(attempted_at: DateTime<Utc>) -> Self {
        Self {
            phase: Some(WindowWarmupPhase::Unconfirmed),
            ..Self::in_progress(attempted_at)
        }
    }

    /// Imports ma.4's completed-but-unconfirmed record without repeating the generating request.
    pub(crate) fn infer_legacy_phase(&mut self) {
        if self.phase.is_none()
            && self.outcome == WindowWarmupOutcome::Failed
            && self.consecutive_failures == 0
            && self.retry_after
                == Some(self.attempted_at + chrono::Duration::minutes(FIVE_HOUR_WINDOW_MINUTES))
        {
            self.phase = Some(WindowWarmupPhase::Unconfirmed);
        }
    }

    /// Orders phases within one attempt so stale in-flight state cannot replace a final result.
    pub(crate) fn compare_progress(&self, other: &Self) -> Ordering {
        let now = Utc::now();
        // A stale process holding a future-dated attempt must not resurrect it after
        // another process replaces it with a valid, durably claimed attempt.
        (self.attempted_at <= now)
            .cmp(&(other.attempted_at <= now))
            .then_with(|| self.attempted_at.cmp(&other.attempted_at))
            .then_with(|| self.progress_rank().cmp(&other.progress_rank()))
            .then_with(|| self.retry_after.cmp(&other.retry_after))
    }

    fn progress_rank(&self) -> u8 {
        if self.outcome == WindowWarmupOutcome::Succeeded {
            return 3;
        }
        match self.phase {
            Some(WindowWarmupPhase::InProgress) => 0,
            Some(WindowWarmupPhase::Unconfirmed) => 2,
            None => 1,
        }
    }

    fn protected_until(&self, rate_limits: &AccountRateLimits) -> DateTime<Utc> {
        let full_window = self.attempted_at + chrono::Duration::minutes(FIVE_HOUR_WINDOW_MINUTES);
        let deadline = self.retry_after.unwrap_or(full_window).min(full_window);
        // A known reset after this attempt ends its protection early. An expired observation
        // from the previous window must not erase protection for a newly sent request.
        rate_limits
            .primary
            .as_ref()
            .and_then(|window| window.resets_at)
            .filter(|reset| *reset > self.attempted_at)
            .map_or(deadline, |reset| reset.min(deadline))
    }

    pub(crate) fn request_is_protected(
        &self,
        rate_limits: &AccountRateLimits,
        now: DateTime<Utc>,
    ) -> bool {
        self.attempted_at <= now
            && now < self.protected_until(rate_limits)
            && (self.outcome == WindowWarmupOutcome::Succeeded || self.phase.is_some())
    }
}

fn quota_is_fresh(rate_limits: &AccountRateLimits, now: DateTime<Utc>) -> bool {
    rate_limits.primary_observed_at().is_some_and(|observed| {
        observed <= now && now - observed < chrono::Duration::minutes(QUOTA_FRESH_MINUTES)
    })
}

fn primary_is_started(rate_limits: &AccountRateLimits, now: DateTime<Utc>) -> bool {
    rate_limits.primary.as_ref().is_some_and(|window| {
        window.used_percent.is_finite()
            && window.used_percent > 0.0
            && window
                .window_minutes
                .is_none_or(|minutes| minutes == FIVE_HOUR_WINDOW_MINUTES)
            && match window.resets_at {
                Some(reset) => {
                    reset > now
                        && reset <= now + chrono::Duration::minutes(FIVE_HOUR_WINDOW_MINUTES + 5)
                }
                None => quota_is_fresh(rate_limits, now),
            }
    })
}

pub(super) fn standby_needs_window_warmup(account: &ManagedAccount, now: DateTime<Utc>) -> bool {
    if primary_is_started(&account.rate_limits, now) {
        return false;
    }
    if account.rate_limits.primary.as_ref().is_some_and(|window| {
        window.window_minutes.is_some_and(|minutes| {
            minutes != FIVE_HOUR_WINDOW_MINUTES
                && (1..=10_080).contains(&minutes)
                && window.resets_at.is_some_and(|reset| {
                    reset > now && reset <= now + chrono::Duration::minutes(minutes + 5)
                })
        })
    }) {
        return false;
    }
    if (quota_is_fresh(&account.rate_limits, now)
        && account.rate_limits.primary.as_ref().is_some_and(|window| {
            window
                .window_minutes
                .is_some_and(|minutes| minutes != FIVE_HOUR_WINDOW_MINUTES)
                && window.resets_at.is_none_or(|reset| reset > now)
        }))
        || super::has_fresh_exhausted_window(&account.rate_limits, &now)
    {
        return false;
    }
    account.window_warmup.as_ref().is_none_or(|observation| {
        if observation.request_is_protected(&account.rate_limits, now) {
            return false;
        }
        if observation.phase.is_some() || observation.outcome == WindowWarmupOutcome::Succeeded {
            return true;
        }
        observation.request_generation != CURRENT_WARMUP_REQUEST_GENERATION
            || observation.attempted_at > now
            || observation
                .retry_after
                .map(|retry| retry.min(observation.attempted_at + chrono::Duration::minutes(60)))
                .is_none_or(|retry| retry <= now)
    })
}

/// Promotes quota evidence to confirmed while retaining attempt order for fair future scheduling.
pub(super) fn confirm_started_window_warmup(account: &mut ManagedAccount) -> bool {
    let now = Utc::now();
    if !primary_is_started(&account.rate_limits, now) {
        return false;
    }
    let Some(observation) = account.window_warmup.as_mut() else {
        return false;
    };
    if observation.attempted_at > now
        || observation.outcome == WindowWarmupOutcome::Succeeded
        || now - observation.attempted_at >= chrono::Duration::minutes(FIVE_HOUR_WINDOW_MINUTES)
    {
        return false;
    }
    observation.outcome = WindowWarmupOutcome::Succeeded;
    observation.phase = None;
    observation.retry_after = None;
    observation.consecutive_failures = 0;
    true
}

pub(super) fn ordered_warmup_candidates(
    state: &AccountPoolState,
    now: DateTime<Utc>,
    includes: impl Fn(&ManagedAccount, DateTime<Utc>) -> bool,
) -> Vec<AccountProfileId> {
    let mut candidates: Vec<_> = state
        .accounts
        .values()
        .filter(|account| {
            account.availability.is_eligible(&now)
                && state.active_profile.as_ref() != Some(&account.profile.id)
                && includes(account, now)
        })
        .map(|account| {
            (
                account
                    .window_warmup
                    .as_ref()
                    .map(|observation| observation.attempted_at),
                u8::from(account.rate_limits.primary.is_some()),
                account.profile.priority,
                account.profile.id.clone(),
            )
        })
        .collect();
    candidates.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.as_str().cmp(right.3.as_str()))
    });
    candidates.into_iter().map(|(_, _, _, id)| id).collect()
}

impl AccountPool {
    /// Standbys needing a quota-only confirmation probe, with no repeated generating request.
    pub fn window_warmup_confirmation_candidates(&self) -> Vec<AccountProfileId> {
        let state = self.lock_state();
        let now = Utc::now();
        ordered_warmup_candidates(&state, now, |account, now| {
            account.window_warmup.as_ref().is_some_and(|observation| {
                let needs_confirmation = match observation.phase {
                    Some(WindowWarmupPhase::Unconfirmed) => true,
                    Some(WindowWarmupPhase::InProgress) => {
                        now - observation.attempted_at
                            >= chrono::Duration::minutes(CONFIRM_INTERRUPTED_AFTER_MINUTES)
                    }
                    None => false,
                };
                needs_confirmation
                    && observation.request_is_protected(&account.rate_limits, now)
                    && !primary_is_started(&account.rate_limits, now)
            })
        })
    }
}

#[cfg(test)]
#[path = "account_pool_warmup_tests.rs"]
mod tests;
