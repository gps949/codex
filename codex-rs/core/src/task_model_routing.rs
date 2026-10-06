//! Task-boundary routing retains manual pins, provider ownership and supported model metadata.

mod media;

pub(crate) use media::estimate_fresh_input_tokens;
pub(crate) use media::retained_media_requirements;

use crate::config::Config;
use codex_config::ModelRoutingMode;
use codex_config::ModelRoutingRole;
use codex_config::ModelRoutingSource;
use codex_model_provider::RoutingCandidate;
use codex_model_provider::RoutingModelRole;
use codex_model_provider::RoutingRequest;
use codex_model_provider::RoutingSelection;
use codex_model_provider::RoutingTaskComplexity;
use codex_model_provider::choose_task_model;
use codex_model_provider::classify_task_locally;
use codex_model_provider::decision_advisor;
use codex_model_provider::default_routing_role;
use codex_models_manager::manager::ModelsManager;
use codex_protocol::openai_models::ReasoningEffort;
use std::str::FromStr;

#[derive(Clone, Copy)]
pub(crate) enum RoutingScope {
    Main,
    Subagent,
}

pub(crate) struct TaskRoutingInput<'a> {
    pub(crate) task: &'a str,
    pub(crate) current_model: &'a str,
    pub(crate) current_effort: Option<ReasoningEffort>,
    pub(crate) service_tier: Option<&'a str>,
    pub(crate) required_context_tokens: i64,
    pub(crate) requires_images: bool,
    pub(crate) scope: RoutingScope,
}

pub(crate) struct TaskRoutingDecision {
    pub(crate) selection: RoutingSelection,
    pub(crate) apply: bool,
    pub(crate) source: String,
}

/// Runs once for admitted root tasks or eligible fresh/partial child creation.
/// Recovery and steering callers must reuse the originally admitted selection.
pub(crate) async fn decide_task_model(
    config: &Config,
    models: &dyn ModelsManager,
    input: TaskRoutingInput<'_>,
) -> Option<TaskRoutingDecision> {
    let policy = config.model_routing_snapshot().await.ok()?;
    if policy.mode == ModelRoutingMode::Off
        || input.task.trim().is_empty()
        || input.task.len() > 2048
        || matches!(input.scope, RoutingScope::Main) && !policy.main_tasks
        || matches!(input.scope, RoutingScope::Subagent) && !policy.subagents
    {
        return None;
    }
    // Independent API selections have captured targets and their own explicit consent.
    let api = codex_login::ApiAccountStore::new(
        config.codex_home.to_path_buf(),
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
    )
    .try_load()
    .ok()?;
    if api.as_ref().is_some_and(|state| {
        matches!(
            state.selection,
            codex_login::ApiAccountSelection::Manual { .. }
        )
    }) {
        return None;
    }
    // Do not add discovery requests solely to classify a task. The identity-aware manager
    // supplies the current catalog or an empty snapshot when it cannot establish one.
    let catalog = models
        .verified_model_catalog(config.http_client_factory())
        .await?;
    let available = models.build_available_models(catalog.models.clone());
    let candidates = catalog.models.into_iter().filter(|model| {
        available
            .iter()
            .any(|preset| preset.model == model.slug && preset.show_in_picker)
            && !model.used_fallback_model_metadata
            && model.model_specialty.is_none()
    });
    // The explicit filter below also handles child backend and retained service tier.
    let candidates = candidates
        .filter(|model| {
            if matches!(input.scope, RoutingScope::Subagent)
                && model.multi_agent_version
                    == Some(codex_protocol::protocol::MultiAgentVersion::Disabled)
            {
                return false;
            }
            input
                .service_tier
                .is_none_or(|tier| tier == "default" || model.supports_service_tier(tier))
        })
        .filter_map(|model| {
            let role = policy
                .model_roles
                .get(&model.slug)
                .map(|role| match role {
                    ModelRoutingRole::Economy => RoutingModelRole::Economy,
                    ModelRoutingRole::Balanced => RoutingModelRole::Balanced,
                    ModelRoutingRole::Capability => RoutingModelRole::Capability,
                })
                .or_else(|| default_routing_role(&model))?;
            Some(RoutingCandidate { model, role })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    let max_effort = ReasoningEffort::from_str(&policy.max_effort).ok()?;
    let request = RoutingRequest {
        task: input.task,
        current_model: input.current_model,
        current_effort: input.current_effort,
        preference: policy.preference,
        candidates: &candidates,
        allowed_models: &policy.allowed_models,
        required_context_tokens: input.required_context_tokens,
        requires_images: input.requires_images,
        max_effort,
    };
    // Never pay for classification when no compatible selection can be made.
    choose_task_model(&request, RoutingTaskComplexity::Simple)?;
    let (complexity, source) = match policy.source {
        ModelRoutingSource::Local => (classify_task_locally(input.task), "Local rules"),
        ModelRoutingSource::DecisionService => {
            if !policy.send_task_description {
                return None;
            }
            let advised = async {
                let snapshot = config.decision_advisor_snapshot().await.map_err(|_| ())?;
                let settings = snapshot.effective;
                let credential = config
                    .decision_advisor_credential(&settings)
                    .map_err(|_| ())?;
                decision_advisor()
                    .assess_task(
                        &settings,
                        &config.http_client_factory(),
                        input.task,
                        credential
                            .as_ref()
                            .map(codex_model_provider::DecisionAdvisorSecret::expose_secret),
                    )
                    .await
                    .map_err(|_| ())
            }
            .await;
            match advised {
                Ok(complexity) => (complexity, "Decision service"),
                Err(_) if policy.local_fallback => {
                    (classify_task_locally(input.task), "Local fallback")
                }
                Err(_) => return None,
            }
        }
    };
    let selection = choose_task_model(&request, complexity)?;
    Some(TaskRoutingDecision {
        selection,
        apply: policy.mode == ModelRoutingMode::Automatic,
        source: source.into(),
    })
}
