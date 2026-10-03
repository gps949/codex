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
        let store = self.api_store();
        let state = store.load()?;
        let accounts = state
            .accounts
            .iter()
            .map(|account| {
                Ok(ApiAccountView {
                    account: account.clone(),
                    has_key: store.has_key(&account.id)?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
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
            AccountManagerOperation::ApiUse { profile_id } => {
                anyhow::ensure!(
                    self.config.forced_chatgpt_workspace_id.is_none()
                        && self.config.auth_config().is_login_method_allowed(
                            codex_protocol::config_types::ForcedLoginMethod::Api
                        ),
                    "The managed workspace policy does not allow selecting a third-party API target"
                );
                store.select(codex_login::ApiAccountSelection::Manual { profile_id })?;
                "Manual API target selected for subsequent turns. Usage is billed by this provider."
            }
            AccountManagerOperation::ApiRemove { profile_id } => {
                store.remove(&profile_id)?;
                "API account and its local key removed."
            }
            AccountManagerOperation::ApiFallback { config } => {
                anyhow::ensure!(
                    self.config.forced_chatgpt_workspace_id.is_none()
                        && self.config.auth_config().is_login_method_allowed(
                            codex_protocol::config_types::ForcedLoginMethod::Api
                        ),
                    "The managed workspace policy does not allow a third-party fallback"
                );
                store.configure_fallback(config)?;
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
