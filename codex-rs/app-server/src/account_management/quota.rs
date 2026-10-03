use super::*;
use chrono::DateTime;
use chrono::Utc;
use codex_backend_client::Client;
use codex_backend_client::ConsumeRateLimitResetCreditCode;
use codex_login::AccountProfileState;
use codex_login::AccountRateLimitWindow;
use codex_login::AccountRateLimits;
use codex_login::AccountRuntimeStateStore;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use futures::StreamExt;
use std::time::Duration;

impl AccountManager {
    pub(super) async fn profile_client(
        &self,
        id: &str,
    ) -> anyhow::Result<(Client, Arc<AuthManager>, CodexAuth)> {
        let record = self.profile(id)?;
        anyhow::ensure!(
            !record.profile.disabled,
            "Account is disabled; enable it before contacting the backend"
        );
        anyhow::ensure!(
            record.state == AccountProfileState::Ready,
            "Account login is incomplete"
        );
        let mut auth_config = self.config.auth_config();
        auth_config.codex_home = record.profile.credential_home.clone();
        let manager = AuthManager::shared_from_auth_config(
            auth_config,
            /*enable_codex_api_key_env*/ false,
        )
        .await?;
        let (auth, factory) = manager
            .auth_with_http_client_factory()
            .await
            .ok_or_else(|| anyhow::anyhow!("Account needs login"))?;
        anyhow::ensure!(
            auth.is_chatgpt_auth(),
            "Reset credits are available only for ChatGPT subscription accounts"
        );
        let loader = codex_cloud_config::cloud_config_bundle_loader(
            Arc::clone(&manager),
            self.config.chatgpt_base_url.clone(),
            self.config.codex_home.to_path_buf(),
            factory,
        );
        let config = codex_core::config::ConfigBuilder::default()
            .codex_home(self.config.codex_home.to_path_buf())
            .cli_overrides(vec![(
                "chatgpt_base_url".into(),
                toml::Value::String(self.config.chatgpt_base_url.clone()),
            )])
            .cloud_config_bundle(loader)
            .build()
            .await?;
        let factory = config
            .http_client_factory()
            .with_network_policy(config.application_network_policy.for_current_account());
        Ok((
            Client::from_auth(config.chatgpt_base_url, &auth, factory),
            manager,
            auth,
        ))
    }

    pub(super) async fn refresh_profiles(&self, ids: Option<Vec<String>>) -> anyhow::Result<()> {
        let records = self.store().load_profile_records()?;
        let ids = match ids {
            Some(ids) => {
                for id in &ids {
                    self.profile(id)?;
                }
                ids
            }
            None => records
                .into_iter()
                .filter(|record| {
                    !record.profile.disabled && record.state == AccountProfileState::Ready
                })
                .map(|record| record.profile.id.to_string())
                .collect(),
        };
        let mut jobs = futures::stream::iter(ids)
            .map(|id| async move {
                let attempted_at = Utc::now().timestamp();
                let result =
                    tokio::time::timeout(Duration::from_secs(10), self.refresh_profile(&id)).await;
                let (succeeded, message, count) = match result {
                    Ok(Ok((recovered, count))) => (
                        true,
                        if recovered {
                            "Backend recovery confirmed"
                        } else {
                            "Quota updated"
                        }
                        .into(),
                        count,
                    ),
                    Ok(Err(error)) => (false, error.to_string(), None),
                    Err(_) => (
                        false,
                        "Quota check timed out; cached values were retained".into(),
                        None,
                    ),
                };
                self.refreshes.lock().await.insert(
                    id,
                    RefreshStatus {
                        attempted_at,
                        succeeded,
                        message,
                        reset_credit_count: count,
                    },
                );
            })
            .buffer_unordered(4);
        while jobs.next().await.is_some() {}
        Ok(())
    }

    async fn refresh_profile(&self, id: &str) -> anyhow::Result<(bool, Option<u64>)> {
        let profile = self.profile(id)?;
        let (client, manager, auth) = self.profile_client(id).await?;
        let store = AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf());
        let pool = self.execution_pool().await?;
        let probe = pool
            .as_ref()
            .map(|pool| store.capture_quota_probe(pool, &profile.profile.id, &auth))
            .transpose()?
            .flatten();
        let observed_at = Utc::now();
        let response = client.get_rate_limits_with_reset_credits().await?;
        manager.reload().await;
        let current = manager
            .auth_cached()
            .ok_or_else(|| anyhow::anyhow!("Account login changed during quota check"))?;
        anyhow::ensure!(
            current.get_account_id() == auth.get_account_id()
                && current.get_chatgpt_user_id() == auth.get_chatgpt_user_id(),
            "Account identity changed during quota check; retry"
        );
        anyhow::ensure!(
            response
                .account_id
                .as_ref()
                .is_none_or(|id| Some(id) == auth.get_account_id().as_ref())
                && response
                    .user_id
                    .as_ref()
                    .is_none_or(|id| Some(id) == auth.get_chatgpt_user_id().as_ref()),
            "Backend quota belongs to a different account"
        );
        let count = response
            .rate_limit_reset_credits
            .as_ref()
            .and_then(|credits| credits.available_count.try_into().ok());
        let recovered = if let (Some(pool), Some(probe)) = (pool.as_ref(), probe) {
            store.reconcile_quota_probe(
                pool,
                probe,
                codex_login::AccountQuotaEvidence {
                    rate_limits: &response.rate_limits,
                    ordinary_usage_allowed: response.ordinary_usage_allowed,
                    account_id: response.account_id.as_deref(),
                    user_id: response.user_id.as_deref(),
                },
            )?
        } else {
            false
        };
        if !recovered
            && let Some(snapshot) = response
                .rate_limits
                .iter()
                .find(|snapshot| snapshot.limit_id.as_deref().is_none_or(|id| id == "codex"))
        {
            let window =
                |window: &codex_protocol::protocol::RateLimitWindow| AccountRateLimitWindow {
                    used_percent: window.used_percent,
                    resets_at: window
                        .resets_at
                        .and_then(|at| DateTime::from_timestamp(at, /*nsecs*/ 0)),
                    window_minutes: window.window_minutes,
                };
            store.record_rate_limits(
                &profile.profile.id,
                AccountRateLimits {
                    primary: snapshot.primary.as_ref().map(window),
                    secondary: snapshot.secondary.as_ref().map(window),
                    observed_at: Some(observed_at),
                    window_observed_at: None,
                },
            )?;
        }
        Ok((recovered, count))
    }

    pub(super) async fn read_credits(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        let (client, _, _) = self.profile_client(id).await?;
        let details = tokio::time::timeout(
            Duration::from_secs(10),
            client.list_rate_limit_reset_credits(),
        )
        .await??;
        let credits: Vec<_> = details.credits.into_iter().map(|credit| serde_json::json!({
            "id":credit.id,"resetType":credit.reset_type,"status":credit.status,"grantedAt":credit.granted_at,
            "expiresAt":credit.expires_at,"title":credit.title,"description":credit.description,
        })).collect();
        Ok(
            serde_json::json!({"profileId":id,"availableCount":details.available_count,"credits":credits}),
        )
    }

    async fn execution_pool(&self) -> anyhow::Result<Option<Arc<codex_login::AccountPool>>> {
        Ok(Some(
            codex_login::load_account_pool_for_management(&self.config.auth_config()).await?,
        ))
    }

    pub(super) async fn redeem_credit(
        &self,
        id: &str,
        credit_id: &str,
        key: &str,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            !credit_id.is_empty() && credit_id.len() <= 256 && !key.is_empty() && key.len() <= 128,
            "Invalid credit or operation ID"
        );
        let profile = self.profile(id)?;
        let (client, manager, auth) = self.profile_client(id).await?;
        let owner = manager
            .auth_change_state_receiver()
            .borrow()
            .owner_generation;
        let store = AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let _lock = loop {
            if let Some(lock) = store.try_lock_reset_credit()? {
                break lock;
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "Another reset operation is running; retry with the same operation ID"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let current_profile = self.profile(id)?;
        anyhow::ensure!(
            !current_profile.profile.disabled
                && current_profile.state == AccountProfileState::Ready
                && current_profile.profile.credential_home == profile.profile.credential_home,
            "Account selection changed while waiting; refresh before redeeming a credit"
        );
        manager.reload().await;
        anyhow::ensure!(
            manager
                .auth_change_state_receiver()
                .borrow()
                .owner_generation
                == owner
                && manager.auth_cached().is_some_and(
                    |current| current.get_token_data().ok() == auth.get_token_data().ok()
                ),
            "Account credentials changed while waiting; refresh before redeeming a credit"
        );
        let credits =
            tokio::time::timeout_at(deadline, client.list_rate_limit_reset_credits()).await??;
        let credit = credits
            .credits
            .iter()
            .find(|credit| credit.id == credit_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "This credit is no longer listed. Refresh quota and inspect available credits."
                )
            })?;
        anyhow::ensure!(
            credit.reset_type == "codex_rate_limits"
                && credit.status == "available"
                && credit
                    .expires_at
                    .as_ref()
                    .is_none_or(|expires| DateTime::parse_from_rfc3339(expires)
                        .is_ok_and(|expires| expires > Utc::now())),
            "This credit is unavailable, expired, or has a different quota scope"
        );
        let current_profile = self.profile(id)?;
        anyhow::ensure!(
            !current_profile.profile.disabled
                && current_profile.state == AccountProfileState::Ready,
            "Account changed during the credit check; no credit was redeemed"
        );
        let pool = self.execution_pool().await?;
        anyhow::ensure!(
            pool.as_ref().is_some_and(|pool| store
                .validate_profile_auth(pool, &profile.profile.id, &auth)
                .unwrap_or(false)),
            "Account credentials changed; refresh before redeeming this credit"
        );
        let probe = pool
            .as_ref()
            .map(|pool| store.capture_quota_probe(pool, &profile.profile.id, &auth))
            .transpose()?
            .flatten();
        let result = tokio::time::timeout_at(
            deadline,
            client.consume_rate_limit_reset_credit_by_id(key, credit_id),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Reset outcome is unconfirmed. Refresh quota before retrying the same operation ID."
            )
        })??;
        manager.reload().await;
        let current = manager.auth_cached();
        anyhow::ensure!(
            manager
                .auth_change_state_receiver()
                .borrow()
                .owner_generation
                == owner
                && current
                    .as_ref()
                    .is_some_and(|current| current.get_account_id() == auth.get_account_id()
                        && current.get_chatgpt_user_id() == auth.get_chatgpt_user_id()),
            "Account identity changed during reset; its new identity was left untouched"
        );
        let message = match result.code {
            ConsumeRateLimitResetCreditCode::Reset if result.windows_reset >= 2 => {
                let confirmed = if let (Some(pool), Some(probe)) = (pool.as_ref(), probe) {
                    store.confirm_quota_reset(pool, probe, Utc::now())?
                } else {
                    false
                };
                if confirmed {
                    "Reset credit redeemed and quota recovery confirmed for this account."
                } else {
                    "Reset credit redeemed. Checking fresh quota before confirming local recovery."
                }
            }
            ConsumeRateLimitResetCreditCode::Reset => {
                "Backend reported a partial or unconfirmed reset. Checking fresh quota."
            }
            ConsumeRateLimitResetCreditCode::NothingToReset
            | ConsumeRateLimitResetCreditCode::AlreadyRedeemed => {
                "Backend reported no new redemption. Checking quota before confirming recovery."
            }
            _ => "No reset was applied. Refresh quota or inspect available credits.",
        };
        drop(_lock);
        self.refresh_profiles(Some(vec![id.into()])).await?;
        Ok(message.into())
    }
}
