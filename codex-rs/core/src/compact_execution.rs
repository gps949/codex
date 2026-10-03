//! Request-owned transport and history ownership for local summarizing compaction.

use std::sync::Arc;

use codex_history::CodexHarnessMetadata;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;

use crate::account_transition::stamp_execution_provenance;
use crate::api_account_execution::ApiExecutionTarget;
use crate::api_account_execution::project_api_history;
use crate::client::ModelClientSession;
use crate::context_manager::ContextManager;
use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthBinding;
use crate::execution_auth::ExecutionAuthMode;
use crate::failover_turn::pool_unavailable_error;
use crate::opaque_history_migration::AccountTransitionTargetProfile;
use crate::opaque_history_migration::preflight_account_transition;
use crate::portable_compaction::PortableCompactionPolicy;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

pub(super) struct CompactionExecutionIdentity {
    pub(super) binding: ExecutionAuthBinding,
    pub(super) api_target: Option<Arc<ApiExecutionTarget>>,
}

impl CompactionExecutionIdentity {
    pub(super) fn stamp(&self, metadata: &mut Option<CodexHarnessMetadata>) {
        if let Some(target) = &self.api_target {
            let metadata = metadata.get_or_insert_default();
            metadata.execution_profile_id = Some(target.profile_id.clone());
            metadata.execution_generation = Some(0);
        } else if let ExecutionAuthBinding::Pooled(lease) = &self.binding {
            stamp_execution_provenance(metadata, lease);
        }
    }
}

pub(super) struct LocalCompactionExecution {
    pub(super) identity: CompactionExecutionIdentity,
    pub(super) client: ModelClientSession,
    pub(super) portable_policy: PortableCompactionPolicy,
}

impl LocalCompactionExecution {
    pub(super) async fn capture(
        session: &Session,
        turn: &TurnContext,
        execution_auth: &ExecutionAuth,
        history: &ContextManager,
    ) -> Result<Self> {
        let api_target = turn.extension_data.get::<ApiExecutionTarget>();
        let mode = if api_target.is_some() {
            ExecutionAuthMode::Stock
        } else {
            execution_auth
                .mode_for_turn(turn.config.as_ref(), turn.provider.info())
                .await
                .map_err(|error| {
                    CodexErr::UnsupportedOperation(format!(
                        "failed to initialize native multi-account execution for compaction: {error}"
                    ))
                })?
        };
        let binding = mode
            .capture_binding()
            .map_err(|_| pool_unavailable_error(execution_auth))?;
        let portable_policy = if api_target.is_some() {
            // API output is a portable checkpoint even when source history had no pool metadata.
            PortableCompactionPolicy::Portable
        } else {
            PortableCompactionPolicy::for_history(&mode, history.annotated_items())
        };
        let preflight_history = history
            .clone()
            .for_prompt_annotated(&turn.model_info().input_modalities);
        let mut client = if let Some(target) = &api_target {
            project_api_history(preflight_history)?;
            session
                .services
                .model_client
                .new_session_for_api_target(target)
        } else {
            let profile = AccountTransitionTargetProfile::from_execution(execution_auth, &binding);
            preflight_account_transition(&preflight_history, &profile)
                .ensure_ready(&profile)
                .map_err(|error| CodexErr::AccountMigrationRequired(error.to_string()))?;
            session.services.model_client.new_session()
        };
        if let Some(request_auth) = binding.request_auth() {
            client.bind_execution_auth(request_auth);
        }
        Ok(Self {
            identity: CompactionExecutionIdentity {
                binding,
                api_target,
            },
            client,
            portable_policy,
        })
    }
}
