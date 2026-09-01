use std::sync::Arc;

use super::SessionTask;
use super::SessionTaskResult;
use super::emit_compact_metric;
use crate::execution_auth::ExecutionAuth;
use crate::portable_compaction::PortableCompactionPolicy;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use codex_features::Feature;
use codex_model_provider::RemoteCompactionSupport;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Default)]
pub(crate) struct CompactTask;

impl SessionTask for CompactTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Compact
    }

    fn span_name(&self) -> &'static str {
        "session_task.compact"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        _cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        let _profile_guard = ctx.turn_timing_state.begin_compaction();
        if ctx.config.features.enabled(Feature::TokenBudget) {
            crate::compact_token_budget::run_manual_compact_task(session, ctx).await?;
            return Ok(None);
        }

        let execution_auth = ExecutionAuth::shared(Arc::clone(&session.services.auth_manager));
        let execution_auth_mode = execution_auth
            .mode_for_turn(ctx.config.as_ref(), ctx.provider.info())
            .await
            .map_err(|err| {
                CodexErr::UnsupportedOperation(format!(
                    "failed to initialize native multi-account execution for compaction: {err}"
                ))
            })?;
        let history = session.clone_history().await;
        let portable_policy =
            PortableCompactionPolicy::for_history(&execution_auth_mode, history.annotated_items());

        let result = if portable_policy == PortableCompactionPolicy::Portable {
            emit_compact_metric(
                &session.services.session_telemetry,
                "local_multi_account",
                /*manual*/ true,
            );
            run_local_compact(Arc::clone(&session), Arc::clone(&ctx)).await
        } else {
            match ctx.provider.capabilities().remote_compaction {
                RemoteCompactionSupport::V2
                    if ctx.config.features.enabled(Feature::RemoteCompactionV2) =>
                {
                    emit_compact_metric(
                        &session.services.session_telemetry,
                        "remote_v2",
                        /*manual*/ true,
                    );
                    crate::compact_remote_v2::run_remote_compact_task(
                        session.clone(),
                        Arc::clone(&ctx),
                    )
                    .await
                }
                RemoteCompactionSupport::V2 => {
                    emit_compact_metric(
                        &session.services.session_telemetry,
                        "remote",
                        /*manual*/ true,
                    );
                    crate::compact_remote::run_remote_compact_task(
                        session.clone(),
                        Arc::clone(&ctx),
                    )
                    .await
                }
                RemoteCompactionSupport::Unsupported => {
                    emit_compact_metric(
                        &session.services.session_telemetry,
                        "local",
                        /*manual*/ true,
                    );
                    run_local_compact(Arc::clone(&session), Arc::clone(&ctx)).await
                }
            }
        };
        match result {
            Ok(()) => {}
            Err(err) if matches!(err.details(), CodexErrorDetails::TurnAborted) => {
                return Err(err);
            }
            Err(err)
                if portable_policy == PortableCompactionPolicy::Portable
                    && matches!(err.details(), CodexErrorDetails::UnsupportedOperation(_)) =>
            {
                session.track_turn_codex_error(ctx.as_ref(), &err);
                session
                    .send_event(
                        ctx.as_ref(),
                        EventMsg::Error(err.to_error_event(/*message_prefix*/ None)),
                    )
                    .await;
            }
            Err(_) => {}
        }
        Ok(None)
    }
}

async fn run_local_compact(session: Arc<Session>, ctx: Arc<TurnContext>) -> Result<(), CodexErr> {
    let input = vec![UserInput::Text {
        text: ctx
            .config
            .compact_prompt
            .as_deref()
            .unwrap_or(crate::compact::SUMMARIZATION_PROMPT)
            .to_string(),
        // Compaction prompt is synthesized; no UI element ranges to preserve.
        text_elements: Vec::new(),
    }];
    crate::compact::run_compact_task(session, ctx, input).await
}
