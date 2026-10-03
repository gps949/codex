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
        let mut data = serde_json::Value::Null;
        let message = match operation {
            operation @ (AccountManagerOperation::ApiAdd { .. }
            | AccountManagerOperation::ApiUpdate { .. }
            | AccountManagerOperation::ApiReplaceKey { .. }
            | AccountManagerOperation::ApiUse { .. }
            | AccountManagerOperation::ApiRemove { .. }
            | AccountManagerOperation::ApiFallback { .. }) => return self.api_operation(operation),
            AccountManagerOperation::Refresh { profile_ids } => {
                self.refresh_profiles(profile_ids).await?;
                "Quota check completed. Each account shows its own result.".into()
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
                if !keep_credentials && record.profile.id.as_str() != "legacy-root" {
                    let mut auth_config = self.config.auth_config();
                    auth_config.codex_home = record.profile.credential_home.clone();
                    let manager = AuthManager::shared_from_auth_config(
                        auth_config,
                        /*enable_codex_api_key_env*/ false,
                    )
                    .await?;
                    // Revocation is best effort; successful local removal does not prove server revocation.
                    manager.logout_with_revoke().await?;
                }
                self.store().remove_profile_metadata(&record.profile.id)?;
                AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf())
                    .remove_profile(&record.profile.id)?;
                if !keep_credentials && record.profile.id.as_str() != "legacy-root" {
                    self.store().purge_managed_credentials(&record.profile.id)?;
                }
                "Account removed from this pool. Server revocation was attempted when local credentials were removed.".into()
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
                self.redeem_credit(&profile_id, &credit_id, &idempotency_key)
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
