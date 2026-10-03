//! Explicit, turn-scoped API targets outside subscription quota scheduling.

use codex_async_utils::OrCancelExt;
use std::sync::Arc;
use std::time::Duration;

use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_login::ApiAccountSelection;
use codex_login::ApiAccountStore;
use codex_model_provider::SharedModelProvider;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ToolMode;
use tokio_util::sync::CancellationToken;

use crate::account_pool_recovery::RecoveryWaitBudget;
use crate::account_transition::AccountHistoryTransition;
use crate::account_transition::HistoryItemOwnership;
use crate::account_transition::history_item_ownership;
use crate::client::ModelClientSession;
use crate::config::Config;
use crate::execution_auth::ExecutionAuth;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;

const API_PROVIDER_PREFIX: &str = "api-account:";

/// Credentials, deployment and capabilities captured together before any API request.
/// The provider has no subscription AuthManager, environment credentials or workspace routing.
#[derive(Clone, Debug)]
pub(crate) struct ApiExecutionTarget {
    pub(crate) profile_id: String,
    pub(crate) provider_id: String,
    pub(crate) provider: SharedModelProvider,
    pub(crate) model_info: Arc<ModelInfo>,
}

/// One explicit selection and optional paid-fallback permission, frozen for the user turn.
pub(crate) struct ApiTurnPolicy {
    pub(crate) selected: Option<Arc<ApiExecutionTarget>>,
    pub(crate) fallback: Option<Arc<ApiExecutionTarget>>,
    pub(crate) max_subscription_wait: Duration,
}

impl ApiTurnPolicy {
    pub(crate) fn capture(config: &Config) -> Result<Self> {
        // Descendants inherit the parent's captured paid target and model. Never send a reviewer
        // or subagent's preferred GPT model to a provider that only authorized one explicit model.
        if let Some(profile_id) = config.model_provider_id.strip_prefix(API_PROVIDER_PREFIX) {
            ensure_api_allowed(config)?;
            let model_info = config
                .model_catalog
                .as_ref()
                .and_then(|catalog| catalog.models.first())
                .ok_or_else(|| {
                    CodexErr::InvalidRequest(
                        "The inherited API target has no selected model.".into(),
                    )
                })?;
            let provider_info = config.model_provider.clone();
            if provider_info.requires_openai_auth
                || provider_info.env_key.is_some()
                || provider_info.auth.is_some()
                || provider_info.experimental_bearer_token.is_none()
            {
                return Err(CodexErr::InvalidRequest(
                    "The inherited API target has no captured API key.".into(),
                ));
            }
            return Ok(Self {
                selected: Some(Arc::new(ApiExecutionTarget {
                    profile_id: profile_id.to_string(),
                    provider_id: config.model_provider_id.clone(),
                    provider: create_model_provider(provider_info, /*auth_manager*/ None),
                    model_info: Arc::new(model_info.clone()),
                })),
                fallback: None,
                max_subscription_wait: Duration::ZERO,
            });
        }
        let store = ApiAccountStore::new(
            config.codex_home.to_path_buf(),
            config.cli_auth_credentials_store_mode,
            config.auth_keyring_backend_kind(),
        );
        let state = store.load()?;
        let selected = match &state.selection {
            ApiAccountSelection::Subscription => None,
            ApiAccountSelection::Manual { profile_id } => Some(Arc::new(
                ApiExecutionTarget::capture(&store, profile_id, config)?,
            )),
        };
        let fallback = if state.fallback.enabled && selected.is_none() {
            match state
                .fallback
                .profile_id
                .as_deref()
                .and_then(|profile_id| ApiExecutionTarget::capture(&store, profile_id, config).ok())
            {
                Some(target) => Some(Arc::new(target)),
                None => {
                    tracing::warn!(
                        "configured API fallback is unavailable; subscription execution remains enabled"
                    );
                    None
                }
            }
        } else {
            None
        };
        let max_subscription_wait = if fallback.is_some() {
            Duration::from_secs(state.fallback.wait_minutes.min(1_440).saturating_mul(60))
        } else {
            config.account_pool.effective_reset_wait()
        };
        Ok(Self {
            selected,
            fallback,
            max_subscription_wait,
        })
    }
}

impl ApiExecutionTarget {
    fn capture(store: &ApiAccountStore, profile_id: &str, config: &Config) -> Result<Self> {
        ensure_api_allowed(config)?;
        let (account, key) = store.capture_target(profile_id)?;
        let provider_info = ModelProviderInfo {
            name: "API account".into(),
            base_url: Some(account.base_url.clone()),
            experimental_bearer_token: Some(key.into()),
            requires_openai_auth: false,
            supports_websockets: false,
            ..Default::default()
        };
        let mut model_info = codex_models_manager::model_info::model_info_from_slug(&account.model);
        apply_api_capabilities(&mut model_info, account.context_window, account.images);
        Ok(Self {
            profile_id: profile_id.to_string(),
            provider_id: format!("{API_PROVIDER_PREFIX}{profile_id}"),
            provider: create_model_provider(provider_info, /*auth_manager*/ None),
            model_info: Arc::new(model_info),
        })
    }
}

pub(crate) fn ensure_api_allowed(config: &Config) -> Result<()> {
    if !config
        .auth_config()
        .is_login_method_allowed(ForcedLoginMethod::Api)
        || config.forced_chatgpt_workspace_id.is_some()
    {
        return Err(CodexErr::InvalidRequest(
            "Managed authentication requirements do not allow API accounts.".into(),
        ));
    }
    Ok(())
}

fn apply_api_capabilities(model_info: &mut ModelInfo, context_window: i64, images: bool) {
    model_info.context_window = Some(context_window);
    model_info.max_context_window = Some(context_window);
    model_info.auto_compact_token_limit = Some(context_window.saturating_mul(9) / 10);
    model_info.input_modalities = if images {
        vec![InputModality::Text, InputModality::Image]
    } else {
        vec![InputModality::Text]
    };
    model_info.supports_reasoning_summary_parameter = false;
    model_info.support_verbosity = false;
    model_info.default_reasoning_level = None;
    model_info.default_verbosity = None;
    model_info.apply_patch_tool_type = None;
    model_info.default_reasoning_summary = ReasoningSummary::None;
    model_info.node_repl_disabled = true;
    model_info.tool_mode = Some(ToolMode::Direct);
    model_info.used_fallback_model_metadata = false;
}

/// Treat every API boundary as a foreign account boundary, including legacy unattributed state.
/// Only the request clone changes; durable tool results and source-account history stay intact.
pub(crate) fn project_api_history(
    mut history: Vec<ResponseItemEnvelope>,
) -> Result<Vec<ResponseItem>> {
    for envelope in &mut history {
        if history_item_ownership(envelope) == HistoryItemOwnership::LegacyRootScoped {
            envelope
                .metadata
                .get_or_insert_with(CodexHarnessMetadata::default)
                .execution_profile_id = Some("subscription-unattributed".into());
        }
    }
    AccountHistoryTransition::stock()
        .prepare_for_request(history)
        .map(|(items, _)| items)
        .map_err(|error| CodexErr::AccountMigrationRequired(error.to_string()))
}

/// Applies a paid transition after a safe checkpoint, then rebuilds the request's tool snapshot.
pub(crate) async fn activate_api_target(
    sess: &Arc<Session>,
    turn: &Arc<TurnContext>,
    target: &Arc<ApiExecutionTarget>,
    client_session: &mut ModelClientSession,
    cancellation: &CancellationToken,
) -> Result<Arc<StepContext>> {
    project_api_history(
        sess.clone_history()
            .await
            .for_prompt_annotated(&target.model_info.input_modalities),
    )?;
    let rebound = Arc::new(turn.with_api_target(target)?);
    rebound
        .extension_data
        .remove::<crate::execution_provenance::SamplingExecutionProvenance>();
    rebound.extension_data.remove::<codex_api::ResponseId>();
    rebound.extension_data.insert(target.as_ref().clone());
    sess.services
        .model_client
        .replace_session_for_api_target(client_session, target);
    let step = sess
        .capture_step_context(Arc::clone(&rebound), cancellation)
        .await?;
    sess.record_context_updates_and_set_reference_context_item(step.as_ref())
        .await?;
    sess.send_event(&rebound, EventMsg::Warning(WarningEvent {
        message: format!("Subscription quota recovery is exhausted. Continuing this turn on API account `{}` with model `{}`; API usage may be billed.", target.profile_id, target.model_info.slug),
    })).await;
    Ok(step)
}

/// Reuses the turn's cumulative subscription waiting allowance before an explicitly paid target.
/// Authentication and entitlement failures never authorize this path.
pub(crate) async fn fallback_after_pool_exhaustion(
    execution_auth: &ExecutionAuth,
    turn: &TurnContext,
    cancellation: &CancellationToken,
) -> Result<Option<Arc<ApiExecutionTarget>>> {
    let Some(policy) = turn.extension_data.get::<ApiTurnPolicy>() else {
        return Ok(None);
    };
    let Some(target) = &policy.fallback else {
        return Ok(None);
    };
    if !eligible_quota_fallback(execution_auth, turn.config.as_ref())
        .or_cancel(cancellation)
        .await?
    {
        return Ok(None);
    }
    let budget = turn
        .extension_data
        .get_or_init(|| RecoveryWaitBudget::new(policy.max_subscription_wait));
    if !budget.remaining().is_zero()
        && crate::account_pool_recovery::wait_for_recovery(
            execution_auth,
            turn.config.as_ref(),
            &budget,
            cancellation,
        )
        .await
    {
        return Ok(None);
    }
    if cancellation.is_cancelled() {
        return Err(CodexErr::TurnAborted);
    }
    // Waiting can be interrupted by a concurrent selection even at the deadline.
    if !eligible_quota_fallback(execution_auth, turn.config.as_ref())
        .or_cancel(cancellation)
        .await?
    {
        return Ok(None);
    }
    Ok(Some(Arc::clone(target)))
}

async fn eligible_quota_fallback(execution_auth: &ExecutionAuth, config: &Config) -> bool {
    if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
        return false;
    }
    let Some(pool) = execution_auth.account_pool() else {
        return false;
    };
    let store = codex_login::AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let state = loop {
        if tokio::time::Instant::now() >= deadline
            || codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home)
        {
            return false;
        }
        match store.try_synchronize(&pool) {
            Ok(true) => match store.try_load() {
                Ok(Some(state)) => break state,
                Ok(None) => {}
                Err(_) => return false,
            },
            Ok(false) => {}
            Err(_) => return false,
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    if execution_auth.active_lease().is_some() {
        return false;
    }
    pool.snapshots().iter().any(|snapshot| {
        matches!(
            snapshot.availability,
            codex_login::AccountAvailability::Exhausted { .. }
        ) && !snapshot.profile.disabled
            && !state.profiles.iter().any(|profile| {
                profile.profile_id == snapshot.profile.id
                    && profile
                        .reset_credit_excluded_until
                        .is_some_and(|until| until > chrono::Utc::now())
            })
    })
}

#[cfg(test)]
#[path = "api_account_execution_tests.rs"]
mod tests;
