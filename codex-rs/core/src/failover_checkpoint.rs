use std::collections::HashSet;

use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ResponseItem;

/// Cursor captured immediately before one concrete sampling request.
///
/// Codex already drains started tool futures and records completed tool outputs before
/// `try_run_sampling_request` returns. Comparing durable history after an error therefore gives us
/// a much more future-proof side-effect checkpoint than instrumenting every individual tool
/// handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SamplingHistoryCursor {
    history_version: u64,
    item_count: usize,
}

/// Progress made by one concrete model sampling attempt.
///
/// Streaming/UI progress is still marked explicitly because partial deltas are intentionally not
/// durable. Completed model items and tool call/output reconciliation are derived from the history
/// delta using [`SamplingHistoryCursor`].
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

impl SamplingHistoryCursor {
    pub(crate) fn new(history_version: u64, item_count: usize) -> Self {
        Self {
            history_version,
            item_count,
        }
    }

    pub(crate) fn from_history(history_version: u64, items: &[ResponseItemEnvelope]) -> Self {
        Self::new(history_version, items.len())
    }

    /// Reconstructs durable progress since this cursor. History rewrites are conservatively treated
    /// as durable progress: replaying the old request across a compaction/rollback boundary would be
    /// less safe than rebuilding the next prompt from the new canonical history.
    pub(crate) fn checkpoint(
        &self,
        current_history_version: u64,
        items: &[ResponseItemEnvelope],
    ) -> SamplingAttemptCheckpoint {
        let mut checkpoint = SamplingAttemptCheckpoint::default();
        if current_history_version != self.history_version || items.len() < self.item_count {
            checkpoint.mark_completed_output_persisted();
            return checkpoint;
        }

        let mut pending_tool_calls = HashSet::<String>::new();
        let mut unkeyed_tool_call = false;
        for envelope in &items[self.item_count..] {
            let item = &envelope.item;
            match item {
                ResponseItem::FunctionCall { call_id, .. }
                | ResponseItem::CustomToolCall { call_id, .. } => {
                    checkpoint.mark_tool_call_persisted();
                    pending_tool_calls.insert(call_id.clone());
                }
                ResponseItem::LocalShellCall { call_id, .. }
                | ResponseItem::ToolSearchCall { call_id, .. } => {
                    checkpoint.mark_tool_call_persisted();
                    if let Some(call_id) = call_id {
                        pending_tool_calls.insert(call_id.clone());
                    } else {
                        // Older/local-only call shapes do not provide a stable id that can be
                        // paired here. Fail closed if an error interrupts such an attempt.
                        unkeyed_tool_call = true;
                    }
                }
                ResponseItem::FunctionCallOutput { call_id, .. }
                | ResponseItem::ToolSearchOutput { call_id, .. } => {
                    checkpoint.mark_tool_result_committed();
                    if let Some(call_id) = call_id {
                        pending_tool_calls.remove(call_id);
                    }
                }
                ResponseItem::CustomToolCallOutput { call_id, .. } => {
                    checkpoint.mark_tool_result_committed();
                    pending_tool_calls.remove(call_id);
                }
                // Server-side search/image calls and ordinary assistant/reasoning items have no
                // local tool side effect to replay, but their completed representation is durable.
                ResponseItem::AdditionalTools { .. }
                | ResponseItem::Message { .. }
                | ResponseItem::AgentMessage { .. }
                | ResponseItem::WebSearchCall { .. }
                | ResponseItem::Reasoning { .. }
                | ResponseItem::ImageGenerationCall { .. }
                | ResponseItem::Compaction { .. }
                | ResponseItem::ContextCompaction { .. }
                | ResponseItem::CompactionTrigger { .. }
                | ResponseItem::Other => checkpoint.mark_completed_output_persisted(),
            }
        }

        if unkeyed_tool_call || !pending_tool_calls.is_empty() {
            checkpoint.mark_tool_reconciliation_failed();
        }
        checkpoint
    }
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

    pub(crate) fn merge(&mut self, other: Self) {
        self.response_started |= other.response_started;
        self.visible_output_emitted |= other.visible_output_emitted;
        self.completed_output_persisted |= other.completed_output_persisted;
        self.tool_call_persisted |= other.tool_call_persisted;
        self.tool_result_committed |= other.tool_result_committed;
        self.tool_reconciliation_failed |= other.tool_reconciliation_failed;
    }

    pub(crate) fn retry_mode(self) -> FailoverRetryMode {
        if self.tool_reconciliation_failed {
            return FailoverRetryMode::ReconcileCurrentAttempt;
        }
        if self.tool_call_persisted || self.tool_result_committed || self.completed_output_persisted
        {
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

    fn envelope(item: ResponseItem) -> ResponseItemEnvelope {
        ResponseItemEnvelope::new(item)
    }

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
    fn completed_message_in_history_continues_instead_of_replaying() {
        let before = Vec::<ResponseItemEnvelope>::new();
        let cursor = SamplingHistoryCursor::from_history(0, &before);
        let after = vec![envelope(ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: Vec::new(),
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        })];
        let checkpoint = cursor.checkpoint(0, &after);
        assert_eq!(
            checkpoint.retry_mode(),
            FailoverRetryMode::ContinueFromDurableHistory
        );
    }

    #[test]
    fn unmatched_function_call_requires_reconciliation() {
        let cursor = SamplingHistoryCursor::new(0, 0);
        let after = vec![envelope(ResponseItem::FunctionCall {
            id: None,
            name: "write_file".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            encrypted_function_args: None,
            call_id: "call-1".to_string(),
            internal_chat_message_metadata_passthrough: None,
        })];
        let checkpoint = cursor.checkpoint(0, &after);
        assert_eq!(
            checkpoint.retry_mode(),
            FailoverRetryMode::ReconcileCurrentAttempt
        );
    }

    #[test]
    fn rewritten_history_is_durable_boundary() {
        let cursor = SamplingHistoryCursor::new(4, 10);
        let checkpoint = cursor.checkpoint(5, &[]);
        assert_eq!(
            checkpoint.retry_mode(),
            FailoverRetryMode::ContinueFromDurableHistory
        );
    }
}
