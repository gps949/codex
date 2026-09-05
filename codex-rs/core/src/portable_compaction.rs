use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ResponseItem;

use crate::account_transition::AccountHistoryTransition;
use crate::account_transition::AccountHistoryTransitionError;
use crate::account_transition::AccountHistoryTransitionStats;
use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthBinding;
use crate::execution_auth::ExecutionAuthMode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PortableCompactionPolicy {
    Stock,
    Portable,
}

pub(crate) fn project_history_for_execution(
    execution_auth: &ExecutionAuth,
    execution_binding: &ExecutionAuthBinding,
    history: Vec<ResponseItemEnvelope>,
) -> Result<Vec<ResponseItem>, AccountHistoryTransitionError> {
    let transition = match execution_binding {
        ExecutionAuthBinding::Stock => AccountHistoryTransition::stock(),
        ExecutionAuthBinding::Pooled(lease) => AccountHistoryTransition::pooled(
            lease,
            execution_auth
                .legacy_unattributed_profile_id()
                .map(|id| id.as_str().to_string()),
        ),
    };
    let (items, stats) = transition.prepare_for_request(history)?;
    trace_projection(execution_binding, stats);
    Ok(items)
}

fn trace_projection(
    execution_binding: &ExecutionAuthBinding,
    stats: AccountHistoryTransitionStats,
) {
    if stats == AccountHistoryTransitionStats::default() {
        return;
    }
    let (profile_id, generation) = match execution_binding {
        ExecutionAuthBinding::Stock => ("<stock>", 0),
        ExecutionAuthBinding::Pooled(lease) => (
            lease
                .profile_id()
                .map(codex_login::AccountProfileId::as_str)
                .unwrap_or("<legacy>"),
            lease.generation(),
        ),
    };
    tracing::info!(
        target: "codex_core::account_transition",
        target_profile_id = profile_id,
        target_generation = generation,
        cleared_response_ids = stats.cleared_response_ids,
        cleared_internal_metadata = stats.cleared_internal_metadata,
        stripped_reasoning_blobs = stats.stripped_reasoning_blobs,
        stripped_encrypted_function_args = stats.stripped_encrypted_function_args,
        stripped_encrypted_tool_outputs = stats.stripped_encrypted_tool_outputs,
        stripped_encrypted_agent_message_parts = stats.stripped_encrypted_agent_message_parts,
        dropped_encrypted_agent_messages = stats.dropped_encrypted_agent_messages,
        "projected account-scoped history for execution"
    );
}

impl PortableCompactionPolicy {
    pub(crate) fn for_history(
        execution_auth_mode: &ExecutionAuthMode,
        history: &[ResponseItemEnvelope],
    ) -> Self {
        if execution_auth_mode.is_pooled()
            || history.iter().any(|envelope| {
                envelope.metadata.as_ref().is_some_and(|metadata| {
                    metadata.execution_profile_id.is_some()
                        || metadata.execution_generation.is_some()
                })
            })
        {
            Self::Portable
        } else {
            Self::Stock
        }
    }
}

#[cfg(test)]
#[path = "portable_compaction_tests.rs"]
mod tests;
