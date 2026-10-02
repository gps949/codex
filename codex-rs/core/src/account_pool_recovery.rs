//! Cancellation-aware waiting for a pooled account's natural quota recovery.

use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::execution_auth::ExecutionAuth;
use crate::failover_checkpoint::FailoverRetryMode;
use crate::failover_turn::earliest_exhausted_reset;

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
    budget: &RecoveryWaitBudget,
    cancellation: &CancellationToken,
) -> bool {
    if cancellation.is_cancelled() || budget.remaining().is_zero() {
        return false;
    }
    if execution_auth.active_lease().is_some() {
        return true;
    }
    if earliest_exhausted_reset(execution_auth).is_none() {
        return false;
    }
    let waiting = budget.begin_wait();
    let deadline = waiting.started + waiting.allowance;
    let mut changes = execution_auth.active_auth_change_receiver();
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
        let delay = earliest_exhausted_reset(execution_auth)
            .map(|reset| {
                (reset - Utc::now())
                    .to_std()
                    .unwrap_or(Duration::from_secs(1))
            })
            .unwrap_or(Duration::from_secs(30))
            .clamp(Duration::from_secs(1), Duration::from_secs(30));
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
