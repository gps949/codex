//! Request-owned transport and history ownership for local summarizing compaction.

use std::sync::Arc;
use std::time::Duration;

use codex_history::CodexHarnessMetadata;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::WarningEvent;

use crate::account_pool_recovery::FinalRecoveryOutcome;
use crate::account_pool_recovery::RecoveryWaitBudget;
use crate::account_pool_recovery::SamplingRecoveryBudget;
use crate::account_pool_recovery::probe_for_final_recovery;
use crate::account_pool_recovery::wait_for_recovery;
use crate::account_transition::stamp_execution_provenance;
use crate::api_account_execution::ApiExecutionTarget;
use crate::api_account_execution::project_api_history;
use crate::client::ModelClientSession;
use crate::context_manager::ContextManager;
use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthBinding;
use crate::execution_auth::ExecutionAuthMode;
use crate::failover::FailoverCause;
use crate::failover_checkpoint::SamplingAttemptCheckpoint;
use crate::failover_turn::AccountFailoverTransition;
use crate::failover_turn::AccountSwitchContinuation;
use crate::failover_turn::SamplingFailoverDirective;
use crate::failover_turn::account_switch_message;
use crate::failover_turn::earliest_exhausted_reset;
use crate::failover_turn::handle_sampling_failover;
use crate::failover_turn::pool_unavailable_error;
use crate::failover_turn::preemptive_switch_message;
use crate::failover_turn::recovery_continuation_message;
use crate::failover_turn::reset_credit_rescue_message;
use crate::failover_turn::sync_session_rate_limits_with_active_account;
use crate::opaque_history_migration::AccountTransitionTargetProfile;
use crate::opaque_history_migration::preflight_account_transition;
use crate::portable_compaction::PortableCompactionPolicy;
use crate::reset_credit_rescue::try_reset_credit_rescue;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
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
        if api_target.is_none()
            && mode.multi_account_enabled()
            && let Some(threshold) = turn
                .config
                .account_pool
                .effective_preemptive_switch_percent()
            && let Some(switch) = execution_auth.preemptive_rotation(threshold)
        {
            execution_auth.compatibility_auth_manager().reload().await;
            sync_session_rate_limits_with_active_account(session, turn, execution_auth).await;
            session
                .send_event(
                    turn,
                    EventMsg::Warning(WarningEvent {
                        message: preemptive_switch_message(&switch),
                    }),
                )
                .await;
        }
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
            .for_prompt_annotated(
                &turn.model_info().input_modalities,
                session.services.image_store.as_ref(),
            )
            .await;
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

    pub(super) fn observe_rate_limits(
        &self,
        execution_auth: &ExecutionAuth,
        snapshot: &RateLimitSnapshot,
    ) {
        if self.identity.api_target.is_some() {
            return;
        }
        if let ExecutionAuthBinding::Pooled(lease) = &self.identity.binding
            && let Err(error) = execution_auth.observe_rate_limits(lease, snapshot)
        {
            tracing::debug!(%error, "failed to record account-pool rate limits during compaction");
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn try_recover_from_error(
        &mut self,
        session: &Session,
        step_context: &StepContext,
        execution_auth: &ExecutionAuth,
        history: &ContextManager,
        error: &CodexErr,
        recovery_budget: &mut SamplingRecoveryBudget,
        wait_budget: &RecoveryWaitBudget,
    ) -> Result<bool> {
        if self.identity.api_target.is_some() {
            return Ok(false);
        }
        let turn = step_context.turn.as_ref();
        if let CodexErrorDetails::UsageLimitReached(limit) = error.details()
            && let Some(rate_limits) = limit.rate_limits.clone()
        {
            session.update_rate_limits(turn, *rate_limits).await;
        }
        let account_failure = matches!(
            error.details(),
            CodexErrorDetails::UsageLimitReached(_)
                | CodexErrorDetails::QuotaExceeded
                | CodexErrorDetails::UsageNotIncluded
                | CodexErrorDetails::RefreshTokenFailed(_)
        );
        let ExecutionAuthBinding::Pooled(failed_lease) = &self.identity.binding else {
            return Ok(false);
        };
        if !account_failure {
            return Ok(false);
        }
        let failed_lease = failed_lease.clone();
        match handle_sampling_failover(
            execution_auth,
            &failed_lease,
            SamplingAttemptCheckpoint::default(),
            error,
        )
        .await
        .map_err(CodexErr::from)?
        {
            SamplingFailoverDirective::NotHandled
            | SamplingFailoverDirective::ReconcileCurrentAttempt { .. } => Ok(false),
            SamplingFailoverDirective::ReplayCurrentSamplingRequest { transition }
            | SamplingFailoverDirective::ContinueFromDurableHistory { transition } => {
                if let Some(account) = failed_lease.account_lease() {
                    let _ = account.auth_manager().auth().await;
                }
                execution_auth.compatibility_auth_manager().reload().await;
                sync_session_rate_limits_with_active_account(session, turn, execution_auth).await;
                if transition == AccountFailoverTransition::Rebound
                    && let Some(message) = account_switch_message(
                        execution_auth,
                        &failed_lease,
                        AccountSwitchContinuation::Automatic,
                    )
                {
                    session
                        .send_event(turn, EventMsg::Warning(WarningEvent { message }))
                        .await;
                }
                *self = Box::pin(Self::capture(session, turn, execution_auth, history)).await?;
                Ok(true)
            }
            SamplingFailoverDirective::PoolExhausted { cause, .. } => {
                if cause == FailoverCause::UsageNotIncluded {
                    session
                        .send_event(
                            turn,
                            EventMsg::Warning(WarningEvent {
                                message: "No available pool account includes this use. Choose another model or check the accounts' plan and workspace access. Reset credits are not redeemed for entitlement errors.".into(),
                            }),
                        )
                        .await;
                    return Ok(false);
                }
                if !recovery_budget.can_retry() {
                    return Ok(false);
                }
                let cancellation = &step_context.turn_cancellation;
                let recovery =
                    probe_for_final_recovery(execution_auth, turn.config.as_ref(), cancellation)
                        .await;
                if cancellation.is_cancelled() {
                    return Err(CodexErr::TurnAborted);
                }
                if recovery == FinalRecoveryOutcome::Recovered {
                    recovery_budget.record_recovery();
                    execution_auth.compatibility_auth_manager().reload().await;
                    sync_session_rate_limits_with_active_account(session, turn, execution_auth)
                        .await;
                    session
                        .send_event(
                            turn,
                            EventMsg::Warning(WarningEvent {
                                message: recovery_continuation_message(execution_auth),
                            }),
                        )
                        .await;
                    *self = Box::pin(Self::capture(session, turn, execution_auth, history)).await?;
                    return Ok(true);
                }
                if let Some(rescue) = try_reset_credit_rescue(
                    execution_auth,
                    &failed_lease,
                    turn.config.as_ref(),
                    cancellation,
                )
                .await
                {
                    if cancellation.is_cancelled() {
                        return Err(CodexErr::TurnAborted);
                    }
                    recovery_budget.record_recovery();
                    execution_auth.compatibility_auth_manager().reload().await;
                    sync_session_rate_limits_with_active_account(session, turn, execution_auth)
                        .await;
                    session
                        .send_event(
                            turn,
                            EventMsg::Warning(WarningEvent {
                                message: reset_credit_rescue_message(
                                    execution_auth,
                                    rescue.redeemed_profile_id.as_ref(),
                                    &rescue.profile_id,
                                    /*safe_to_continue*/ true,
                                ),
                            }),
                        )
                        .await;
                    *self = Box::pin(Self::capture(session, turn, execution_auth, history)).await?;
                    return Ok(true);
                }
                let max_wait = wait_budget.remaining();
                if !max_wait.is_zero()
                    && let Some(reset) = earliest_exhausted_reset(execution_auth)
                    && (reset - chrono::Utc::now())
                        .to_std()
                        .unwrap_or(Duration::ZERO)
                        <= max_wait
                {
                    session
                        .send_event(
                            turn,
                            EventMsg::Warning(WarningEvent {
                                message: format!(
                                    "All Codex accounts are cooling down. Waiting to retry (earliest check: {}). Cancel to stop waiting.",
                                    codex_login::format_exhausted_reset(reset),
                                ),
                            }),
                        )
                        .await;
                    let recovered = wait_for_recovery(
                        execution_auth,
                        turn.config.as_ref(),
                        wait_budget,
                        cancellation,
                    )
                    .await;
                    if recovered {
                        recovery_budget.record_recovery();
                        execution_auth.compatibility_auth_manager().reload().await;
                        sync_session_rate_limits_with_active_account(session, turn, execution_auth)
                            .await;
                        session
                            .send_event(
                                turn,
                                EventMsg::Warning(WarningEvent {
                                    message: recovery_continuation_message(execution_auth),
                                }),
                            )
                            .await;
                        *self = Self::capture(session, turn, execution_auth, history).await?;
                        return Ok(true);
                    }
                    if cancellation.is_cancelled() {
                        return Err(CodexErr::TurnAborted);
                    }
                }
                Ok(false)
            }
        }
    }
}
