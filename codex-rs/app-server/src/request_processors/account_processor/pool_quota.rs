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
use codex_login::AuthManager;
use futures::FutureExt;
use futures::StreamExt;

use super::BackendClient;
use super::account_pool_rate_limits;

pub(super) async fn refresh(config: &Config, response: &mut AccountPoolReadResponse) {
    if !response.enabled {
        return;
    }
    let Ok(records) =
        AccountProfileStore::new(config.codex_home.to_path_buf()).load_profile_records()
    else {
        return;
    };
    let store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    // A previous panel probe may have reached disk before the runtime's next poll.
    if let Ok(saved) = store.load() {
        for account in &mut response.accounts {
            if let Some(profile) = saved
                .profiles
                .iter()
                .find(|profile| profile.profile_id.as_str() == account.profile_id)
            {
                let cached = account_pool_rate_limits(profile.rate_limits.clone());
                if cached.observed_at > account.rate_limits.observed_at {
                    account.rate_limits = cached;
                }
            }
        }
    }
    let jobs: Vec<_> = records
        .into_iter()
        .filter(|record| {
            response.accounts.iter().any(|account| {
                account.profile_id == record.profile.id.as_str()
                    && !matches!(
                        account.availability,
                        AccountPoolAvailability::Disabled
                            | AccountPoolAvailability::AuthenticationUnavailable { .. }
                    )
            })
        })
        .map(|record| {
            let mut auth_config = config.auth_config();
            auth_config.codex_home = record.profile.credential_home.clone();
            let base_url = config.chatgpt_base_url.clone();
            let factory = config.http_client_factory();
            async move {
                let observed_at = Utc::now();
                let request = async {
                    let manager = AuthManager::shared_from_auth_config(
                        auth_config,
                        /*enable_codex_api_key_env*/ false,
                    )
                    .await
                    .ok()?;
                    let auth = manager
                        .auth()
                        .await
                        .filter(codex_login::CodexAuth::is_chatgpt_auth)?;
                    let client = BackendClient::from_auth(base_url, &auth, factory);
                    let snapshots = client.get_rate_limits_many().await.ok()?;
                    let snapshot = snapshots
                        .iter()
                        .find(|snapshot| snapshot.limit_id.as_deref() == Some("codex"))
                        .or_else(|| snapshots.first())?;
                    let window = |window: &codex_protocol::protocol::RateLimitWindow| {
                        AccountRateLimitWindow {
                            used_percent: window.used_percent,
                            resets_at: window
                                .resets_at
                                .and_then(|time| DateTime::from_timestamp(time, 0)),
                        }
                    };
                    Some(AccountRateLimits {
                        primary: snapshot.primary.as_ref().map(window),
                        secondary: snapshot.secondary.as_ref().map(window),
                        observed_at: Some(observed_at),
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
}
