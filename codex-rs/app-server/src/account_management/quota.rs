use super::*;
use chrono::DateTime;
use chrono::Utc;
use codex_backend_client::Client;
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
#[path = "quota_reset_credits.rs"]
mod reset_credits;
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

    async fn execution_pool(&self) -> anyhow::Result<Option<Arc<codex_login::AccountPool>>> {
        Ok(Some(
            codex_login::load_account_pool_for_management(&self.config.auth_config()).await?,
        ))
    }
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
