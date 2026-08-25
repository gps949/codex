use codex_protocol::error::CodexErr;

use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthLease;
use crate::failover::FailoverCoordinator;
use crate::failover::FailoverOutcome;
use crate::failover_checkpoint::FailoverRetryMode;
use crate::failover_checkpoint::SamplingAttemptCheckpoint;

/// Turn-loop action after the native account layer has inspected an inference failure.
#[derive(Clone, Debug)]
pub(crate) enum SamplingFailoverDirective {
    /// Keep normal Codex error/retry handling; this is not an account-availability failure.
    NotHandled,
    /// Submit the exact same logical prompt using the newly selected execution account.
    ReplayCurrentSamplingRequest,
    /// The failed response already produced durable conversation/tool state. Rebuild the prompt
    /// from local history and continue on the newly selected execution account.
    ContinueFromDurableHistory,
    /// The account pool recognized the failure but every configured account is unavailable.
    PoolExhausted,
    /// A tool may have caused an external side effect without a durable result, or partial visible
    /// model output must first be reconciled. The pool may already have moved to another account,
    /// but automatic replay is intentionally blocked.
    ReconcileCurrentAttempt,
}

/// Converts one model failure plus its sampling checkpoint into a turn-loop continuation policy.
///
/// The coordinator owns execution-identity switching. This adapter owns replay semantics, keeping
/// scheduling independent from the agent loop so the same failover machinery can serve subagents,
/// review workers and other inference surfaces.
pub(crate) fn handle_sampling_failover(
    execution_auth: &ExecutionAuth,
    failed_lease: &ExecutionAuthLease,
    checkpoint: SamplingAttemptCheckpoint,
    error: &CodexErr,
) -> std::io::Result<SamplingFailoverDirective> {
    match FailoverCoordinator::handle_inference_error(execution_auth, failed_lease, error)? {
        FailoverOutcome::NotApplicable => Ok(SamplingFailoverDirective::NotHandled),
        FailoverOutcome::PoolExhausted { cause } => {
            tracing::warn!(
                ?cause,
                "every configured Codex execution account is unavailable"
            );
            Ok(SamplingFailoverDirective::PoolExhausted)
        }
        FailoverOutcome::Rebound {
            cause,
            from_profile,
            from_generation,
            to_profile,
            to_generation,
        } => {
            tracing::info!(
                ?cause,
                ?from_profile,
                from_generation,
                ?to_profile,
                to_generation,
                "rotated to another Codex execution account"
            );
            match checkpoint.retry_mode() {
                FailoverRetryMode::ReplayCurrentSamplingRequest => {
                    Ok(SamplingFailoverDirective::ReplayCurrentSamplingRequest)
                }
                // Until the UI/protocol layer has an explicit abandoned-partial-output lifecycle,
                // never duplicate visible assistant/reasoning text just to make switching look
                // seamless.
                FailoverRetryMode::ReplayAfterAbandoningPartialOutput => {
                    Ok(SamplingFailoverDirective::ReconcileCurrentAttempt)
                }
                FailoverRetryMode::ContinueFromDurableHistory => {
                    Ok(SamplingFailoverDirective::ContinueFromDurableHistory)
                }
                FailoverRetryMode::ReconcileCurrentAttempt => {
                    Ok(SamplingFailoverDirective::ReconcileCurrentAttempt)
                }
            }
        }
    }
}

/// How the turn proceeds after an automatic account switch, for user-facing messaging.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AccountSwitchContinuation {
    /// The turn continues automatically on the newly selected account.
    Automatic,
    /// Visible partial output blocked automatic replay; the user must re-send their message.
    ResendRequired,
}

/// Builds the user-facing message for a completed account rotation, naming the account that took
/// over. Returns `None` when no account is schedulable; the pool-exhausted message covers that.
pub(crate) fn account_switch_message(
    execution_auth: &ExecutionAuth,
    failed_lease: &ExecutionAuthLease,
    continuation: AccountSwitchContinuation,
) -> Option<String> {
    let active = execution_auth.active_lease()?;
    let to_profile = active.profile_id()?.clone();
    let from = failed_lease
        .profile_id()
        .map(|id| format!("`{id}`"))
        .unwrap_or_else(|| "the previous account".to_string());
    let suffix = match continuation {
        AccountSwitchContinuation::Automatic => "The turn continues automatically.",
        AccountSwitchContinuation::ResendRequired => {
            "Partial output could not be replayed safely; re-send your message to continue on the new account."
        }
    };
    Some(format!(
        "Codex account {from} became unavailable; switched to `{to_profile}`. {suffix}"
    ))
}

/// Builds the user-facing message shown when every configured account is unavailable.
pub(crate) fn pool_exhausted_message(execution_auth: &ExecutionAuth) -> String {
    let earliest_reset = execution_auth
        .account_pool()
        .map(|pool| pool.snapshots())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|snapshot| match snapshot.availability {
            codex_login::AccountAvailability::Exhausted { resets_at } => resets_at,
            codex_login::AccountAvailability::Available
            | codex_login::AccountAvailability::AuthenticationUnavailable { .. }
            | codex_login::AccountAvailability::Disabled => None,
        })
        .min();
    match earliest_reset {
        Some(resets_at) => format!(
            "All configured Codex accounts have hit their usage limits. The earliest cooldown ends at {}.",
            resets_at.format("%Y-%m-%d %H:%M UTC")
        ),
        None => "All configured Codex accounts are currently unavailable. Run `codex account list` for details.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_account_failure_directive_is_stable() {
        assert!(matches!(
            SamplingFailoverDirective::NotHandled,
            SamplingFailoverDirective::NotHandled
        ));
    }
}
