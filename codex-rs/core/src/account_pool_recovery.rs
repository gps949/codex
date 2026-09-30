//! Cancellation-aware waiting for a pooled account's natural quota recovery.

use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;

use crate::execution_auth::ExecutionAuth;
use crate::failover_checkpoint::FailoverRetryMode;
use crate::failover_turn::earliest_exhausted_reset;

pub(crate) fn can_continue(retry_mode: FailoverRetryMode) -> bool {
    matches!(
        retry_mode,
        FailoverRetryMode::ReplayCurrentSamplingRequest
            | FailoverRetryMode::ContinueFromDurableHistory
    )
}

pub(crate) async fn wait_for_recovery(
    execution_auth: &ExecutionAuth,
    max_wait: Duration,
    cancellation: &CancellationToken,
) -> bool {
    if cancellation.is_cancelled() {
        return false;
    }
    if execution_auth.active_lease().is_some() {
        return true;
    }
    if max_wait.is_zero() || earliest_exhausted_reset(execution_auth).is_none() {
        return false;
    }
    let deadline = tokio::time::Instant::now() + max_wait;
    let mut changes = execution_auth.active_auth_change_receiver();
    loop {
        if cancellation.is_cancelled() {
            return false;
        }
        if execution_auth.active_lease().is_some() {
            return true;
        }
        let now = tokio::time::Instant::now();
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
