//! Uses the execution scheduler's eligibility and strategy for standalone account selection.

use super::*;
use codex_login::AccountRuntimeStateStore;
use codex_login::AccountSelectionMode;

impl AccountManager {
    pub(super) async fn current_pool_settings(
        &self,
    ) -> anyhow::Result<codex_config::AccountPoolConfigToml> {
        let mut layers = self.config.config_layer_stack.clone();
        let original = layers
            .effective_config()
            .get("account_pool")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()?
            .unwrap_or_default();
        if self.config.account_pool != original {
            return Ok(self.config.account_pool.clone());
        }
        for layer in self.config.config_layer_stack.layers_low_to_high() {
            let codex_config::ConfigLayerSource::User { file, .. } = &layer.name else {
                continue;
            };
            let bytes = match tokio::fs::read_to_string(file).await {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(error.into()),
            };
            anyhow::ensure!(
                bytes.len() <= 2 * 1024 * 1024,
                "Account settings file exceeds the local safety limit"
            );
            let current: toml::Value = toml::from_str(&bytes)?;
            let mut updated = layer.config.clone();
            let table = updated
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("Invalid account settings table"))?;
            table.remove("account_pool");
            if let Some(settings) = current.get("account_pool") {
                table.insert("account_pool".into(), settings.clone());
            }
            layers = layers.with_user_config(file, updated)?;
        }
        Ok(layers
            .effective_config()
            .get("account_pool")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()?
            .unwrap_or_default())
    }

    pub(super) async fn select(
        &self,
        id: &str,
        mode: AccountSelectionMode,
        context: &AccountOperationContext,
    ) -> anyhow::Result<()> {
        let record = self.profile(id)?;
        anyhow::ensure!(
            !record.profile.disabled,
            "Enable this account before selecting it"
        );
        anyhow::ensure!(
            record.state == codex_login::AccountProfileState::Ready,
            "Complete account login first"
        );
        let pool =
            codex_login::load_account_pool_for_management(&self.config.auth_config()).await?;
        let lease = match mode {
            AccountSelectionMode::AvailableOnly => pool.activate(&record.profile.id),
            AccountSelectionMode::ForceProbe => pool.force_activate(&record.profile.id),
        }
        .map_err(|_| {
            anyhow::anyhow!(
                "Account is unavailable. Refresh quota or sign in again before selecting it."
            )
        })?;
        let manager = lease.auth_manager();
        let auth = manager
            .auth_cached()
            .ok_or_else(|| anyhow::anyhow!("Account needs login"))?;
        let store = AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf());
        anyhow::ensure!(
            manager.refresh_failure_for_auth(&auth).is_none()
                && store.validate_profile_auth(&pool, &record.profile.id, &auth)?,
            "Account login changed. Sign in again before selecting it."
        );
        context.ensure_current().await?;
        store.select(record.profile.id, mode)?;
        self.api_store()
            .select(codex_login::ApiAccountSelection::Subscription)?;
        codex_login::AccountPoolRuntime::resume_home(&self.config.codex_home)?;
        Ok(())
    }

    pub(super) async fn select_automatic(
        &self,
        context: &AccountOperationContext,
    ) -> anyhow::Result<String> {
        let settings = self.current_pool_settings().await?;
        let pool =
            codex_login::load_account_pool_for_management(&self.config.auth_config()).await?;
        pool.set_rotation_strategy(settings.effective_rotation_strategy());
        pool.set_return_to_preferred(settings.effective_return_to_preferred());
        context.ensure_current().await?;
        let state = AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf());
        let selected = pool
            .activate_fill_first()
            .ok()
            .map(|lease| lease.profile().id.clone());
        context.ensure_current().await?;
        if let Some(selected) = selected {
            state.select(selected, AccountSelectionMode::AvailableOnly)?;
            self.api_store()
                .select(codex_login::ApiAccountSelection::Subscription)?;
            codex_login::AccountPoolRuntime::resume_home(&self.config.codex_home)?;
            Ok(
                "Automatic subscription selection applied using the current rotation strategy."
                    .into(),
            )
        } else {
            self.api_store()
                .select(codex_login::ApiAccountSelection::Subscription)?;
            codex_login::AccountPoolRuntime::resume_home(&self.config.codex_home)?;
            Ok("Returned to subscriptions. No eligible account is currently available; quota cooldowns were retained.".into())
        }
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
