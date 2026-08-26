use std::sync::Arc;
use std::sync::Mutex;

use crate::failover_checkpoint::SamplingAttemptCheckpoint;
use crate::session::turn_context::TurnContext;

/// Request-local sampling progress stored in the turn's existing extension data.
///
/// Durable model/tool progress is reconstructed from annotated history after an error. This state
/// tracks only progress that can escape to a client before it is durable, which is the extra fact
/// needed to avoid duplicating partial assistant/reasoning output after an account switch.
#[derive(Clone, Debug, Default)]
pub(crate) struct SamplingAttemptState {
    checkpoint: Arc<Mutex<SamplingAttemptCheckpoint>>,
}

impl SamplingAttemptState {
    pub(crate) fn mark_response_started(&self) {
        self.with_checkpoint(SamplingAttemptCheckpoint::mark_response_started);
    }

    pub(crate) fn mark_visible_output_emitted(&self) {
        self.with_checkpoint(SamplingAttemptCheckpoint::mark_visible_output_emitted);
    }

    pub(crate) fn snapshot(&self) -> SamplingAttemptCheckpoint {
        *self
            .checkpoint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn with_checkpoint(&self, update: impl FnOnce(&mut SamplingAttemptCheckpoint)) {
        let mut checkpoint = self
            .checkpoint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        update(&mut checkpoint);
    }
}

/// Replaces the attempt state immediately before a concrete model request.
pub(crate) fn install_sampling_attempt(turn_context: &TurnContext) -> SamplingAttemptState {
    let attempt = SamplingAttemptState::default();
    turn_context.extension_data.insert(attempt.clone());
    attempt
}

pub(crate) fn sampling_attempt(turn_context: &TurnContext) -> Option<SamplingAttemptState> {
    turn_context
        .extension_data
        .get::<SamplingAttemptState>()
        .map(|state| state.as_ref().clone())
}

pub(crate) fn mark_sampling_response_started(turn_context: &TurnContext) {
    if let Some(attempt) = sampling_attempt(turn_context) {
        attempt.mark_response_started();
    }
}

pub(crate) fn mark_sampling_visible_output(turn_context: &TurnContext) {
    if let Some(attempt) = sampling_attempt(turn_context) {
        attempt.mark_visible_output_emitted();
    }
}
