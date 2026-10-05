use super::*;
use codex_login::ApiAccountStore;

impl AccountManager {
    pub(super) fn api_store(&self) -> ApiAccountStore {
        ApiAccountStore::new(
            self.config.codex_home.to_path_buf(),
            self.config.cli_auth_credentials_store_mode,
            self.config.auth_keyring_backend_kind(),
        )
    }

    pub(super) fn api_inventory(
        &self,
    ) -> anyhow::Result<(Vec<ApiAccountView>, codex_login::ApiAccountState)> {
        let codex_login::ApiAccountInventory {
            state,
            credential_revisions,
        } = self.api_store().management_snapshot()?;
        let accounts = state
            .accounts
            .iter()
            .map(|account| {
                let credential_revision = credential_revisions.get(&account.id).cloned();
                ApiAccountView {
                    account: account.clone(),
                    has_key: credential_revision.is_some(),
                    credential_revision,
                }
            })
            .collect();
        Ok((accounts, state))
    }

    pub(super) fn api_operation(
        &self,
        operation: AccountManagerOperation,
    ) -> anyhow::Result<AccountManagerResult> {
        let store = self.api_store();
        let mut data = serde_json::Value::Null;
        let message = match operation {
            AccountManagerOperation::ApiAdd {
                label,
                base_url,
                model,
                api_key,
                context_window,
                images,
            } => {
                let account = store.add(
                    codex_login::ApiAccount {
                        id: String::new(),
                        label,
                        base_url,
                        model,
                        disabled: false,
                        context_window: context_window.unwrap_or(32768),
                        images: images.unwrap_or(false),
                    },
                    &api_key,
                )?;
                data = serde_json::to_value(account)?;
                "API account saved for manual selection. No generating request was sent."
            }
            AccountManagerOperation::ApiUpdate { account } => {
                store.update(account)?;
                "API account details updated."
            }
            AccountManagerOperation::ApiReplaceKey {
                profile_id,
                api_key,
            } => {
                store.replace_key(&profile_id, &api_key)?;
                "API key replaced for subsequent turns. No generating request was sent."
            }
            AccountManagerOperation::ApiUse {
                profile_id,
                expected_credential_revision,
            } => {
                anyhow::ensure!(
                    self.config.forced_chatgpt_workspace_id.is_none()
                        && self.config.auth_config().is_login_method_allowed(
                            codex_protocol::config_types::ForcedLoginMethod::Api
                        ),
                    "The managed workspace policy does not allow selecting a third-party API target"
                );
                store.select_checked(
                    codex_login::ApiAccountSelection::Manual { profile_id },
                    expected_credential_revision.as_deref(),
                )?;
                "Manual API target selected for subsequent turns. Usage is billed by this provider."
            }
            AccountManagerOperation::ApiRemove { profile_id } => {
                store.remove(&profile_id)?;
                "API account and its local key removed."
            }
            AccountManagerOperation::ApiFallback {
                config,
                expected_credential_revision,
            } => {
                anyhow::ensure!(
                    self.config.forced_chatgpt_workspace_id.is_none()
                        && self.config.auth_config().is_login_method_allowed(
                            codex_protocol::config_types::ForcedLoginMethod::Api
                        ),
                    "The managed workspace policy does not allow a third-party fallback"
                );
                store
                    .configure_fallback_checked(config, expected_credential_revision.as_deref())?;
                "API fallback policy saved. Automatic subscription selection remains the default."
            }
            _ => anyhow::bail!("Unsupported API account operation"),
        };
        Ok(AccountManagerResult {
            message: message.into(),
            data,
        })
    }
}

#[cfg(test)]
#[path = "api_accounts_tests.rs"]
mod tests;
