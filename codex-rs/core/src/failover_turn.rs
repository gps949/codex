use chrono::DateTime;
use chrono::Utc;
use codex_login::AccountAvailability;
use codex_protocol::error::CodexErr;
use codex_protocol::error::UsageLimitReachedError;

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
    ReplayCurrentSamplingRequest {
        transition: AccountFailoverTransition,
    },
    /// The failed response already produced durable conversation/tool state. Rebuild the prompt
    /// from local history and continue on the newly selected execution account.
    ContinueFromDurableHistory {
        transition: AccountFailoverTransition,
    },
    /// The account pool recognized the failure but every configured account is unavailable.
    PoolExhausted,
    /// A tool may have caused an external side effect without a durable result, or partial visible
    /// model output must first be reconciled. The pool may already have moved to another account,
    /// but automatic replay is intentionally blocked.
    ReconcileCurrentAttempt {
        transition: AccountFailoverTransition,
    },
}

/// Whether this failure mutation selected the active account or found it already selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccountFailoverTransition {
    Rebound,
    ActiveUnchanged,
}

/// Converts one model failure plus its sampling checkpoint into a turn-loop continuation policy.
///
/// The coordinator owns execution-identity switching. This adapter owns replay semantics, keeping
/// scheduling independent from the agent loop so the same failover machinery can serve subagents,
/// review workers and other inference surfaces.
pub(crate) async fn handle_sampling_failover(
    execution_auth: &ExecutionAuth,
    failed_lease: &ExecutionAuthLease,
    checkpoint: SamplingAttemptCheckpoint,
    error: &CodexErr,
) -> std::io::Result<SamplingFailoverDirective> {
    match FailoverCoordinator::handle_inference_error(execution_auth, failed_lease, error).await? {
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
            Ok(directive_for_retry_mode(
                checkpoint.retry_mode(),
                AccountFailoverTransition::Rebound,
            ))
        }
        FailoverOutcome::ActiveUnchanged {
            cause,
            failed_profile,
            failed_generation,
            active_profile,
            active_generation,
        } => {
            tracing::info!(
                ?cause,
                ?failed_profile,
                failed_generation,
                ?active_profile,
                active_generation,
                "attributed a late execution failure without changing the active Codex account"
            );
            Ok(directive_for_retry_mode(
                checkpoint.retry_mode(),
                AccountFailoverTransition::ActiveUnchanged,
            ))
        }
    }
}

fn directive_for_retry_mode(
    retry_mode: FailoverRetryMode,
    transition: AccountFailoverTransition,
) -> SamplingFailoverDirective {
    match retry_mode {
        FailoverRetryMode::ReplayCurrentSamplingRequest => {
            SamplingFailoverDirective::ReplayCurrentSamplingRequest { transition }
        }
        // Until the UI/protocol layer has an explicit abandoned-partial-output lifecycle, never
        // duplicate visible assistant/reasoning text just to make switching look seamless.
        FailoverRetryMode::ReplayAfterAbandoningPartialOutput
        | FailoverRetryMode::ReconcileCurrentAttempt => {
            SamplingFailoverDirective::ReconcileCurrentAttempt { transition }
        }
        FailoverRetryMode::ContinueFromDurableHistory => {
            SamplingFailoverDirective::ContinueFromDurableHistory { transition }
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

/// Earliest known cooldown end across exhausted pool profiles.
pub(crate) fn earliest_exhausted_reset(execution_auth: &ExecutionAuth) -> Option<DateTime<Utc>> {
    execution_auth
        .account_pool()
        .map(|pool| pool.snapshots())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|snapshot| match snapshot.availability {
            AccountAvailability::Exhausted { resets_at } => resets_at,
            AccountAvailability::Available
            | AccountAvailability::AuthenticationUnavailable { .. }
            | AccountAvailability::Disabled => None,
        })
        .min()
}

/// Builds the user-facing message shown when every configured account is unavailable.
pub(crate) fn pool_exhausted_message(execution_auth: &ExecutionAuth) -> String {
    match earliest_exhausted_reset(execution_auth) {
        Some(resets_at) => format!(
            "All configured Codex accounts have hit their usage limits. The earliest cooldown ends at {}. If a plan has earned rate-limit reset credits, redeeming one (for example from /status in the TUI or the app) unblocks that account immediately.",
            resets_at.format("%Y-%m-%d %H:%M UTC")
        ),
        None => "All configured Codex accounts are currently unavailable. Run `codex account list` for details.".to_string(),
    }
}

/// Error returned when a new sampling request cannot obtain any pool lease.
///
/// Must be [`CodexErr::UsageLimitReached`] (not UnsupportedOperation) so clients map it to
/// `UsageLimitExceeded` and can show the normal cooldown UI instead of a hard BadRequest.
pub(crate) fn pool_unavailable_error(execution_auth: &ExecutionAuth) -> CodexErr {
    CodexErr::UsageLimitReached(UsageLimitReachedError {
        plan_type: None,
        resets_at: earliest_exhausted_reset(execution_auth),
        rate_limits: None,
        promo_message: None,
        rate_limit_reached_type: None,
    })
}

#[cfg(test)]
#[path = "failover_turn_tests.rs"]
mod tests;
