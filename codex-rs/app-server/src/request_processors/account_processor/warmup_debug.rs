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

    pub(super) async fn account_pool_warmup_debug_response(
        &self,
        params: AccountPoolWarmupDebugParams,
    ) -> Result<AccountPoolWarmupDebugResponse, JSONRPCErrorError> {
        let config = self.load_latest_config().await;
        let pool_enabled = self
            .execution_account_pool
            .ensure_from_config(&config)
            .await
            .map_err(|err| internal_error(format!("failed to initialize account pool: {err}")))?;

        if pool_enabled && config.account_pool.effective_window_warmup() {
            self.profile_routing_owners
                .synchronize(self.execution_account_pool.auth_managers())
                .await;
        }
        let enabled = pool_enabled && config.account_pool.effective_window_warmup();
        if pool_enabled && let Some(pool) = self.execution_account_pool.account_pool() {
            let store = codex_login::AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
            let _ = store.apply_window_warmup_to_pool(&pool);
            let _ = codex_login::recover_pool_auth_from_disk(pool.as_ref()).await;
        }

        let pass_requested = enabled
            && params.run_now
            && self
                .execution_account_pool
                .request_window_warmup_pass_now(&config);

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
        if pool_enabled {
            for snapshot in self.execution_account_pool.snapshots() {
                let (_plan_type, email) = self.load_pool_profile_identity(&snapshot).await;
                let is_candidate = enabled
                    && candidate_ids
                        .iter()
                        .any(|id| id == snapshot.profile.id.as_str());
                let now = chrono::Utc::now();
                let status = snapshot.window_warmup.as_ref().and_then(|observation| {
                    codex_login::visible_window_warmup_status(
                        observation,
                        snapshot
                            .rate_limits
                            .primary
                            .as_ref()
                            .map(|window| window.used_percent),
                        now,
                    )
                });
                let candidate_reason = match &snapshot.availability {
                    AccountAvailability::Disabled => "account disabled",
                    AccountAvailability::AuthenticationUnavailable { .. } => "login required",
                    AccountAvailability::Exhausted { .. } => "quota cooldown",
                    AccountAvailability::Available if snapshot.is_active => {
                        "current execution account"
                    }
                    AccountAvailability::Available if !enabled => "automatic warmup is off",
                    AccountAvailability::Available if is_candidate => {
                        "eligible for quota check and warmup"
                    }
                    AccountAvailability::Available
                        if snapshot.window_warmup.as_ref().is_some_and(|observation| {
                            observation.retry_after.is_some_and(|retry| retry > now)
                        }) =>
                    {
                        "attempt protected; quota-only confirmation or retry later"
                    }
                    AccountAvailability::Available
                        if snapshot.rate_limits.primary.as_ref().is_some_and(|window| {
                            window.used_percent > 0.0
                                && window.resets_at.is_none_or(|reset| reset > now)
                        }) =>
                    {
                        "primary window already has usage"
                    }
                    AccountAvailability::Available
                        if snapshot
                            .rate_limits
                            .secondary
                            .as_ref()
                            .is_some_and(|window| {
                                window.used_percent >= 100.0
                                    && window.resets_at.is_none_or(|reset| reset > now)
                            }) =>
                    {
                        "weekly quota exhausted"
                    }
                    AccountAvailability::Available => "no generating request needed",
                };
                accounts.push(AccountPoolWarmupDebugAccount {
                    profile_id: snapshot.profile.id.to_string(),
                    email,
                    label: snapshot.profile.label.clone(),
                    priority: snapshot.profile.priority,
                    is_active: snapshot.is_active,
                    is_candidate,
                    availability: availability_label(&snapshot.availability).to_string(),
                    primary_used_percent: snapshot
                        .rate_limits
                        .primary
                        .as_ref()
                        .map(|window| window.used_percent),
                    attempted_at: snapshot
                        .window_warmup
                        .as_ref()
                        .map(|observation| observation.attempted_at.timestamp()),
                    retry_after: snapshot.window_warmup.as_ref().and_then(|observation| {
                        observation.retry_after.map(|retry| retry.timestamp())
                    }),
                    status,
                    candidate_reason: Some(candidate_reason.to_string()),
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
            pool_enabled: Some(pool_enabled),
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
