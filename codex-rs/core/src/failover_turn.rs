use codex_protocol::error::CodexErr;

use crate::execution_model_client::ExecutionModelClient;
use crate::execution_model_client::ExecutionModelClientSession;
use crate::failover::FailoverCoordinator;
use crate::failover::FailoverOutcome;
use crate::failover_checkpoint::FailoverRetryMode;
use crate::failover_checkpoint::SamplingAttemptCheckpoint;

/// Turn-loop action after the native account layer has inspected an inference failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SamplingFailoverDirective {
    /// Keep normal Codex error/retry handling; this is not an account-availability failure.
    NotHandled,
    /// Submit the exact same logical prompt using the newly-bound execution account.
    ReplayCurrentSamplingRequest,
    /// The failed response already produced durable conversation/tool state. Rebuild the prompt
    /// from local history and continue on the newly-bound execution account.
    ContinueFromDurableHistory,
    /// The account pool recognized the failure but every configured account is unavailable.
    PoolExhausted,
    /// A tool may have caused an external side effect without a durable result, or partial visible
    /// model output must first be reconciled. Automatic replay is intentionally blocked.
    ReconcileCurrentAttempt,
}

/// Converts one model failure plus its sampling checkpoint into a turn-loop continuation policy.
///
/// The coordinator owns execution-identity switching. This adapter owns replay semantics, keeping
/// scheduling independent from the agent loop so the same failover machinery can later serve
/// subagents, review workers and other inference surfaces.
pub(crate) fn handle_sampling_failover(
    model_client: &ExecutionModelClient,
    client_session: &mut ExecutionModelClientSession,
    checkpoint: &SamplingAttemptCheckpoint,
    error: &CodexErr,
) -> std::io::Result<SamplingFailoverDirective> {
    match FailoverCoordinator::handle_inference_error(model_client, client_session, error)? {
        FailoverOutcome::NotApplicable => Ok(SamplingFailoverDirective::NotHandled),
        FailoverOutcome::PoolExhausted { .. } => Ok(SamplingFailoverDirective::PoolExhausted),
        FailoverOutcome::Rebound { .. } => match checkpoint.retry_mode() {
            FailoverRetryMode::ReplayCurrentSamplingRequest => {
                Ok(SamplingFailoverDirective::ReplayCurrentSamplingRequest)
            }
            // Until the UI/protocol layer has an explicit abandoned-partial-output lifecycle,
            // never duplicate visible assistant/reasoning text just to make switching look seamless.
            FailoverRetryMode::ReplayAfterAbandoningPartialOutput => {
                Ok(SamplingFailoverDirective::ReconcileCurrentAttempt)
            }
            FailoverRetryMode::ContinueFromDurableHistory => {
                Ok(SamplingFailoverDirective::ContinueFromDurableHistory)
            }
            FailoverRetryMode::ReconcileCurrentAttempt => {
                Ok(SamplingFailoverDirective::ReconcileCurrentAttempt)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_enum_is_copy_for_turn_state_machine() {
        let directive = SamplingFailoverDirective::ContinueFromDurableHistory;
        let copied = directive;
        assert_eq!(directive, copied);
    }
}
