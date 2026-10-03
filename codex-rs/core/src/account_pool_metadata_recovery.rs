//! Bounded metadata discovery for an exhausted subscription pool.

use std::time::Duration;

use codex_login::AccountAvailability;
use codex_login::AccountRuntimeStateStore;
use codex_login::account_runtime_state::AccountQuotaEvidence;
use futures::FutureExt;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::execution_auth::ExecutionAuth;

pub(super) const MAX_PROFILES_PER_PASS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProbeOutcome {
    Recovered,
    Busy,
    Unchanged,
}

/// Performs one passive recovery pass before a new exhausted turn is rejected or starts waiting.
pub(crate) async fn probe_for_recovery(
    execution_auth: &ExecutionAuth,
    config: &Config,
    cancellation: &CancellationToken,
) -> bool {
    probe_candidates(execution_auth, config, cancellation, /*cursor*/ 0).await
        == ProbeOutcome::Recovered
}

pub(super) async fn probe_candidates(
    execution_auth: &ExecutionAuth,
    config: &Config,
    cancellation: &CancellationToken,
    cursor: usize,
) -> ProbeOutcome {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let Some(pool) = execution_auth.account_pool() else {
        return ProbeOutcome::Unchanged;
    };
    if cancellation.is_cancelled() {
        return ProbeOutcome::Unchanged;
    }
    let store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    match store.try_synchronize(&pool) {
        Ok(true) => {}
        Ok(false) => return ProbeOutcome::Busy,
        Err(_) => return ProbeOutcome::Unchanged,
    }
    if cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline {
        return ProbeOutcome::Unchanged;
    }
    if pool.lease().is_ok() {
        return ProbeOutcome::Recovered;
    }
    let candidates: Vec<_> = pool
        .snapshots()
        .into_iter()
        .filter(|snapshot| {
            !snapshot.profile.disabled
                && matches!(snapshot.availability, AccountAvailability::Exhausted { .. })
        })
        .collect();
    if candidates.is_empty() {
        return ProbeOutcome::Unchanged;
    }
    let count = candidates.len();
    let managers = pool.auth_managers();
    let jobs: Vec<_> = candidates
        .into_iter()
        .cycle()
        .skip(cursor % count)
        .take(count.min(MAX_PROFILES_PER_PASS))
        .filter_map(|candidate| {
            let manager = managers
                .iter()
                .find(|(id, _)| id == &candidate.profile.id)?
                .1
                .clone();
            let store = store.clone();
            let pool = pool.clone();
            let id = candidate.profile.id;
            Some(
                async move {
                    let request = async {
                        let (auth, mut factory) = manager.auth_with_http_client_factory().await?;
                        if !auth.is_chatgpt_auth() {
                            return None;
                        }
                        let mut base_url = config.chatgpt_base_url.clone();
                        if let Some(clients) = manager.maintenance_clients(&auth).await.ok()? {
                            factory = clients.http_client_factory;
                            base_url = clients.chatgpt_base_url;
                        }
                        let probe = store.capture_quota_probe(&pool, &id, &auth).ok()??;
                        let client =
                            codex_backend_client::Client::from_auth(base_url, &auth, factory);
                        let observed = client.get_rate_limits_with_reset_credits().await.ok()?;
                        if cancellation.is_cancelled() {
                            return None;
                        }
                        store
                            .reconcile_quota_probe(
                                &pool,
                                probe,
                                AccountQuotaEvidence {
                                    rate_limits: &observed.rate_limits,
                                    ordinary_usage_allowed: observed.ordinary_usage_allowed,
                                    account_id: observed.account_id.as_deref(),
                                    user_id: observed.user_id.as_deref(),
                                },
                            )
                            .ok()
                    };
                    tokio::time::timeout(Duration::from_secs(3), request)
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or(false)
                }
                .boxed(),
            )
        })
        .collect();
    let pass = async move {
        let mut pending = futures::stream::iter(jobs).buffer_unordered(MAX_PROFILES_PER_PASS);
        while let Some(recovered) = pending.next().await {
            if recovered {
                return true;
            }
        }
        pool.lease().is_ok()
    };
    tokio::select! {
        _ = cancellation.cancelled() => ProbeOutcome::Unchanged,
        result = tokio::time::timeout_at(deadline, pass) => if result.unwrap_or(false) { ProbeOutcome::Recovered } else { ProbeOutcome::Unchanged },
    }
}

#[cfg(test)]
#[path = "account_pool_metadata_recovery_tests.rs"]
mod tests;
