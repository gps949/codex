/// Progress made by one concrete model sampling attempt.
///
/// The turn loop owns this checkpoint because only it can distinguish transport progress from
/// durable conversation progress and externally visible tool side effects. Native account failover
/// uses it to avoid replaying a potentially non-idempotent tool call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SamplingAttemptCheckpoint {
    response_started: bool,
    visible_output_emitted: bool,
    completed_output_persisted: bool,
    tool_call_persisted: bool,
    tool_result_committed: bool,
    tool_reconciliation_failed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailoverRetryMode {
    /// Nothing observable escaped the failed request; submit the same prompt on the new account.
    ReplayCurrentSamplingRequest,
    /// Partial assistant/reasoning UI output escaped but no durable/tool state did. The caller must
    /// close/abandon the partial UI item before replaying the same logical request.
    ReplayAfterAbandoningPartialOutput,
    /// A completed model item or a tool call/output is already in local history. Rebuild the next
    /// prompt from local history and continue; never replay the old request's tool call.
    ContinueFromDurableHistory,
    /// A tool may have had a side effect but its result could not be durably reconciled. Do not
    /// automatically replay or continue until the incomplete call pair is resolved.
    ReconcileCurrentAttempt,
}

impl SamplingAttemptCheckpoint {
    pub(crate) fn mark_response_started(&mut self) {
        self.response_started = true;
    }

    pub(crate) fn mark_visible_output_emitted(&mut self) {
        self.response_started = true;
        self.visible_output_emitted = true;
    }

    /// Called after a completed response item has been persisted to local conversation history.
    pub(crate) fn mark_completed_output_persisted(&mut self) {
        self.response_started = true;
        self.completed_output_persisted = true;
    }

    /// Called only after the model's tool-call item itself is durable and the execution future has
    /// been accepted by the turn loop.
    pub(crate) fn mark_tool_call_persisted(&mut self) {
        self.response_started = true;
        self.completed_output_persisted = true;
        self.tool_call_persisted = true;
    }

    /// Called after the matching tool output is written to conversation history.
    pub(crate) fn mark_tool_result_committed(&mut self) {
        self.response_started = true;
        self.tool_call_persisted = true;
        self.tool_result_committed = true;
    }

    /// A tool future failed without yielding a durable output. Its external side effect may still
    /// have happened, so automatic failover must stop rather than duplicate it on another account.
    pub(crate) fn mark_tool_reconciliation_failed(&mut self) {
        self.response_started = true;
        self.tool_call_persisted = true;
        self.tool_reconciliation_failed = true;
    }

    pub(crate) fn retry_mode(&self) -> FailoverRetryMode {
        if self.tool_reconciliation_failed {
            return FailoverRetryMode::ReconcileCurrentAttempt;
        }
        if self.tool_call_persisted || self.tool_result_committed || self.completed_output_persisted {
            return FailoverRetryMode::ContinueFromDurableHistory;
        }
        if self.visible_output_emitted {
            return FailoverRetryMode::ReplayAfterAbandoningPartialOutput;
        }

        // Receiving a response envelope alone has no semantic or user-visible side effect.
        let _ = self.response_started;
        FailoverRetryMode::ReplayCurrentSamplingRequest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_attempt_can_be_replayed() {
        assert_eq!(
            SamplingAttemptCheckpoint::default().retry_mode(),
            FailoverRetryMode::ReplayCurrentSamplingRequest
        );
    }

    #[test]
    fn response_envelope_without_output_can_be_replayed() {
        let mut checkpoint = SamplingAttemptCheckpoint::default();
        checkpoint.mark_response_started();
        assert_eq!(
            checkpoint.retry_mode(),
            FailoverRetryMode::ReplayCurrentSamplingRequest
        );
    }

    #[test]
    fn partial_ui_output_must_be_closed_before_replay() {
        let mut checkpoint = SamplingAttemptCheckpoint::default();
        checkpoint.mark_visible_output_emitted();
        assert_eq!(
            checkpoint.retry_mode(),
            FailoverRetryMode::ReplayAfterAbandoningPartialOutput
        );
    }

    #[test]
    fn persisted_tool_call_continues_from_history() {
        let mut checkpoint = SamplingAttemptCheckpoint::default();
        checkpoint.mark_tool_call_persisted();
        assert_eq!(
            checkpoint.retry_mode(),
            FailoverRetryMode::ContinueFromDurableHistory
        );
        checkpoint.mark_tool_result_committed();
        assert_eq!(
            checkpoint.retry_mode(),
            FailoverRetryMode::ContinueFromDurableHistory
        );
    }

    #[test]
    fn unreconciled_tool_failure_blocks_automatic_replay() {
        let mut checkpoint = SamplingAttemptCheckpoint::default();
        checkpoint.mark_tool_call_persisted();
        checkpoint.mark_tool_reconciliation_failed();
        assert_eq!(
            checkpoint.retry_mode(),
            FailoverRetryMode::ReconcileCurrentAttempt
        );
    }
}
