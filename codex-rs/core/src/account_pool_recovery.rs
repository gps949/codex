//! Cancellation-aware waiting for a pooled account's natural quota recovery.

use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::execution_auth::ExecutionAuth;
use crate::failover_checkpoint::FailoverRetryMode;
use crate::failover_turn::earliest_exhausted_reset;

#[path = "account_pool_metadata_recovery.rs"]
mod metadata;
pub(crate) use metadata::SpendingRecoveryCoverage;
pub(crate) use metadata::coverage_for_spending;
pub(crate) use metadata::probe_for_recovery;

/// Cumulative waiting allowance shared by every sampling step of one user turn.
pub(crate) struct RecoveryWaitBudget {
    remaining: Mutex<Duration>,
}

impl RecoveryWaitBudget {
    pub(crate) fn new(max_wait: Duration) -> Self {
        Self {
            remaining: Mutex::new(max_wait),
        }
    }

    pub(crate) fn remaining(&self) -> Duration {
        *self
            .remaining
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn begin_wait(&self) -> RecoveryWait<'_> {
        let allowance = std::mem::take(
            &mut *self
                .remaining
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        RecoveryWait {
            budget: self,
            allowance,
            started: Instant::now(),
        }
    }
}

/// Reserve the allowance until completion, including cancellation by dropping the future.
struct RecoveryWait<'a> {
    budget: &'a RecoveryWaitBudget,
    allowance: Duration,
    started: Instant,
}

impl Drop for RecoveryWait<'_> {
    fn drop(&mut self) {
        let unspent = self.allowance.saturating_sub(self.started.elapsed());
        let mut remaining = self
            .budget
            .remaining
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *remaining = remaining.saturating_add(unspent);
    }
}

pub(crate) fn can_continue(retry_mode: FailoverRetryMode) -> bool {
    matches!(
        retry_mode,
        FailoverRetryMode::ReplayCurrentSamplingRequest
            | FailoverRetryMode::ContinueFromDurableHistory
    )
}

pub(crate) async fn wait_for_recovery(
    execution_auth: &ExecutionAuth,
    config: &Config,
    budget: &RecoveryWaitBudget,
    cancellation: &CancellationToken,
) -> bool {
    if cancellation.is_cancelled() || budget.remaining().is_zero() {
        return false;
    }
    if execution_auth.active_lease().is_some() {
        return true;
    }
    if execution_auth.account_pool().is_none_or(|pool| {
        !pool.snapshots().iter().any(|snapshot| {
            matches!(
                snapshot.availability,
                codex_login::AccountAvailability::Exhausted { .. }
            )
        })
    }) {
        return false;
    }
    let waiting = budget.begin_wait();
    let deadline = waiting.started + waiting.allowance;
    let mut changes = execution_auth.active_auth_change_receiver();
    let mut next_probe = Instant::now();
    loop {
        if cancellation.is_cancelled() {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        if execution_auth.active_lease().is_some() {
            return true;
        }
        if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home)
            || execution_auth.account_pool().is_none_or(|pool| {
                !pool.snapshots().iter().any(|snapshot| {
                    !snapshot.profile.disabled
                        && matches!(
                            snapshot.availability,
                            codex_login::AccountAvailability::Exhausted { .. }
                        )
                })
            })
        {
            return false;
        }
        if now >= next_probe {
            let outcome = tokio::select! {
                _ = cancellation.cancelled() => return false,
                _ = tokio::time::sleep_until(deadline) => return false,
                recovered = metadata::probe_candidates(execution_auth, config, cancellation) => recovered,
            };
            if outcome == metadata::ProbeOutcome::Recovered {
                return true;
            }
            next_probe = Instant::now()
                + if outcome == metadata::ProbeOutcome::Busy {
                    Duration::from_millis(200)
                } else {
                    Duration::from_secs(30)
                };
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let delay = earliest_exhausted_reset(execution_auth)
            .map(|reset| {
                (reset - Utc::now())
                    .to_std()
                    .unwrap_or(Duration::from_secs(1))
            })
            .unwrap_or(Duration::from_secs(30))
            .clamp(Duration::from_secs(1), Duration::from_secs(30))
            .min(next_probe.saturating_duration_since(now));
        tokio::select! {
            _ = cancellation.cancelled() => return false,
            _ = tokio::time::sleep(delay.min(deadline - now)) => {}
            result = changes.changed() => {
                if result.is_err() {
                    return false;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "account_pool_recovery_tests.rs"]
mod tests;
