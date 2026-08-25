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
