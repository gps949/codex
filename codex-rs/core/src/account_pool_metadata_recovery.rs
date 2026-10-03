//! Bounded metadata discovery for an exhausted subscription pool.

use std::time::Duration;

use codex_login::AccountAvailability;
use codex_login::AccountPool;
use codex_login::AccountRuntimeStateStore;
use codex_login::account_runtime_state::AccountQuotaEvidence;
use futures::FutureExt;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::recovery_coordinator::MAX_COVERAGE_PROFILES;
use crate::execution_auth::recovery_coordinator::MAX_PROFILES_PER_PASS;
use crate::execution_auth::recovery_coordinator::RecoveryProbeAttempt;
use crate::execution_auth::recovery_coordinator::RecoveryProbeKey;
use crate::execution_auth::recovery_coordinator::RecoveryProbeMode;
use crate::execution_auth::recovery_coordinator::RecoveryProbeVerdict;
use crate::execution_auth::recovery_coordinator::quota_owner_key;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProbeOutcome {
    Recovered,
    Busy,
    Unchanged,
}

/// Paid rescue is allowed only after every eligible exhausted seat has a fresh backend denial.
/// Missing owner information, transport failures and oversized or slow pools remain incomplete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpendingRecoveryCoverage {
    Recovered,
    CompleteUnchanged,
    Incomplete,
}

/// Final discovery distinguishes fresh backend denials from incomplete permission reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FinalRecoveryOutcome {
    Recovered,
    ConfirmedExhausted,
    Incomplete { checked: usize, total: usize },
}

enum PassOutcome {
    Recovered,
    CompleteUnchanged,
    Busy,
    Incomplete,
}

/// Rotates through exhausted profiles even when the natural-recovery wait is disabled.
pub(crate) async fn probe_for_recovery(
    execution_auth: &ExecutionAuth,
    config: &Config,
    cancellation: &CancellationToken,
) -> bool {
    probe_candidates(execution_auth, config, cancellation).await == ProbeOutcome::Recovered
}

/// Reuses fresh permission reads and checks all remaining seats within the same four-second bound.
/// This does not debit or replenish the user's cumulative natural-recovery waiting allowance.
pub(crate) async fn coverage_for_spending(
    execution_auth: &ExecutionAuth,
    config: &Config,
    cancellation: &CancellationToken,
) -> SpendingRecoveryCoverage {
    match probe_for_final_recovery(execution_auth, config, cancellation).await {
        FinalRecoveryOutcome::Recovered => SpendingRecoveryCoverage::Recovered,
        FinalRecoveryOutcome::ConfirmedExhausted => SpendingRecoveryCoverage::CompleteUnchanged,
        FinalRecoveryOutcome::Incomplete { .. } => SpendingRecoveryCoverage::Incomplete,
    }
}

/// Checks all remaining seats before ending a request, within one four-second discovery bound.
pub(crate) async fn probe_for_final_recovery(
    execution_auth: &ExecutionAuth,
    config: &Config,
    cancellation: &CancellationToken,
) -> FinalRecoveryOutcome {
    match probe_pass(
        execution_auth,
        config,
        cancellation,
        RecoveryProbeMode::Coverage,
    )
    .await
    {
        PassOutcome::Recovered => FinalRecoveryOutcome::Recovered,
        PassOutcome::CompleteUnchanged => FinalRecoveryOutcome::ConfirmedExhausted,
        PassOutcome::Busy | PassOutcome::Incomplete => {
            let Some(pool) = execution_auth.account_pool() else {
                return FinalRecoveryOutcome::Incomplete {
                    checked: 0,
                    total: 0,
                };
            };
            let store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
            let keys = candidate_keys(&pool, &store, config);
            let checked = keys
                .as_ref()
                .map_or(0, |keys| execution_auth.recovery_probes.checked(keys));
            let total = keys.as_ref().map_or_else(
                || {
                    pool.snapshots()
                        .iter()
                        .filter(|snapshot| {
                            !snapshot.profile.disabled
                                && matches!(
                                    snapshot.availability,
                                    AccountAvailability::Exhausted { .. }
                                )
                        })
                        .count()
                },
                Vec::len,
            );
            FinalRecoveryOutcome::Incomplete { checked, total }
        }
    }
}

pub(super) async fn probe_candidates(
    execution_auth: &ExecutionAuth,
    config: &Config,
    cancellation: &CancellationToken,
) -> ProbeOutcome {
    match probe_pass(
        execution_auth,
        config,
        cancellation,
        RecoveryProbeMode::Batch,
    )
    .await
    {
        PassOutcome::Recovered => ProbeOutcome::Recovered,
        PassOutcome::Busy => ProbeOutcome::Busy,
        PassOutcome::CompleteUnchanged | PassOutcome::Incomplete => ProbeOutcome::Unchanged,
    }
}

async fn probe_pass(
    execution_auth: &ExecutionAuth,
    config: &Config,
    cancellation: &CancellationToken,
    mode: RecoveryProbeMode,
) -> PassOutcome {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let Some(pool) = execution_auth.account_pool() else {
        return PassOutcome::Incomplete;
    };
    let store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    loop {
        if cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline {
            return PassOutcome::Incomplete;
        }
        match store.try_synchronize(&pool) {
            Ok(true) => {}
            Ok(false) if matches!(mode, RecoveryProbeMode::Coverage) => {
                tokio::select! {
                    _ = cancellation.cancelled() => return PassOutcome::Incomplete,
                    _ = tokio::time::sleep_until(deadline) => return PassOutcome::Incomplete,
                    _ = tokio::time::sleep(Duration::from_millis(20)) => continue,
                }
            }
            Ok(false) => return PassOutcome::Busy,
            Err(_) => return PassOutcome::Incomplete,
        }
        if pool.lease().is_ok() {
            return PassOutcome::Recovered;
        }
        let Some(keys) = candidate_keys(&pool, &store, config) else {
            if matches!(mode, RecoveryProbeMode::Coverage) {
                tokio::select! {
                    _ = cancellation.cancelled() => return PassOutcome::Incomplete,
                    _ = tokio::time::sleep_until(deadline) => return PassOutcome::Incomplete,
                    _ = tokio::time::sleep(Duration::from_millis(20)) => continue,
                }
            }
            return PassOutcome::Incomplete;
        };
        if matches!(mode, RecoveryProbeMode::Coverage) && keys.len() > MAX_COVERAGE_PROFILES {
            return PassOutcome::Incomplete;
        }
        if execution_auth.recovery_probes.covers(&keys) {
            return PassOutcome::CompleteUnchanged;
        }
        let leader = match execution_auth.recovery_probes.begin(&keys, mode) {
            RecoveryProbeAttempt::Leader(leader) => leader,
            RecoveryProbeAttempt::NoWork => return PassOutcome::Incomplete,
            RecoveryProbeAttempt::Follower(mut completion) => {
                let wait = async {
                    while !*completion.borrow_and_update() {
                        if completion.changed().await.is_err() {
                            break;
                        }
                    }
                };
                tokio::select! {
                    _ = cancellation.cancelled() => return PassOutcome::Incomplete,
                    _ = tokio::time::sleep_until(deadline) => return PassOutcome::Incomplete,
                    _ = wait => {}
                }
                if matches!(mode, RecoveryProbeMode::Batch) {
                    return if pool.lease().is_ok() {
                        PassOutcome::Recovered
                    } else {
                        PassOutcome::Incomplete
                    };
                }
                continue;
            }
        };
        let managers = pool.auth_managers();
        let leader_ref = &leader;
        let jobs: Vec<_> = leader
            .keys
            .iter()
            .cloned()
            .map(|key| {
                let manager = managers
                    .iter()
                    .find(|(id, _)| id == &key.snapshot.profile.id)
                    .map(|(_, manager)| manager.clone());
                let store = store.clone();
                let pool = pool.clone();
                async move {
                    let request = async {
                        let manager = manager?;
                        let (auth, mut factory) = manager.auth_with_http_client_factory().await?;
                        if !auth.is_chatgpt_auth() {
                            return None;
                        }
                        let mut base_url = config.chatgpt_base_url.clone();
                        if let Some(clients) = manager.maintenance_clients(&auth).await.ok()? {
                            factory = clients.http_client_factory;
                            base_url = clients.chatgpt_base_url;
                        }
                        let id = &key.snapshot.profile.id;
                        let probe = loop {
                            if cancellation.is_cancelled() {
                                return None;
                            }
                            if let Some(probe) = store.capture_quota_probe(&pool, id, &auth).ok()? {
                                break probe;
                            }
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        };
                        let client =
                            codex_backend_client::Client::from_auth(base_url, &auth, factory);
                        leader_ref.started(&key);
                        let observed = client.get_rate_limits_with_reset_credits().await.ok()?;
                        if cancellation.is_cancelled() {
                            return None;
                        }
                        if observed.ordinary_usage_allowed == Some(true) {
                            let recovered = store
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
                                .filter(|recovered| *recovered)
                                .is_some();
                            if recovered
                                && let Some(snapshot) = pool
                                    .snapshots()
                                    .into_iter()
                                    .find(|snapshot| &snapshot.profile.id == id)
                                && let (Some(account), Some(user)) =
                                    (observed.account_id.as_deref(), observed.user_id.as_deref())
                            {
                                leader_ref.recovered(
                                    &key,
                                    snapshot,
                                    quota_owner_key(account, user),
                                );
                            }
                            return recovered.then_some(RecoveryProbeVerdict::Recovered);
                        }
                        // A denied ordinary Codex permission is evidence only for this exact seat.
                        // Query success alone, missing owners or model-specific buckets prove nothing.
                        let denied = observed.ordinary_usage_allowed == Some(false)
                            && observed.account_id.as_ref() == auth.get_account_id().as_ref()
                            && observed.user_id.as_ref() == auth.get_chatgpt_user_id().as_ref()
                            && observed
                                .rate_limits
                                .iter()
                                .filter(|snapshot| snapshot.limit_id.as_deref() == Some("codex"))
                                .count()
                                == 1
                            && *manager.auth_change_state_receiver().borrow() == key.auth_revision;
                        let denied = if denied {
                            loop {
                                if cancellation.is_cancelled()
                                    || *manager.auth_change_state_receiver().borrow()
                                        != key.auth_revision
                                {
                                    return None;
                                }
                                if store.validate_profile_auth(&pool, id, &auth).ok()? {
                                    break true;
                                }
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                        } else {
                            false
                        };
                        Some(if denied {
                            RecoveryProbeVerdict::Rejected
                        } else {
                            RecoveryProbeVerdict::Unknown
                        })
                    };
                    let verdict = tokio::time::timeout(Duration::from_secs(3), request)
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or(RecoveryProbeVerdict::Unknown);
                    (key, verdict)
                }
                .boxed()
            })
            .collect();
        let pass = async {
            let mut pending = futures::stream::iter(jobs).buffer_unordered(MAX_PROFILES_PER_PASS);
            while let Some((key, verdict)) = pending.next().await {
                leader.observe(&key, verdict);
                if matches!(verdict, RecoveryProbeVerdict::Recovered) {
                    return true;
                }
            }
            false
        };
        let recovered = tokio::select! {
            _ = cancellation.cancelled() => return PassOutcome::Incomplete,
            result = tokio::time::timeout_at(deadline, pass) => match result {
                Ok(recovered) => recovered,
                Err(_) => return PassOutcome::Incomplete,
            },
        };
        drop(leader);
        if recovered || pool.lease().is_ok() {
            return PassOutcome::Recovered;
        }
        // Reimport concurrent failure/reset/entitlement state before trusting the cached coverage.
        if !store.try_synchronize(&pool).unwrap_or(false) {
            if matches!(mode, RecoveryProbeMode::Coverage) {
                tokio::select! {
                    _ = cancellation.cancelled() => return PassOutcome::Incomplete,
                    _ = tokio::time::sleep_until(deadline) => return PassOutcome::Incomplete,
                    _ = tokio::time::sleep(Duration::from_millis(20)) => continue,
                }
            }
            return PassOutcome::Incomplete;
        }
        if pool.lease().is_ok() {
            return PassOutcome::Recovered;
        }
        let complete = candidate_keys(&pool, &store, config)
            .is_some_and(|keys| execution_auth.recovery_probes.covers(&keys));
        if complete {
            return PassOutcome::CompleteUnchanged;
        }
        if matches!(mode, RecoveryProbeMode::Batch) {
            return PassOutcome::Incomplete;
        }
    }
}

fn candidate_keys(
    pool: &AccountPool,
    store: &AccountRuntimeStateStore,
    config: &Config,
) -> Option<Vec<RecoveryProbeKey>> {
    let shared = store.try_load().ok()??;
    let profiles = codex_login::AccountProfileStore::new(config.codex_home.to_path_buf());
    let manifest_version = std::fs::metadata(profiles.manifest_path())
        .ok()?
        .modified()
        .ok()?;
    let managers = pool.auth_managers();
    let mut keys = pool
        .snapshots()
        .into_iter()
        .filter(|snapshot| {
            !snapshot.profile.disabled
                && matches!(snapshot.availability, AccountAvailability::Exhausted { .. })
                && !shared.profiles.iter().any(|profile| {
                    profile.profile_id == snapshot.profile.id
                        && profile
                            .reset_credit_excluded_until
                            .is_some_and(|until| until > chrono::Utc::now())
                })
        })
        .map(|mut snapshot| {
            // Selecting a different exhausted seat does not change this seat's quota permission.
            snapshot.is_active = false;
            let manager = managers
                .iter()
                .find(|(id, _)| id == &snapshot.profile.id)?
                .1
                .clone();
            let auth_revision = *manager.auth_change_state_receiver().borrow();
            let credential_versions = ["auth.json", ".account-credentials-version"].map(|name| {
                let metadata =
                    std::fs::metadata(snapshot.profile.credential_home.join(name)).ok()?;
                Some((metadata.modified().ok()?, metadata.len()))
            });
            Some(RecoveryProbeKey {
                quota_owner: manager.auth_cached().and_then(|auth| {
                    Some(quota_owner_key(
                        &auth.get_account_id()?,
                        &auth.get_chatgpt_user_id()?,
                    ))
                }),
                snapshot,
                auth_revision,
                manifest_version,
                credential_versions,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let now = chrono::Utc::now();
    keys.sort_by_key(|key| {
        let due = key
            .snapshot
            .rate_limits
            .primary
            .iter()
            .chain(key.snapshot.rate_limits.secondary.iter())
            .any(|window| window.resets_at.is_some_and(|reset| reset <= now));
        (
            !due,
            key.snapshot.quota_failure_at,
            key.snapshot.profile.priority,
            key.snapshot.profile.id.as_str().to_owned(),
        )
    });
    Some(keys)
}

#[cfg(test)]
#[path = "account_pool_metadata_recovery_tests.rs"]
mod tests;
