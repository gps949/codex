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

const MAX_REFRESH_RECEIPTS: usize = 512;

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
        let enrolled_ids = records
            .iter()
            .map(|record| record.profile.id.to_string())
            .collect::<std::collections::HashSet<_>>();
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
        let single_account = ids.len() == 1;
        let mut failed = 0;
        let mut detail = String::new();
        let keys = ids
            .into_iter()
            .filter_map(|id| match self.profile_identity(&id) {
                Ok(owner) => Some((id, owner)),
                Err(_) => {
                    failed += 1;
                    detail =
                        "Account identity could not be verified; complete login and refresh again."
                            .into();
                    None
                }
            })
            .collect::<Vec<_>>();
        let mut pending = Vec::new();
        let mut skipped = 0;
        {
            let mut statuses = self
                .refreshes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            statuses.retain(|(profile, _), status| {
                status.in_progress || enrolled_ids.contains(profile)
            });
            for key in keys {
                // An old owner's in-flight permit keeps its own slot until completion or drop.
                statuses.retain(|(profile, owner), status| {
                    status.in_progress || profile != &key.0 || owner == &key.1
                });
                if statuses.get(&key).is_some_and(|status| status.in_progress) {
                    skipped += 1;
                    continue;
                }
                let count = statuses
                    .get(&key)
                    .and_then(|status| status.reset_credit_count);
                if !statuses.contains_key(&key) && statuses.len() >= MAX_REFRESH_RECEIPTS {
                    if let Some(oldest) = statuses
                        .iter()
                        .filter(|(_, status)| !status.in_progress)
                        .min_by_key(|(_, status)| status.attempted_at)
                        .map(|(key, _)| key.clone())
                    {
                        statuses.remove(&oldest);
                    } else {
                        failed += 1;
                        detail = "Too many quota checks are in progress; wait for an existing check before refreshing again.".into();
                        continue;
                    }
                }
                statuses.insert(
                    key.clone(),
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
                    key,
                    completed: false,
                });
            }
        }
        let mut jobs = futures::stream::iter(pending)
            .map(|mut permit| async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(10),
                    self.refresh_profile(&permit.key.0, &permit.key.1),
                )
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
                if let Some(status) = statuses.get_mut(&permit.key) {
                    status.in_progress = false;
                    status.succeeded = succeeded;
                    status.message = message.clone();
                    if succeeded && (count.is_some() || outcome != QuotaRefreshOutcome::Incomplete)
                    {
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

    async fn refresh_profile(
        &self,
        id: &str,
        expected_identity: &str,
    ) -> anyhow::Result<(QuotaRefreshReport, Option<u64>)> {
        let profile = self.profile(id)?;
        anyhow::ensure!(
            self.profile_identity(id)? == expected_identity,
            "Account identity changed before quota refresh; retry for the current account"
        );
        let (client, manager, auth) = self.profile_client(id).await?;
        anyhow::ensure!(
            self.profile_identity(id)? == expected_identity,
            "Account identity changed before quota refresh; retry for the current account"
        );
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
            self.profile_identity(id)? == expected_identity
                && current.get_account_id() == auth.get_account_id()
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
        if recovered
            && let Some(pool) = pool.as_ref()
            && let Some(_spending) = store.try_lock_reset_credit()?
        {
            let state = store.load()?;
            if let Err(error) =
                codex_login::reconcile_reset_credit_recovery(pool, &store, &state).await
            {
                tracing::warn!(%error, "quota recovered; interrupted reset journal still needs review");
            }
        }
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
