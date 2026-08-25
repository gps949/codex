use codex_protocol::models::ResponseItem;

use crate::execution_auth::ExecutionAuthLease;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

/// Request-scoped execution identity stored in the existing turn extension data.
///
/// The value is replaced immediately before each sampling request. Completed model items and tool
/// results are persisted before that request returns, so they retain the exact lease that produced
/// them even if another worker rotates the process-wide account pool concurrently.
#[derive(Clone, Debug)]
pub(crate) struct SamplingExecutionProvenance {
    lease: ExecutionAuthLease,
}

impl SamplingExecutionProvenance {
    pub(crate) fn new(lease: ExecutionAuthLease) -> Self {
        Self { lease }
    }

    pub(crate) fn lease(&self) -> &ExecutionAuthLease {
        &self.lease
    }
}

pub(crate) fn set_sampling_execution_provenance(
    turn_context: &TurnContext,
    lease: ExecutionAuthLease,
) {
    turn_context
        .extension_data
        .insert(SamplingExecutionProvenance::new(lease));
}

pub(crate) fn sampling_execution_provenance(
    turn_context: &TurnContext,
) -> Option<SamplingExecutionProvenance> {
    turn_context
        .extension_data
        .get::<SamplingExecutionProvenance>()
        .map(|value| value.as_ref().clone())
}

/// Persists conversation items with request execution provenance when a sampling lease is present.
/// Locally-authored paths that do not run inside a sampling request retain stock history behavior.
pub(crate) async fn record_conversation_items_with_execution_provenance(
    sess: &Session,
    turn_context: &TurnContext,
    items: &[ResponseItem],
) {
    if let Some(provenance) = sampling_execution_provenance(turn_context) {
        sess.record_conversation_items_for_execution(
            turn_context,
            items,
            provenance.lease(),
        )
        .await;
    } else {
        sess.record_conversation_items(turn_context, items).await;
    }
}
