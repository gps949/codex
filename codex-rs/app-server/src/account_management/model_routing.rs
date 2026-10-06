//! Local configuration and catalog previews for task-boundary model selection.

use super::*;
use codex_config::ModelRoutingConfigToml;
use codex_config::ModelRoutingMode;
use codex_config::ModelRoutingSource;
use codex_model_provider::RoutingCandidate;
use codex_model_provider::RoutingModelRole;
use codex_model_provider::RoutingRequest;
use codex_model_provider::choose_task_model;
use codex_model_provider::classify_task_locally;
use codex_model_provider::default_routing_role;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use std::str::FromStr;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoutingView {
    pub config: ModelRoutingConfigToml,
    pub effective_config: ModelRoutingConfigToml,
    pub overridden: bool,
    pub user_config_version: String,
    pub models: Vec<ModelRoutingModelView>,
    pub last_decision: Option<serde_json::Value>,
    pub decision_service_ready: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoutingModelView {
    pub model: String,
    pub label: String,
    pub role: Option<String>,
    pub efforts: Vec<ReasoningEffort>,
    pub context_window: Option<i64>,
}

impl AccountManager {
    pub async fn model_routing_view(&self) -> anyhow::Result<ModelRoutingView> {
        let (_, config, version) = self.routing_user_config().await?;
        let effective = self.config.model_routing_snapshot().await?;
        let (catalog, _) = self.routing_models(RefreshStrategy::Offline).await?;
        let models = catalog
            .into_iter()
            .map(|model| {
                let role = effective
                    .model_roles
                    .get(&model.slug)
                    .map(|role| serde_json::to_value(role).unwrap_or_default())
                    .or_else(|| {
                        default_routing_role(&model)
                            .map(|role| serde_json::to_value(role).unwrap_or_default())
                    })
                    .and_then(|role| role.as_str().map(str::to_owned));
                ModelRoutingModelView {
                    model: model.slug.clone(),
                    label: model.display_name.clone(),
                    role,
                    efforts: model
                        .supported_reasoning_levels
                        .iter()
                        .map(|preset| preset.effort.clone())
                        .collect(),
                    context_window: model.usable_context_window(),
                }
            })
            .collect();
        let decision_service_ready = self.decision_advisor_view().await.ok().is_some_and(|view| {
            view.effective_config.validate_service().is_ok()
                && view.policy_status == "knownAllowed"
                && (view.credential_present
                    || view.effective_config.allow_local_http
                        && view.effective_config.api_key_env.is_empty())
        });
        let last_decision = tokio::fs::read(
            self.config
                .codex_home
                .join("model-routing-observation.json"),
        )
        .await
        .ok()
        .filter(|bytes| bytes.len() <= 4096)
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Ok(ModelRoutingView {
            overridden: config != effective,
            config,
            effective_config: effective,
            user_config_version: version,
            models,
            last_decision,
            decision_service_ready,
        })
    }

    async fn routing_user_config(
        &self,
    ) -> anyhow::Result<(toml::Value, ModelRoutingConfigToml, String)> {
        let file = self
            .config
            .config_layer_stack
            .get_user_config_file()
            .cloned()
            .unwrap_or_else(|| self.config.codex_home.join(codex_config::CONFIG_TOML_FILE));
        let text = match tokio::fs::read_to_string(file).await {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            text.len() <= 2 * 1024 * 1024,
            "Model routing configuration is oversized"
        );
        let table: toml::Value = toml::from_str(&text)?;
        let value = table
            .get("model_routing")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()?
            .unwrap_or_default();
        let version = codex_config::version_for_toml(&table);
        Ok((table, value, version))
    }

    async fn routing_models(
        &self,
        refresh: RefreshStrategy,
    ) -> anyhow::Result<(Vec<ModelInfo>, String)> {
        let pool =
            codex_login::load_account_pool_for_management(&self.config.auth_config()).await?;
        let auth = pool.identity_lease().ok().map(|lease| lease.auth_manager());
        let provider =
            codex_model_provider::create_model_provider(self.config.model_provider.clone(), auth);
        let models = provider.models_manager(
            self.config.codex_home.to_path_buf(),
            self.config.model_catalog.clone(),
        );
        if refresh == RefreshStrategy::Online {
            models
                .raw_model_catalog(refresh, self.config.http_client_factory())
                .await;
        }
        let Some(catalog) = models
            .verified_model_catalog(self.config.http_client_factory())
            .await
        else {
            return Ok((Vec::new(), self.config.model.clone().unwrap_or_default()));
        };
        let available = models.build_available_models(catalog.models.clone());
        let current = self.config.model.clone().unwrap_or_else(|| {
            available
                .iter()
                .find(|model| model.is_default)
                .or_else(|| available.first())
                .map(|model| model.model.clone())
                .unwrap_or_default()
        });
        Ok((
            catalog
                .models
                .into_iter()
                .filter(|model| {
                    !model.used_fallback_model_metadata
                        && model.model_specialty.is_none()
                        && available
                            .iter()
                            .any(|preset| preset.model == model.slug && preset.show_in_picker)
                })
                .take(32)
                .collect(),
            current,
        ))
    }

    pub(super) async fn refresh_routing_models(&self) -> anyhow::Result<AccountManagerResult> {
        let (models, _) = self.routing_models(RefreshStrategy::Online).await?;
        anyhow::ensure!(
            !models.is_empty(),
            "No verified model catalog was returned. Check the selected account and connection."
        );
        Ok(AccountManagerResult {
            message:
                "Model catalog checked. Available entries may come from the last verified snapshot."
                    .into(),
            data: serde_json::to_value(self.model_routing_view().await?)?,
        })
    }

    pub(super) async fn save_model_routing(
        &self,
        config: ModelRoutingConfigToml,
        expected_version: Option<String>,
    ) -> anyhow::Result<AccountManagerResult> {
        config.validate()?;
        let home = self.config.codex_home.to_path_buf();
        let _lock = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&home)?;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(home.join(".decision-advisor.lock"))?;
            file.lock()?;
            Ok::<_, std::io::Error>(file)
        })
        .await??;
        let (_, _, current_version) = self.routing_user_config().await?;
        anyhow::ensure!(
            expected_version
                .as_ref()
                .is_none_or(|version| version == &current_version),
            "Model routing settings changed. Reload before saving."
        );
        if config.mode != ModelRoutingMode::Off
            && config.source == ModelRoutingSource::DecisionService
        {
            let snapshot = self.config.decision_advisor_snapshot().await?;
            snapshot
                .effective
                .validate_service()
                .map_err(anyhow::Error::msg)?;
            anyhow::ensure!(
                self.config
                    .decision_advisor_credential(&snapshot.effective)?
                    .is_some()
                    || snapshot.effective.allow_local_http
                        && snapshot.effective.api_key_env.is_empty(),
                "Configure a decision service token before enabling task routing."
            );
        }
        let document: toml_edit::DocumentMut = toml::to_string(&config)?.parse()?;
        codex_core::config::edit::ConfigEditsBuilder::for_config(&self.config)
            .with_edits([codex_core::config::edit::ConfigEdit::SetPath {
                segments: vec!["model_routing".into()],
                value: toml_edit::Item::Table(document.as_table().clone()),
            }])
            .apply()
            .await?;
        let view = self.model_routing_view().await?;
        Ok(AccountManagerResult { message: "Model routing settings saved. New tasks read this policy; active tasks keep their admitted choice.".into(),
            data: serde_json::to_value(view)? })
    }

    pub(super) async fn preview_model_routing(
        &self,
        task: &str,
        config: ModelRoutingConfigToml,
    ) -> anyhow::Result<AccountManagerResult> {
        // This simulation never calls the selected external service.
        ModelRoutingConfigToml {
            mode: ModelRoutingMode::Off,
            ..config.clone()
        }
        .validate()?;
        anyhow::ensure!(
            !task.trim().is_empty() && task.len() <= 2048,
            "Preview needs a task description of 1 to 2048 bytes"
        );
        let (models, current_model) = self.routing_models(RefreshStrategy::Offline).await?;
        let candidates = models
            .into_iter()
            .filter_map(|model| {
                let role = config
                    .model_roles
                    .get(&model.slug)
                    .map(|role| match role {
                        codex_config::ModelRoutingRole::Economy => RoutingModelRole::Economy,
                        codex_config::ModelRoutingRole::Balanced => RoutingModelRole::Balanced,
                        codex_config::ModelRoutingRole::Capability => RoutingModelRole::Capability,
                    })
                    .or_else(|| default_routing_role(&model))?;
                Some(RoutingCandidate { model, role })
            })
            .collect::<Vec<_>>();
        let selected = choose_task_model(
            &RoutingRequest {
                task,
                current_model: &current_model,
                current_effort: self.config.model_reasoning_effort.clone(),
                preference: config.preference,
                candidates: &candidates,
                allowed_models: &config.allowed_models,
                required_context_tokens: 4096,
                requires_images: false,
                max_effort: ReasoningEffort::from_str(&config.max_effort)
                    .map_err(anyhow::Error::msg)?,
            },
            classify_task_locally(task),
        );
        Ok(AccountManagerResult { message: "Local preview only. No task was sent to a decision service and no inference was started.".into(),
            data: selected.map_or(serde_json::Value::Null, |choice| serde_json::json!({"model":choice.model,"effort":choice.effort,"reason":choice.reason})) })
    }
}

#[cfg(test)]
#[path = "model_routing_tests.rs"]
mod tests;
