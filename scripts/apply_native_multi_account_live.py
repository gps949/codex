#!/usr/bin/env python3
from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected exactly one patch anchor, found {count}\n--- anchor ---\n{old}")
    file.write_text(text.replace(old, new, 1))
    print(f"patched {path}")


# Keep one execution-account coordinator alive for the process lifetime. This makes scheduler
# state/cooldowns stable across turns without adding another field to ThreadManager/Session APIs.
replace_once(
    "codex-rs/core/src/execution_auth.rs",
    "use std::sync::RwLock;\nuse std::sync::Weak;\n",
    "use std::sync::RwLock;\n",
)
replace_once(
    "codex-rs/core/src/execution_auth.rs",
    "static EXECUTION_AUTH_REGISTRY: OnceLock<StdMutex<HashMap<usize, Weak<ExecutionAuth>>>> =\n    OnceLock::new();",
    "static EXECUTION_AUTH_REGISTRY: OnceLock<StdMutex<HashMap<usize, Arc<ExecutionAuth>>>> =\n    OnceLock::new();",
)
replace_once(
    "codex-rs/core/src/execution_auth.rs",
    "        registry.retain(|_, coordinator| coordinator.strong_count() > 0);\n        if let Some(existing) = registry.get(&key).and_then(Weak::upgrade) {\n            return existing;\n        }\n\n        let coordinator = Arc::new(Self::legacy(legacy_manager));\n        registry.insert(key, Arc::downgrade(&coordinator));\n        coordinator",
    "        if let Some(existing) = registry.get(&key) {\n            return Arc::clone(existing);\n        }\n\n        let coordinator = Arc::new(Self::legacy(legacy_manager));\n        registry.insert(key, Arc::clone(&coordinator));\n        coordinator",
)
replace_once(
    "codex-rs/core/src/execution_auth.rs",
    "/// distinct AuthManager must keep their auth lifecycle isolated. Stale weak entries are removed on\n/// lookup, so allocator address reuse cannot attach a new manager to an old coordinator.",
    "/// distinct AuthManager must keep their auth lifecycle isolated. The registry intentionally owns\n/// a strong process-lifetime reference: the coordinator in turn owns the AuthManager, so pointer\n/// identity cannot be recycled while the process is alive and scheduler state survives between turns.",
)

# Compile the request provenance/checkpoint helpers.
replace_once(
    "codex-rs/core/src/lib.rs",
    "mod execution_auth;\nmod failover;",
    "mod execution_auth;\nmod execution_provenance;\nmod failover;",
)
replace_once(
    "codex-rs/core/src/lib.rs",
    "mod responses_retry;\npub(crate) mod session;",
    "mod responses_retry;\nmod sampling_attempt;\npub(crate) mod session;",
)

# A pooled request always projects annotated history against the target account. Same-account items
# remain lossless; foreign account-scoped state is sanitized by prepare_for_request().
replace_once(
    "codex-rs/core/src/account_transition.rs",
    "    pub(crate) fn is_cross_account(&self) -> bool {\n        self.cross_account\n    }",
    "    pub(crate) fn pooled(\n        lease: &ExecutionAuthLease,\n        legacy_unattributed_profile_id: Option<String>,\n    ) -> Self {\n        let mut transition = Self::initial(lease, legacy_unattributed_profile_id);\n        transition.cross_account = true;\n        transition\n    }\n\n    pub(crate) fn is_cross_account(&self) -> bool {\n        self.cross_account\n    }",
)

# Add a parallel Session recording entrypoint that uses the existing annotated persistence pipeline
# while stamping only model/tool items produced by a concrete execution lease.
replace_once(
    "codex-rs/core/src/session/mod.rs",
    "        self.record_prepared_conversation_items(turn_context, items, image_preparations)\n            .await;\n    }\n\n    async fn record_prepared_conversation_items(",
    "        self.record_prepared_conversation_items(turn_context, items, image_preparations)\n            .await;\n    }\n\n    pub(crate) async fn record_conversation_items_for_execution(\n        &self,\n        turn_context: &TurnContext,\n        items: &[ResponseItem],\n        lease: &crate::execution_auth::ExecutionAuthLease,\n    ) {\n        let (items, image_preparations) =\n            self.prepare_conversation_items_for_history(turn_context, items);\n        let items = items\n            .into_owned()\n            .into_iter()\n            .map(|item| crate::account_transition::envelope_from_execution(item, lease))\n            .collect();\n        self.record_prepared_conversation_items(turn_context, items, image_preparations)\n            .await;\n    }\n\n    async fn record_prepared_conversation_items(",
)

# Completed model output must retain the lease that produced it, rather than whatever account is
# globally active by the time the stream callback runs.
replace_once(
    "codex-rs/core/src/stream_events_utils.rs",
    "    sess.record_conversation_items(turn_context, std::slice::from_ref(item))\n        .await;\n    let defers_mailbox_delivery = finalized_facts.map_or_else(",
    "    crate::execution_provenance::record_conversation_items_with_execution_provenance(\n        sess,\n        turn_context,\n        std::slice::from_ref(item),\n    )\n    .await;\n    let defers_mailbox_delivery = finalized_facts.map_or_else(",
)

# Turn imports for native account execution/failover.
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "use crate::client::ModelClientSession;",
    "use crate::account_transition::AccountHistoryTransition;\nuse crate::client::ModelClientSession;\nuse crate::execution_auth::ExecutionAuth;\nuse crate::execution_provenance::record_conversation_items_with_execution_provenance;\nuse crate::execution_provenance::set_sampling_execution_provenance;\nuse crate::failover_checkpoint::SamplingHistoryCursor;\nuse crate::failover_turn::SamplingFailoverDirective;\nuse crate::failover_turn::handle_sampling_failover;\nuse crate::sampling_attempt::install_sampling_attempt;\nuse crate::sampling_attempt::mark_sampling_response_started;\nuse crate::sampling_attempt::mark_sampling_visible_output;",
)

# Lazily install the pool before pre-turn compaction. A configured pool discards any startup
# prewarmed transport because it may have been authenticated before ExternalAuth was installed.
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "    let mut client_session =\n        prewarmed_client_session.unwrap_or_else(|| sess.services.model_client.new_session());",
    "    let execution_auth = ExecutionAuth::shared(Arc::clone(&sess.services.auth_manager));\n    let multi_account_enabled = match execution_auth\n        .ensure_runtime_from_config(turn_context.config.as_ref())\n        .await\n    {\n        Ok(_) => execution_auth.multi_account_enabled(),\n        Err(err) => {\n            warn!(%err, \"failed to initialize native multi-account execution; using stock auth\");\n            false\n        }\n    };\n    let mut client_session = if multi_account_enabled {\n        sess.services.model_client.new_session()\n    } else {\n        prewarmed_client_session.unwrap_or_else(|| sess.services.model_client.new_session())\n    };",
)

# Pass the shared coordinator into the sampling loop.
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "                Arc::clone(&turn_diff_tracker),\n                &mut client_session,",
    "                Arc::clone(&turn_diff_tracker),\n                Arc::clone(&execution_auth),\n                &mut client_session,",
)
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "    turn_store: Arc<codex_extension_api::ExtensionData>,\n    turn_diff_tracker: SharedTurnDiffTracker,\n    client_session: &mut ModelClientSession,",
    "    turn_store: Arc<codex_extension_api::ExtensionData>,\n    turn_diff_tracker: SharedTurnDiffTracker,\n    execution_auth: Arc<ExecutionAuth>,\n    client_session: &mut ModelClientSession,",
)

# Make StepContext replaceable after an account switch and preserve stock initial-input behavior when
# pooling is not active.
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "    let turn_context = Arc::clone(&step_context.turn);\n    let base_instructions = sess.get_base_instructions().await;\n\n    let tool_runtime = ToolCallRuntime::new(\n        Arc::clone(&sess),\n        Arc::clone(&step_context),\n        Arc::clone(&turn_diff_tracker),\n    );",
    "    let mut step_context = step_context;\n    let turn_context = Arc::clone(&step_context.turn);\n    let base_instructions = sess.get_base_instructions().await;\n    let pooled_execution = execution_auth.multi_account_enabled();",
)
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "    let mut retry_state = ResponsesStreamRetryState::default();\n    let mut initial_input = Some(input);",
    "    let mut retry_state = ResponsesStreamRetryState::default();\n    let mut initial_input = if pooled_execution { None } else { Some(input) };",
)

# Each concrete request captures one immutable lease, request provenance, a durable-history cursor,
# and request-local visible-output checkpoint. Pooled requests are rebuilt from annotated history.
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "    loop {\n        let prompt_input = if let Some(input) = initial_input.take() {\n            input\n        } else {\n            sess.clone_history()\n                .await\n                .for_prompt(&step_context.model_info.input_modalities)\n        };",
    "    loop {\n        let execution_lease = execution_auth.active_lease().ok_or_else(|| {\n            CodexErr::UnsupportedOperation(\n                \"no schedulable Codex execution account is available\".to_string(),\n            )\n        })?;\n        set_sampling_execution_provenance(&turn_context, execution_lease.clone());\n\n        let history_before = sess.clone_history().await;\n        let history_cursor = pooled_execution.then(|| {\n            SamplingHistoryCursor::from_history(\n                history_before.history_version(),\n                history_before.annotated_items(),\n            )\n        });\n        let attempt_state = pooled_execution.then(|| install_sampling_attempt(&turn_context));\n\n        let prompt_input = if let Some(input) = initial_input.take() {\n            input\n        } else if pooled_execution {\n            let transition = AccountHistoryTransition::pooled(\n                &execution_lease,\n                execution_auth\n                    .legacy_unattributed_profile_id()\n                    .map(|id| id.as_str().to_string()),\n            );\n            let annotated = history_before\n                .clone()\n                .for_prompt_annotated(&step_context.model_info.input_modalities);\n            transition\n                .prepare_for_request(annotated)\n                .map(|(items, _stats)| items)\n                .map_err(|err| CodexErr::UnsupportedOperation(err.to_string()))?\n        } else {\n            history_before.for_prompt(&step_context.model_info.input_modalities)\n        };",
)

# ToolRuntime must follow the freshly recaptured StepContext after a switch.
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "        let err = match try_run_sampling_request(\n            tool_runtime.clone(),",
    "        let tool_runtime = ToolCallRuntime::new(\n            Arc::clone(&sess),\n            Arc::clone(&step_context),\n            Arc::clone(&turn_diff_tracker),\n        );\n        let err = match try_run_sampling_request(\n            tool_runtime,",
)

# Replace the quota-only early return with checkpoint-aware quota/auth failover. The account pool is
# rotated only from the exact failed lease. A successful switch gets a fresh ModelClientSession,
# fresh step context and history-derived prompt; no old WebSocket/x-codex-turn-state survives.
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "            Err(err) => match err.details() {\n                CodexErrorDetails::ContextWindowExceeded => {\n                    sess.set_total_tokens_full(&turn_context).await;\n                    return Err(err);\n                }\n                CodexErrorDetails::UsageLimitReached(e) => {\n                    let rate_limits = e.rate_limits.clone();\n                    if let Some(rate_limits) = rate_limits {\n                        sess.update_rate_limits(&turn_context, *rate_limits).await;\n                    }\n                    return Err(err);\n                }\n                _ => err,\n            },\n        };\n\n        if original_input.is_none() {",
    "            Err(err) => {\n                if matches!(err.details(), CodexErrorDetails::ContextWindowExceeded) {\n                    sess.set_total_tokens_full(&turn_context).await;\n                    return Err(err);\n                }\n                if let CodexErrorDetails::UsageLimitReached(limit) = err.details()\n                    && let Some(rate_limits) = limit.rate_limits.clone()\n                {\n                    sess.update_rate_limits(&turn_context, *rate_limits).await;\n                }\n\n                let account_failure = matches!(\n                    err.details(),\n                    CodexErrorDetails::UsageLimitReached(_)\n                        | CodexErrorDetails::RefreshTokenFailed(_)\n                );\n                if pooled_execution && account_failure {\n                    let history_after = sess.clone_history().await;\n                    let mut checkpoint = attempt_state\n                        .as_ref()\n                        .map_or_default(crate::sampling_attempt::SamplingAttemptState::snapshot);\n                    if let Some(cursor) = history_cursor.as_ref() {\n                        checkpoint.merge(cursor.checkpoint(\n                            history_after.history_version(),\n                            history_after.annotated_items(),\n                        ));\n                    }\n\n                    match handle_sampling_failover(\n                        execution_auth.as_ref(),\n                        &execution_lease,\n                        &checkpoint,\n                        &err,\n                    )\n                    .map_err(CodexErr::from)?\n                    {\n                        SamplingFailoverDirective::ReplayCurrentSamplingRequest { .. }\n                        | SamplingFailoverDirective::ContinueFromDurableHistory { .. } => {\n                            // Make the outer provider observe the selected pool identity now rather\n                            // than racing the background auth-sync task.\n                            execution_auth.compatibility_auth_manager().reload().await;\n                            // A new turn-scoped session drops the old WebSocket, previous-response\n                            // state and x-codex-turn-state before the next account sends anything.\n                            *client_session = sess.services.model_client.new_session();\n                            retry_state = ResponsesStreamRetryState::default();\n                            initial_input = None;\n                            sess.refresh_mcp_if_dirty().await;\n                            step_context = sess\n                                .capture_step_context(\n                                    Arc::clone(&turn_context),\n                                    &cancellation_token,\n                                )\n                                .await?;\n                            turn_context.turn_timing_state.record_sampling_retry();\n                            continue;\n                        }\n                        SamplingFailoverDirective::PoolExhausted\n                        | SamplingFailoverDirective::ReconcileCurrentAttempt\n                        | SamplingFailoverDirective::NotHandled => return Err(err),\n                    }\n                }\n                err\n            }\n        };\n\n        if original_input.is_none() {",
)

# Tool results belong to the same sampling lease as the model tool call that caused them.
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "                sess.record_conversation_items(&turn_context, std::slice::from_ref(&response_item))\n                    .await;",
    "                record_conversation_items_with_execution_provenance(\n                    sess.as_ref(),\n                    turn_context.as_ref(),\n                    std::slice::from_ref(&response_item),\n                )\n                .await;",
)

# Request-local partial UI progress. Durable progress is reconstructed from history instead.
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "        match event {\n            ResponseEvent::Created => {}",
    "        match event {\n            ResponseEvent::Created => mark_sampling_response_started(&turn_context)",
)
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "                    if stream_item_to_client {\n                        if let Some(state) = plan_mode_state.as_mut()",
    "                    if stream_item_to_client {\n                        mark_sampling_visible_output(&turn_context);\n                        if let Some(state) = plan_mode_state.as_mut()",
)

# Once more than one execution profile is available, do not create new opaque remote compaction
# checkpoints. Token-budget compaction is already local/non-model and remains unchanged.
replace_once(
    "codex-rs/core/src/session/turn.rs",
    "    if turn_context.config.features.enabled(Feature::TokenBudget) {\n        // Compaction is the reset request, so force a new context window\n        // instead of consuming a pending `new_context` tool request.\n        crate::compact_token_budget::run_inline_auto_compact_task(\n            Arc::clone(sess),\n            step_context,\n            initial_context_injection,\n        )\n        .await?;\n        return Ok(());\n    }\n\n    match turn_context.provider.capabilities().remote_compaction {",
    "    if turn_context.config.features.enabled(Feature::TokenBudget) {\n        // Compaction is the reset request, so force a new context window\n        // instead of consuming a pending `new_context` tool request.\n        crate::compact_token_budget::run_inline_auto_compact_task(\n            Arc::clone(sess),\n            step_context,\n            initial_context_injection,\n        )\n        .await?;\n        return Ok(());\n    }\n\n    let execution_auth = ExecutionAuth::shared(Arc::clone(&sess.services.auth_manager));\n    if crate::portable_compaction::requires_portable_compaction(execution_auth.as_ref()) {\n        emit_compact_metric(\n            &sess.services.session_telemetry,\n            \"local_multi_account\",\n            /*manual*/ false,\n        );\n        run_inline_auto_compact_task(\n            Arc::clone(sess),\n            Arc::clone(turn_context),\n            initial_context_injection,\n            reason,\n            phase,\n        )\n        .await?;\n        return Ok(());\n    }\n\n    match turn_context.provider.capabilities().remote_compaction {",
)

print("native multi-account live wiring patch applied successfully")
