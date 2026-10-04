//! Refreshes only decision settings at call boundaries, retaining every other config layer.

use super::Config;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::DecisionAdvisorConfigToml;
use codex_config::LoaderOverrides;
use codex_model_provider::DecisionAdvisorCredentialStore;
use codex_model_provider::DecisionAdvisorSecret;
use codex_model_provider::DecisionAdvisorSettings;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct DecisionAdvisorLocalSource {
    pub(super) initial_settings: DecisionAdvisorSettings,
    reload_user_config: bool,
    system_requirements_path: Option<PathBuf>,
    ignore_managed_requirements: bool,
    macos_managed_config_requirements_base64: Option<String>,
}

impl DecisionAdvisorLocalSource {
    pub(super) fn from_overrides(overrides: &LoaderOverrides) -> Self {
        Self {
            initial_settings: DecisionAdvisorSettings::default(),
            reload_user_config: !overrides.ignore_user_config,
            system_requirements_path: overrides.system_requirements_path.clone(),
            ignore_managed_requirements: overrides.ignore_managed_requirements,
            macos_managed_config_requirements_base64: overrides
                .macos_managed_config_requirements_base64
                .clone(),
        }
    }
}

/// A local config observation; it is not evidence that any execution process adopted it.
#[derive(Debug, Clone)]
pub struct DecisionAdvisorSnapshot {
    pub configured: DecisionAdvisorConfigToml,
    pub effective: DecisionAdvisorSettings,
    pub user_config_version: String,
    pub overridden: bool,
}

impl Config {
    /// Reads local decision settings without rebuilding config or fetching cloud policy.
    pub async fn decision_advisor_snapshot(&self) -> io::Result<DecisionAdvisorSnapshot> {
        self.decision_snapshot(/*draft*/ None).await
    }

    /// Resolves a typed user-table draft under the already-loaded higher-priority layers.
    pub async fn decision_advisor_draft_snapshot(
        &self,
        draft: &DecisionAdvisorConfigToml,
    ) -> io::Result<DecisionAdvisorSnapshot> {
        self.decision_snapshot(Some(draft)).await
    }

    async fn decision_snapshot(
        &self,
        draft: Option<&DecisionAdvisorConfigToml>,
    ) -> io::Result<DecisionAdvisorSnapshot> {
        let mut layers = self.config_layer_stack.clone();
        let source = self.decision_advisor_source.as_ref();
        let active_file = layers
            .get_user_config_file()
            .cloned()
            .unwrap_or_else(|| self.codex_home.join(codex_config::CONFIG_TOML_FILE));
        let mut active =
            if draft.is_some() || source.is_some_and(|source| source.reload_user_config) {
                read_local_table(active_file.as_path()).await?
            } else {
                layers
                    .get_active_user_layer()
                    .map(|layer| layer.config.clone())
                    .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()))
            };
        let user_config_version = codex_config::version_for_toml(&active);
        if let Some(draft) = draft {
            active
                .as_table_mut()
                .ok_or_else(|| io::Error::other("Invalid user configuration table"))?
                .insert(
                    "decision_advisor".into(),
                    toml::Value::try_from(draft).map_err(io::Error::other)?,
                );
        }
        let configured = decision_table(&active)?;
        if source.is_some_and(|source| source.reload_user_config) {
            for layer in self
                .config_layer_stack
                .layers_low_to_high()
                .filter(|layer| matches!(layer.name, ConfigLayerSource::User { .. }))
            {
                let ConfigLayerSource::User { file, .. } = &layer.name else {
                    continue;
                };
                let current = if file == &active_file {
                    active.clone()
                } else {
                    read_local_table(file.as_path()).await?
                };
                let mut updated = layer.config.clone();
                let table = updated
                    .as_table_mut()
                    .ok_or_else(|| io::Error::other("Invalid user configuration table"))?;
                table.remove("decision_advisor");
                if let Some(value) = current.get("decision_advisor") {
                    table.insert("decision_advisor".into(), value.clone());
                }
                layers = layers.with_user_config(file, updated)?;
            }
        }
        // Programmatic callers may explicitly install settings outside the TOML stack.
        let effective =
            if source.is_none_or(|source| self.decision_advisor != source.initial_settings) {
                self.decision_advisor.clone()
            } else {
                resolve_layers(&layers)?
            };
        let requested = super::decision_advisor::resolve(Some(&configured))?;
        Ok(DecisionAdvisorSnapshot {
            configured,
            overridden: effective != requested,
            effective,
            user_config_version,
        })
    }

    pub fn decision_advisor_credential_store(&self) -> DecisionAdvisorCredentialStore {
        DecisionAdvisorCredentialStore::new(
            self.codex_home.to_path_buf(),
            self.cli_auth_credentials_store_mode,
            self.auth_keyring_backend_kind(),
        )
    }

    /// Captures exactly the independent credential belonging to this settings snapshot.
    pub fn decision_advisor_credential(
        &self,
        settings: &DecisionAdvisorSettings,
    ) -> io::Result<Option<DecisionAdvisorSecret>> {
        self.decision_advisor_credential_store()
            .resolve(settings, |name| std::env::var(name).ok())
    }

    /// Refreshes local application restrictions while retaining loaded enterprise restrictions.
    pub async fn decision_advisor_local_application_requirements(
        &self,
    ) -> io::Result<Option<codex_config::ApplicationRequirementsToml>> {
        let source = self
            .decision_advisor_source
            .as_ref()
            .ok_or_else(|| io::Error::other("Decision advisor policy inputs are unavailable"))?;
        let overrides = LoaderOverrides {
            system_requirements_path: source.system_requirements_path.clone(),
            ignore_managed_requirements: source.ignore_managed_requirements,
            macos_managed_config_requirements_base64: source
                .macos_managed_config_requirements_base64
                .clone(),
            ..Default::default()
        };
        codex_config::loader::load_local_application_requirements(
            codex_exec_server::LOCAL_FS.as_ref(),
            &overrides,
        )
        .await?
        .compose(Default::default())
    }
}

async fn read_local_table(path: &std::path::Path) -> io::Result<toml::Value> {
    let contents = match tokio::fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    if contents.len() > 2 * 1024 * 1024 {
        return Err(io::Error::other(
            "User configuration exceeds the local decision settings limit",
        ));
    }
    toml::from_str(&contents).map_err(io::Error::other)
}

fn decision_table(config: &toml::Value) -> io::Result<DecisionAdvisorConfigToml> {
    config
        .get("decision_advisor")
        .cloned()
        .map(toml::Value::try_into)
        .transpose()
        .map_err(io::Error::other)
        .map(Option::unwrap_or_default)
}

fn resolve_layers(layers: &ConfigLayerStack) -> io::Result<DecisionAdvisorSettings> {
    super::decision_advisor::resolve(Some(&decision_table(&layers.effective_config())?))
}

#[cfg(test)]
#[path = "decision_advisor_current_tests.rs"]
mod tests;
