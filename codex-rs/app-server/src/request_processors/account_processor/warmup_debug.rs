//! `accountPool/warmupDebug` — screenshotable dump for the TUI `/warmup` command.

use super::AccountRequestProcessor;
use crate::error_code::internal_error;
use codex_app_server_protocol::AccountPoolWarmupDebugAccount;
use codex_app_server_protocol::AccountPoolWarmupDebugEvent;
use codex_app_server_protocol::AccountPoolWarmupDebugParams;
use codex_app_server_protocol::AccountPoolWarmupDebugResponse;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_config::AccountPoolRotationStrategy;
use codex_core::ExecutionAccountPoolHandle;
use codex_login::AccountAvailability;
use codex_login::WindowWarmupOutcome;
use codex_login::window_warmup_debug_events;

impl AccountRequestProcessor {
    pub(crate) async fn get_account_pool_warmup_debug(
        &self,
        params: AccountPoolWarmupDebugParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Ok(Some(
            self.account_pool_warmup_debug_response(params)
                .await?
                .into(),
        ))
    }

    async fn account_pool_warmup_debug_response(
        &self,
        params: AccountPoolWarmupDebugParams,
    ) -> Result<AccountPoolWarmupDebugResponse, JSONRPCErrorError> {
        let config = self.load_latest_config().await;
        let enabled = self
            .execution_account_pool
            .ensure_from_config(&config)
            .await
            .map_err(|err| internal_error(format!("failed to initialize account pool: {err}")))?;

        let mut pass_requested = false;
        if enabled && params.run_now {
            self.execution_account_pool
                .request_window_warmup_pass_now(&config);
            pass_requested = true;
        }

        let candidate_ids = self
            .execution_account_pool
            .account_pool()
            .map(|pool| {
                pool.window_warmup_candidates()
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut accounts = Vec::new();
        if enabled {
            if let Some(pool) = self.execution_account_pool.account_pool() {
                let _ = codex_login::recover_pool_auth_from_disk(pool.as_ref()).await;
            }
            for snapshot in self.execution_account_pool.snapshots() {
                let (_plan_type, email) = self.load_pool_profile_identity(&snapshot).await;
                accounts.push(AccountPoolWarmupDebugAccount {
                    profile_id: snapshot.profile.id.to_string(),
                    email,
                    label: snapshot.profile.label.clone(),
                    priority: snapshot.profile.priority,
                    is_active: snapshot.is_active,
                    is_candidate: candidate_ids
                        .iter()
                        .any(|id| id == snapshot.profile.id.as_str()),
                    availability: availability_label(&snapshot.availability).to_string(),
                    primary_used_percent: snapshot
                        .rate_limits
                        .primary
                        .as_ref()
                        .map(|window| window.used_percent),
                    persisted_warmup_outcome: snapshot
                        .window_warmup
                        .as_ref()
                        .map(|observation| outcome_label(observation.outcome).to_string()),
                });
            }
        }

        let events = window_warmup_debug_events()
            .into_iter()
            .map(|event| AccountPoolWarmupDebugEvent {
                at: event.at.timestamp(),
                message: event.message,
            })
            .collect();

        Ok(AccountPoolWarmupDebugResponse {
            enabled,
            task_running: self.execution_account_pool.window_warmup_task_running(),
            pass_requested,
            interval_seconds: config
                .account_pool
                .effective_window_warmup_interval()
                .as_secs(),
            settle_seconds: ExecutionAccountPoolHandle::window_warmup_settle_seconds(),
            rotation_strategy: rotation_strategy_label(
                config.account_pool.effective_rotation_strategy(),
            )
            .to_string(),
            session_model: config.model.clone(),
            accounts,
            events,
        })
    }
}

fn rotation_strategy_label(strategy: AccountPoolRotationStrategy) -> &'static str {
    match strategy {
        AccountPoolRotationStrategy::FillFirst => "fillFirst",
        AccountPoolRotationStrategy::EarliestReset => "earliestReset",
    }
}

fn availability_label(availability: &AccountAvailability) -> &'static str {
    match availability {
        AccountAvailability::Available => "available",
        AccountAvailability::Exhausted { .. } => "exhausted",
        AccountAvailability::AuthenticationUnavailable { .. } => "authUnavailable",
        AccountAvailability::Disabled => "disabled",
    }
}

fn outcome_label(outcome: WindowWarmupOutcome) -> &'static str {
    match outcome {
        WindowWarmupOutcome::Succeeded => "succeeded",
        WindowWarmupOutcome::Failed => "failed",
        WindowWarmupOutcome::SkippedNoAuth => "skippedNoAuth",
    }
}
