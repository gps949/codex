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

#[path = "quota_report.rs"]
mod report;
use report::QuotaRefreshOutcome;
use report::QuotaRefreshReport;
use report::RefreshPermit;

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
        let manager = AuthManager::shared_managed_profile_from_auth_config(auth_config).await;
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
        let controller = codex_http_client::NetworkPolicyController::default();
        controller.publish(
            controller.policy().revision(),
            crate::config_manager::application_network::destination_policy(
                config
                    .config_layer_stack
                    .requirements_toml()
                    .application
                    .as_ref(),
            ),
        );
        let factory = config
            .http_client_factory()
            .with_network_policy(controller.policy());
        Ok((
            Client::from_auth(config.chatgpt_base_url, &auth, factory),
            manager,
            auth,
        ))
    }

    pub(super) async fn refresh_profiles(
        &self,
        ids: Option<Vec<String>>,
    ) -> anyhow::Result<String> {
        let records = self.store().load_profile_records()?;
        let ids = match ids {
            Some(ids) => {
                for id in &ids {
                    anyhow::ensure!(
                        records
                            .iter()
                            .any(|record| record.profile.id.as_str() == id),
                        "Account profile no longer exists"
                    );
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
        let ids: std::collections::HashSet<_> = ids.into_iter().collect();
        let mut pending = Vec::new();
        let mut skipped = 0;
        {
            let mut statuses = self
                .refreshes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for id in ids {
                if statuses.get(&id).is_some_and(|status| status.in_progress) {
                    skipped += 1;
                    continue;
                }
                let count = statuses
                    .get(&id)
                    .and_then(|status| status.reset_credit_count);
                statuses.insert(
                    id.clone(),
                    RefreshStatus {
                        in_progress: true,
                        attempted_at: Utc::now().timestamp(),
                        succeeded: false,
                        message: "Checking fresh quota…".into(),
                        reset_credit_count: count,
                    },
                );
                pending.push(RefreshPermit {
                    statuses: Arc::clone(&self.refreshes),
                    id,
                    completed: false,
                });
            }
        }
        let single_account = pending.len() == 1;
        let mut jobs = futures::stream::iter(pending)
            .map(|mut permit| async move {
                let result =
                    tokio::time::timeout(Duration::from_secs(10), self.refresh_profile(&permit.id))
                        .await;
                let (outcome, message, count) = match result {
                    Ok(Ok((report, count))) => (report.outcome, report.message, count),
                    Ok(Err(error)) => (QuotaRefreshOutcome::Failed, error.to_string(), None),
                    Err(_) => (
                        QuotaRefreshOutcome::Failed,
                        "Quota check timed out; cached values were retained. Refresh to retry."
                            .into(),
                        None,
                    ),
                };
                // This existing wire field describes the request, not window completeness or
                // backend permission. Keep partial observations separate from request errors.
                let succeeded = outcome != QuotaRefreshOutcome::Failed;
                let mut statuses = self
                    .refreshes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(status) = statuses.get_mut(&permit.id) {
                    status.in_progress = false;
                    status.succeeded = succeeded;
                    status.message = message.clone();
                    if succeeded {
                        status.reset_credit_count = count;
                    }
                }
                permit.completed = true;
                drop(statuses);
                drop(permit);
                (outcome, message)
            })
            .buffer_unordered(4);
        let mut updated = 0;
        let mut incomplete = 0;
        let mut denied = 0;
        let mut failed = 0;
        let mut detail = String::new();
        while let Some((outcome, message)) = jobs.next().await {
            match outcome {
                QuotaRefreshOutcome::Updated => updated += 1,
                QuotaRefreshOutcome::Incomplete => incomplete += 1,
                QuotaRefreshOutcome::Denied => denied += 1,
                QuotaRefreshOutcome::Failed => failed += 1,
            }
            detail = message;
        }
        let mut message = if incomplete == 0 && denied == 0 {
            format!(
                "Quota check: {updated} updated, {failed} failed, {skipped} already checking. Each account shows its own result."
            )
        } else {
            format!(
                "Quota check: {updated} updated, {incomplete} incomplete, {denied} backend denied, {failed} failed, {skipped} already checking. Each account shows its own result."
            )
        };
        if single_account && updated == 0 {
            message.push_str(&format!("\n{detail}"));
        }
        Ok(message)
    }

    async fn refresh_profile(&self, id: &str) -> anyhow::Result<(QuotaRefreshReport, Option<u64>)> {
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
        let report = if recovered {
            QuotaRefreshReport {
                outcome: QuotaRefreshOutcome::Updated,
                message: "Backend recovery confirmed".into(),
            }
        } else {
            let saved = store
                .load()?
                .profiles
                .into_iter()
                .find(|saved| saved.profile_id == profile.profile.id)
                .ok_or_else(|| {
                    anyhow::anyhow!("Account state changed during quota check; refresh to retry")
                })?;
            report::quota_refresh_report(&response, &saved, observed_at)
        };
        Ok((report, count))
    }

    pub(super) async fn read_credits(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        let profile = self.profile(id)?;
        let (client, manager, auth) = self.profile_client(id).await?;
        let owner = manager
            .auth_change_state_receiver()
            .borrow()
            .owner_generation;
        let details = tokio::time::timeout(
            Duration::from_secs(10),
            client.list_rate_limit_reset_credits(),
        )
        .await??;
        manager.reload().await;
        let current_profile = self.profile(id)?;
        let current = manager.auth_cached();
        let pool = self.execution_pool().await?;
        let store = AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf());
        anyhow::ensure!(
            !current_profile.profile.disabled
                && current_profile.state == AccountProfileState::Ready
                && current_profile.profile.credential_home == profile.profile.credential_home
                && manager
                    .auth_change_state_receiver()
                    .borrow()
                    .owner_generation
                    == owner
                && current
                    .as_ref()
                    .is_some_and(|current| current.get_account_id() == auth.get_account_id()
                        && current.get_chatgpt_user_id() == auth.get_chatgpt_user_id())
                && pool.as_ref().is_some_and(|pool| store
                    .validate_profile_auth(pool, &profile.profile.id, &auth)
                    .unwrap_or(false)),
            "Account identity changed during the credit check; refresh before viewing credits"
        );
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
        context: &AccountOperationContext,
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
        context.ensure_current().await?;
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

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
