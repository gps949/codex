//! Explicit account-panel quota refreshes; never rotate execution identity to inspect a profile.

use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AccountPoolAvailability;
use codex_app_server_protocol::AccountPoolReadResponse;
use codex_core::config::Config;
use codex_login::AccountProfileStore;
use codex_login::AccountRateLimitWindow;
use codex_login::AccountRateLimits;
use codex_login::AccountRuntimeStateStore;
use futures::FutureExt;
use futures::StreamExt;

use super::BackendClient;
use super::account_pool_rate_limits;

pub(super) enum RefreshScope {
    All,
    Profiles(Vec<String>),
}

pub(super) async fn refresh(
    config: &Config,
    response: &mut AccountPoolReadResponse,
    scope: RefreshScope,
    execution_pool: &codex_core::ExecutionAccountPoolHandle,
) {
    if !response.enabled || codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
        return;
    }
    let Ok(records) =
        AccountProfileStore::new(config.codex_home.to_path_buf()).load_profile_records()
    else {
        return;
    };
    let store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    let pool = execution_pool.account_pool();
    let managers: std::collections::HashMap<_, _> =
        execution_pool.auth_managers().into_iter().collect();
    let jobs: Vec<_> = records
        .into_iter()
        .filter(|record| {
            let selected = match &scope {
                RefreshScope::All => true,
                RefreshScope::Profiles(ids) => {
                    ids.iter().any(|id| record.profile.id.as_str() == id)
                }
            };
            selected
                && response.accounts.iter().any(|account| {
                    account.profile_id == record.profile.id.as_str()
                        && !matches!(
                            account.availability,
                            AccountPoolAvailability::Disabled
                                | AccountPoolAvailability::AuthenticationUnavailable { .. }
                        )
                })
        })
        .map(|record| {
            let manager = managers.get(&record.profile.id).cloned();
            let base_url = config.chatgpt_base_url.clone();
            let pool = pool.clone();
            async move {
                let observed_at = Utc::now();
                let request = async {
                    if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
                        return None;
                    }
                    let manager = manager?;
                    let (auth, mut factory) = manager.auth_with_http_client_factory().await?;
                    if !auth.is_chatgpt_auth() {
                        return None;
                    }
                    if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
                        return None;
                    }
                    let mut base_url = base_url;
                    if let Some(clients) = manager.maintenance_clients(&auth).await.ok()? {
                        factory = clients.http_client_factory;
                        base_url = clients.chatgpt_base_url;
                    }
                    let client = BackendClient::from_auth(base_url, &auth, factory);
                    let store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
                    let probe = pool.as_ref().and_then(|pool| {
                        store
                            .capture_quota_probe(pool, &record.profile.id, &auth)
                            .ok()
                            .flatten()
                    });
                    let observed = client.get_rate_limits_with_reset_credits().await.ok()?;
                    if let (Some(pool), Some(probe)) = (pool.as_ref(), probe) {
                        let _ = store.reconcile_quota_probe(
                            pool,
                            probe,
                            codex_login::AccountQuotaEvidence {
                                rate_limits: &observed.rate_limits,
                                ordinary_usage_allowed: observed.ordinary_usage_allowed,
                                account_id: observed.account_id.as_deref(),
                                user_id: observed.user_id.as_deref(),
                            },
                        );
                    }
                    let snapshot = observed.rate_limits.iter().find(|snapshot| {
                        snapshot.limit_id.as_deref().is_none_or(|id| id == "codex")
                    })?;
                    let window = |window: &codex_protocol::protocol::RateLimitWindow| {
                        AccountRateLimitWindow {
                            used_percent: window.used_percent,
                            resets_at: window
                                .resets_at
                                .and_then(|time| DateTime::from_timestamp(time, 0)),
                            window_minutes: window.window_minutes,
                        }
                    };
                    Some(AccountRateLimits {
                        primary: snapshot.primary.as_ref().map(window),
                        secondary: snapshot.secondary.as_ref().map(window),
                        observed_at: Some(observed_at),

                        window_observed_at: None,
                    })
                };
                let limits = tokio::time::timeout(Duration::from_secs(3), request)
                    .await
                    .ok()
                    .flatten()?;
                Some((record.profile.id, limits))
            }
            .boxed()
        })
        .collect();
    let mut pending = futures::stream::iter(jobs).buffer_unordered(4);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let mut observations = Vec::new();
    while let Ok(Some(result)) = tokio::time::timeout_at(deadline, pending.next()).await {
        if let Some((id, limits)) = result {
            if store.record_rate_limits(&id, limits.clone()).is_err() {
                tracing::warn!(profile_id = %id, "failed to save account quota observation");
            }
            observations.push((id, limits));
        }
    }
    drop(pending);
    for (id, limits) in observations {
        if let Some(account) = response
            .accounts
            .iter_mut()
            .find(|account| account.profile_id == id.as_str())
        {
            account.rate_limits = account_pool_rate_limits(limits);
        }
    }
    // Show the persisted merge, including observations from overlapping panel probes,
    // instead of reintroducing a stale or partial network snapshot into the UI.
    if let Ok(saved) = store.load() {
        for account in &mut response.accounts {
            if let Some(profile) = saved
                .profiles
                .iter()
                .find(|profile| profile.profile_id.as_str() == account.profile_id)
            {
                if let Some(pool) = pool.as_ref() {
                    let _ =
                        pool.update_rate_limits(&profile.profile_id, profile.rate_limits.clone());
                }
                let cached = account_pool_rate_limits(profile.rate_limits.clone());
                if cached.observed_at >= account.rate_limits.observed_at {
                    account.rate_limits = cached;
                }
            }
        }
    }
    if let Some(pool) = pool {
        for account in &mut response.accounts {
            if let Some(snapshot) = pool
                .snapshots()
                .into_iter()
                .find(|snapshot| snapshot.profile.id.as_str() == account.profile_id)
            {
                account.availability = super::account_pool_availability(snapshot.availability);
                account.backend_resets_at = snapshot.backend_resets_at.map(|at| at.timestamp());
                account.rate_limits = account_pool_rate_limits(snapshot.rate_limits);
            }
        }
    }
}
