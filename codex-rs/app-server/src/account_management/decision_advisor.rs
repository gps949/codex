//! Local settings and explicitly consented, synthetic probes for an independent service.

use super::*;
use codex_config::DecisionAdvisorConfigToml;
use codex_config::DecisionAdvisorCredentialSourceToml;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_http_client::DestinationPolicy;
use codex_http_client::HttpClientFactory;
use codex_http_client::NetworkPolicyController;
use codex_model_provider::DecisionAdvice;
use codex_model_provider::DecisionAdvisor;
use codex_model_provider::DecisionAdvisorCredentialSource;
use codex_model_provider::DecisionAdvisorFallback;
use codex_model_provider::DecisionAdvisorMode;
use codex_model_provider::DecisionAdvisorSecret;
use codex_model_provider::DecisionAdvisorSettings;
use codex_model_provider::DecisionCandidate;
use codex_model_provider::DecisionSearchRequest;
use codex_model_provider::DecisionSearchScope;

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum DecisionAdvisorCredentialAction {
    #[default]
    Keep,
    Replace {
        value: DecisionAdvisorSecret,
    },
    Remove,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionAdvisorView {
    pub config: DecisionAdvisorConfigToml,
    pub effective_config: DecisionAdvisorSettings,
    pub credential_present: bool,
    pub credential_source: String,
    pub credential_storage: String,
    pub user_config_version: String,
    pub overridden: bool,
    pub policy_status: String,
    pub runtime_status: String,
    pub application_hint: String,
}

impl AccountManager {
    /// Purely local observation. A successful synthetic probe is never execution evidence.
    pub async fn decision_advisor_view(&self) -> anyhow::Result<DecisionAdvisorView> {
        let snapshot = self.config.decision_advisor_snapshot().await.map_err(|_| {
            anyhow::anyhow!("Decision settings are unavailable; check the local configuration")
        })?;
        let credential_present = self
            .config
            .decision_advisor_credential(&snapshot.effective)
            .map_err(|_| anyhow::anyhow!("Decision credential storage is unavailable"))?
            .is_some();
        let credential_source = match snapshot.effective.credential_source {
            DecisionAdvisorCredentialSource::Stored => "saved",
            DecisionAdvisorCredentialSource::Environment => "environment",
        };
        let policy_status = match self.decision_factory(&snapshot.effective).await {
            Ok(_) => "knownAllowed",
            Err("Decision service destination is blocked by managed network policy.") => "blocked",
            Err(_) => "unavailable",
        };
        let credential_storage = match self.config.cli_auth_credentials_store_mode {
            codex_login::AuthCredentialsStoreMode::File => "file",
            codex_login::AuthCredentialsStoreMode::Keyring => "keyring",
            codex_login::AuthCredentialsStoreMode::Auto => "keyringOrFile",
            codex_login::AuthCredentialsStoreMode::Ephemeral => "ephemeral",
        };
        Ok(DecisionAdvisorView {
            config: snapshot.configured,
            effective_config: snapshot.effective,
            credential_present,
            credential_source: credential_source.into(),
            credential_storage: credential_storage.into(),
            user_config_version: snapshot.user_config_version,
            overridden: snapshot.overridden,
            policy_status: policy_status.into(),
            runtime_status: "unobserved".into(),
            application_hint: "Updated hosts read decision settings on their next tool search or root user turn. Actual execution adoption has not been observed.".into(),
        })
    }

    pub(super) async fn save_decision_advisor(
        &self,
        mut config: DecisionAdvisorConfigToml,
        credential: DecisionAdvisorCredentialAction,
        consent: bool,
        expected_version: Option<String>,
    ) -> anyhow::Result<AccountManagerResult> {
        // This lock is independent of inference-account state and shared across managers/processes.
        let home = self.config.codex_home.to_path_buf();
        let _transaction = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&home)?;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(home.join(".decision-advisor.lock"))?;
            file.lock()?;
            Ok::<_, std::io::Error>(file)
        })
        .await??;
        if matches!(credential, DecisionAdvisorCredentialAction::Replace { .. }) {
            config.credential_source = DecisionAdvisorCredentialSourceToml::Stored;
        }
        let snapshot = self
            .config
            .decision_advisor_draft_snapshot(&config)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Invalid decision settings; check the local configuration and service fields"
                )
            })?;
        anyhow::ensure!(
            expected_version
                .as_ref()
                .is_none_or(|version| version == &snapshot.user_config_version),
            "Decision settings changed while this page was open. Reload before saving."
        );
        anyhow::ensure!(
            (config.mode == codex_config::DecisionAdvisorModeToml::Off
                && snapshot.effective.mode == DecisionAdvisorMode::Off)
                || consent,
            "Confirm that the independent service may receive discovery text and charge separately before enabling it."
        );
        let settings = codex_core::config::resolve_decision_advisor_settings(Some(&config))?;
        if !settings.endpoint.is_empty() {
            let mut validated = settings.clone();
            validated.mode = DecisionAdvisorMode::Shadow;
            validated.validate().map_err(anyhow::Error::msg)?;
        }
        let store = self.config.decision_advisor_credential_store();
        if settings.mode != DecisionAdvisorMode::Off
            && !matches!(credential, DecisionAdvisorCredentialAction::Replace { .. })
        {
            anyhow::ensure!(
                !matches!(credential, DecisionAdvisorCredentialAction::Remove),
                "Disable decision assistance before removing its token."
            );
            let present = self
                .config
                .decision_advisor_credential(&settings)
                .map_err(|_| anyhow::anyhow!("Decision credential storage is unavailable"))?
                .is_some();
            let local_without_auth = settings.api_key_env.is_empty() && settings.allow_local_http;
            anyhow::ensure!(
                present || local_without_auth,
                "Enter a token for this service before enabling decision assistance."
            );
        }
        match &credential {
            DecisionAdvisorCredentialAction::Keep => {}
            DecisionAdvisorCredentialAction::Replace { value } => {
                store.save(&settings, value).map_err(|_| {
                    anyhow::anyhow!(
                        "The independent decision key could not be stored. Settings were not saved."
                    )
                })?;
            }
            DecisionAdvisorCredentialAction::Remove => {
                store.remove(&settings).map_err(|_| anyhow::anyhow!("The independent decision key could not be removed. Settings were not saved."))?;
            }
        }
        let document: toml_edit::DocumentMut = toml::to_string(&config)?.parse()?;
        ConfigEditsBuilder::for_config(&self.config).with_edits([ConfigEdit::SetPath {
            segments: vec!["decision_advisor".into()],
            value: toml_edit::Item::Table(document.as_table().clone()),
        }]).apply().await.map_err(|_| anyhow::anyhow!("Decision settings could not be saved. A requested credential change may already have been stored."))?;
        let view = self.decision_advisor_view().await?;
        Ok(AccountManagerResult {
            message: if view.overridden { "Decision settings saved locally. Higher-priority settings override this manager's effective configuration." } else { "Decision settings saved locally. Updated hosts read them on the next decision call; actual execution adoption has not been observed." }.into(),
            data: serde_json::to_value(view)?,
        })
    }

    pub(super) async fn probe_decision_advisor(
        &self,
        mut config: DecisionAdvisorConfigToml,
        credential: DecisionAdvisorCredentialAction,
        consent: bool,
        expected_version: Option<String>,
    ) -> anyhow::Result<AccountManagerResult> {
        anyhow::ensure!(
            consent,
            "Confirm sending the built-in synthetic example to the independent service before testing."
        );
        if matches!(credential, DecisionAdvisorCredentialAction::Replace { .. }) {
            config.credential_source = DecisionAdvisorCredentialSourceToml::Stored;
        }
        let snapshot = self
            .config
            .decision_advisor_draft_snapshot(&config)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Invalid decision settings; check the local configuration and service fields"
                )
            })?;
        anyhow::ensure!(
            expected_version
                .as_ref()
                .is_none_or(|version| version == &snapshot.user_config_version),
            "Decision settings changed while this page was open. Reload before testing."
        );
        let mut settings = snapshot.effective;
        settings.mode = DecisionAdvisorMode::Shadow;
        settings.validate().map_err(anyhow::Error::msg)?;
        let factory = match self.decision_factory(&settings).await {
            Ok(factory) => factory,
            Err(message) => return Ok(probe_result(/*connected*/ false, message)),
        };
        let secret = match credential {
            DecisionAdvisorCredentialAction::Keep => self
                .config
                .decision_advisor_credential(&settings)
                .map_err(|_| anyhow::anyhow!("Decision credential storage is unavailable"))?,
            DecisionAdvisorCredentialAction::Remove => None,
            DecisionAdvisorCredentialAction::Replace { value } => {
                value.validate()?;
                let requested =
                    codex_core::config::resolve_decision_advisor_settings(Some(&config))?;
                anyhow::ensure!(
                    requested.provider == settings.provider
                        && requested.endpoint == settings.endpoint,
                    "Higher-priority settings select another service target. Test that target with its own credentials."
                );
                Some(value)
            }
        };
        let candidates = [
            DecisionCandidate {
                id: "clock".into(),
                description: "Reports the current time.".into(),
            },
            DecisionCandidate {
                id: "weather".into(),
                description: "Reports a weather forecast.".into(),
            },
        ];
        // A fresh instance ensures every explicit test attempts a request instead of a cache hit.
        let advice = DecisionAdvisor::default()
            .rank(
                &settings,
                &factory,
                DecisionSearchRequest {
                    scope: DecisionSearchScope::Tools,
                    query: "Find the tool that reports the current time.",
                    candidates: &candidates,
                    catalog_revision: b"synthetic-manager-probe-v1",
                },
                secret.as_ref().map(DecisionAdvisorSecret::expose_secret),
            )
            .await;
        Ok(match advice {
            DecisionAdvice::Ranked(_)
            | DecisionAdvice::Fallback(
                DecisionAdvisorFallback::LowConfidence | DecisionAdvisorFallback::NoMatch,
            ) => probe_result(
                /*connected*/ true,
                "Synthetic connection test succeeded. Draft settings were not saved.",
            ),
            DecisionAdvice::Fallback(reason) => probe_result(
                /*connected*/ false,
                match reason {
                    DecisionAdvisorFallback::MissingCredential => {
                        "No independent decision credential is available for this service target."
                    }
                    DecisionAdvisorFallback::NetworkDenied => {
                        "Decision service destination is blocked by managed network policy."
                    }
                    DecisionAdvisorFallback::TimedOut => "Synthetic connection test timed out.",
                    DecisionAdvisorFallback::InvalidResponse => {
                        "The service returned an incompatible synthetic test response."
                    }
                    DecisionAdvisorFallback::Unavailable => {
                        "Synthetic connection test could not reach the service or authenticate."
                    }
                    DecisionAdvisorFallback::InvalidConfiguration => {
                        "Decision service configuration is invalid."
                    }
                    DecisionAdvisorFallback::Busy => {
                        "The independent decision service is busy. Try the synthetic test again."
                    }
                    DecisionAdvisorFallback::Disabled => "The synthetic test was disabled.",
                    DecisionAdvisorFallback::OversizedInput => {
                        "The synthetic test input was rejected."
                    }
                    DecisionAdvisorFallback::LowConfidence | DecisionAdvisorFallback::NoMatch => {
                        unreachable!("handled above")
                    }
                },
            ),
        })
    }

    async fn decision_factory(
        &self,
        settings: &DecisionAdvisorSettings,
    ) -> Result<HttpClientFactory, &'static str> {
        let endpoint = url::Url::parse(&settings.endpoint)
            .map_err(|_| "Decision service endpoint is not configured.")?;
        let loaded = crate::config_manager::application_network::destination_policy(
            self.config
                .config_layer_stack
                .requirements_toml()
                .application
                .as_ref(),
        );
        let local = self
            .config
            .decision_advisor_local_application_requirements()
            .await
            .map_err(|_| "Managed network policy is unavailable. No test request was sent.")?;
        let local = crate::config_manager::application_network::destination_policy(local.as_ref());
        let narrowed = match (loaded, local) {
            (DestinationPolicy::Unrestricted, policy)
            | (policy, DestinationPolicy::Unrestricted) => policy,
            (
                DestinationPolicy::Restricted {
                    allowed_hosts: loaded,
                },
                DestinationPolicy::Restricted {
                    allowed_hosts: local,
                },
            ) => DestinationPolicy::Restricted {
                allowed_hosts: loaded.intersection(&local).cloned().collect(),
            },
        };
        let controller = NetworkPolicyController::default();
        controller.publish(controller.policy().revision(), narrowed);
        controller
            .policy()
            .acquire(&endpoint)
            .map_err(|_| "Decision service destination is blocked by managed network policy.")?;
        let factory = self.config.http_client_factory();
        if factory.network_policy().is_managed() {
            factory
                .network_policy()
                .acquire(&endpoint)
                .map_err(|error| match error {
                    codex_http_client::NetworkPolicyDenied::Unavailable
                    | codex_http_client::NetworkPolicyDenied::Revoked => {
                        "Managed network policy is unavailable. No test request was sent."
                    }
                    codex_http_client::NetworkPolicyDenied::Destination
                    | codex_http_client::NetworkPolicyDenied::UnsupportedTransport => {
                        "Decision service destination is blocked by managed network policy."
                    }
                })?;
            Ok(factory)
        } else {
            Ok(factory.with_network_policy(controller.policy()))
        }
    }
}

fn probe_result(connected: bool, message: &'static str) -> AccountManagerResult {
    AccountManagerResult {
        message: message.into(),
        data: serde_json::json!({ "connected": connected, "scope": "synthetic" }),
    }
}

#[cfg(test)]
#[path = "decision_advisor_tests.rs"]
mod tests;
