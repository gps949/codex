use super::*;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_login::AccountLabelUpdate;
use codex_login::AccountProfileMetadataUpdate;
use codex_login::AccountProfileState;
use codex_login::AccountRuntimeStateStore;
use codex_login::AccountSelectionMode;
use codex_login::AuthManager;

impl AccountManager {
    pub async fn execute(
        self: &Arc<Self>,
        operation: AccountManagerOperation,
    ) -> anyhow::Result<AccountManagerResult> {
        self.execute_with_context(operation, &AccountOperationContext::Independent)
            .await
    }

    pub(crate) async fn execute_with_context(
        self: &Arc<Self>,
        operation: AccountManagerOperation,
        context: &AccountOperationContext,
    ) -> anyhow::Result<AccountManagerResult> {
        context.ensure_current().await?;
        let mut data = serde_json::Value::Null;
        let message = match operation {
            AccountManagerOperation::PrimaryUse { profile_id } => {
                codex_login::PrimaryLoginStore::new(self.config.codex_home.to_path_buf())
                    .select_profile(
                        &self.config.auth_config(),
                        &codex_login::AccountProfileId::new(profile_id)?,
                    )
                    .await?;
                "Host sign-in selected. Running hosts apply this automatically and continue Remote if it was enabled. Devices may need pairing for the new owner.".into()
            }
            AccountManagerOperation::PrimaryRoot => {
                codex_login::PrimaryLoginStore::new(self.config.codex_home.to_path_buf())
                    .select_root(&self.config.auth_config())
                    .await?;
                "Host sign-in now uses root login. Inference selection is unchanged.".into()
            }
            AccountManagerOperation::PrimaryLogout => {
                codex_login::PrimaryLoginStore::new(self.config.codex_home.to_path_buf())
                    .sign_out()?;
                "Host signed out. Pool credentials were retained.".into()
            }
            operation @ (AccountManagerOperation::ApiAdd { .. }
            | AccountManagerOperation::ApiUpdate { .. }
            | AccountManagerOperation::ApiReplaceKey { .. }
            | AccountManagerOperation::ApiUse { .. }
            | AccountManagerOperation::ApiRemove { .. }
            | AccountManagerOperation::ApiFallback { .. }) => return self.api_operation(operation),
            AccountManagerOperation::Refresh { profile_ids } => {
                self.refresh_profiles(profile_ids).await?
            }
            AccountManagerOperation::Use { profile_id } => {
                self.select(&profile_id, AccountSelectionMode::AvailableOnly)?;
                "Account selected for subsequent requests.".into()
            }
            AccountManagerOperation::Retry { profile_id } => {
                self.select(&profile_id, AccountSelectionMode::ForceProbe)?;
                "Local quota cooldown cleared for one probe. No reset credit was used.".into()
            }
            AccountManagerOperation::Automatic => {
                let inventory = self.inventory().await?;
                if let Some(candidate) = inventory
                    .accounts
                    .iter()
                    .find(|account| account.availability == "ready")
                {
                    self.select(&candidate.profile_id, AccountSelectionMode::AvailableOnly)?;
                } else {
                    self.api_store()
                        .select(codex_login::ApiAccountSelection::Subscription)?;
                    codex_login::AccountPoolRuntime::resume_home(&self.config.codex_home)?;
                }
                "Returned to subscription account selection. Exhausted accounts retain their cooldowns.".into()
            }
            AccountManagerOperation::Update {
                profile_id,
                label,
                priority,
                disabled,
            } => {
                let record = self.profile(&profile_id)?;
                let label = label.map(|label| {
                    let label = label
                        .trim()
                        .chars()
                        .filter(|ch| !ch.is_control())
                        .take(80)
                        .collect::<String>();
                    if label.is_empty() {
                        AccountLabelUpdate::Clear
                    } else {
                        AccountLabelUpdate::Set(label)
                    }
                });
                self.store().update_profile_metadata(
                    &record.profile.id,
                    AccountProfileMetadataUpdate {
                        label,
                        priority,
                        disabled,
                    },
                )?;
                "Account details saved. Running clients synchronize the change.".into()
            }
            AccountManagerOperation::Remove {
                profile_id,
                keep_credentials,
            } => {
                let record = self.profile(&profile_id)?;
                // Reject selected host identities before any token revocation.
                self.store().remove_profile_metadata(&record.profile.id)?;
                let mut warnings = Vec::new();
                if let Err(error) =
                    AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf())
                        .remove_profile(&record.profile.id)
                {
                    warnings.push(format!("Scheduler cleanup failed: {error}"));
                }
                let remove_credentials =
                    !keep_credentials && record.profile.id.as_str() != "legacy-root";
                let mut credentials_removed = false;
                if remove_credentials {
                    let mut auth_config = self.config.auth_config();
                    auth_config.codex_home = record.profile.credential_home.clone();
                    let manager =
                        AuthManager::shared_managed_profile_from_auth_config(auth_config).await;
                    // Revocation is best effort. Always report local cleanup separately.
                    match manager.logout_with_revoke().await {
                        Ok(_) => match self.store().purge_managed_credentials(&record.profile.id) {
                            Ok(_) => credentials_removed = true,
                            Err(error) => {
                                warnings.push(format!("Credential folder cleanup failed: {error}"))
                            }
                        },
                        Err(error) => {
                            warnings.push(format!("Local credential removal failed: {error}"))
                        }
                    }
                }
                data = serde_json::json!({
                    "removedFromPool": true,
                    "credentialsRemoved": credentials_removed,
                    "credentialsRetained": !remove_credentials,
                    "cleanupWarnings": warnings,
                });
                if warnings.is_empty() {
                    if remove_credentials {
                        "Account removed from the pool and local credentials deleted. Server token revocation was attempted.".into()
                    } else {
                        "Account removed from the pool. Credentials were retained.".into()
                    }
                } else {
                    format!(
                        "Account removed from the pool; some cleanup remains: {}",
                        warnings.join("; ")
                    )
                }
            }
            AccountManagerOperation::Login { profile_id, label } => {
                data = serde_json::to_value(self.start_login(profile_id, label).await?)?;
                "Open the verification link and complete account login.".into()
            }
            AccountManagerOperation::CancelLogin { operation_id } => {
                let jobs = self.logins.lock().await;
                let job = jobs
                    .get(&operation_id)
                    .ok_or_else(|| anyhow::anyhow!("Login operation no longer exists"))?;
                job.cancel.cancel();
                "Login cancellation requested.".into()
            }
            AccountManagerOperation::Credits { profile_id } => {
                data = self.read_credits(&profile_id).await?;
                "Reset credits loaded for the selected account.".into()
            }
            AccountManagerOperation::Redeem {
                profile_id,
                credit_id,
                idempotency_key,
            } => {
                self.redeem_credit(&profile_id, &credit_id, &idempotency_key, context)
                    .await?
            }
            AccountManagerOperation::Settings { values } => {
                self.update_settings(values).await?;
                "Pool settings saved. Active sessions apply them after configuration refresh."
                    .into()
            }
        };
        Ok(AccountManagerResult { message, data })
    }

    fn select(&self, id: &str, mode: AccountSelectionMode) -> anyhow::Result<()> {
        let record = self.profile(id)?;
        anyhow::ensure!(
            !record.profile.disabled,
            "Enable this account before selecting it"
        );
        anyhow::ensure!(
            record.state == AccountProfileState::Ready,
            "Complete account login first"
        );
        AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf())
            .select(record.profile.id, mode)?;
        self.api_store()
            .select(codex_login::ApiAccountSelection::Subscription)?;
        codex_login::AccountPoolRuntime::resume_home(&self.config.codex_home)?;
        Ok(())
    }

    async fn update_settings(&self, values: serde_json::Value) -> anyhow::Result<()> {
        let values = values
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Settings must be an object"))?;
        let allowed = [
            "rotation_strategy",
            "preemptive_switch_percent",
            "return_to_preferred",
            "window_warmup",
            "window_warmup_interval_minutes",
            "resume_after_reset",
            "max_reset_wait_minutes",
            "auto_reset_credits",
            "auto_reset_credit_min_wait_minutes",
        ];
        anyhow::ensure!(
            values.keys().all(|key| allowed.contains(&key.as_str())),
            "Unsupported account pool setting"
        );
        let _: codex_config::AccountPoolConfigToml =
            serde_json::from_value(serde_json::Value::Object(values.clone()))?;
        let document: toml_edit::DocumentMut = toml::to_string(values)?.parse()?;
        let edits = document.iter().map(|(key, value)| ConfigEdit::SetPath {
            segments: vec!["account_pool".into(), key.into()],
            value: value.clone(),
        });
        ConfigEditsBuilder::for_config(&self.config)
            .with_edits(edits)
            .apply()
            .await
    }
}
