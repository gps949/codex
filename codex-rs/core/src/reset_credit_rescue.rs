//! Opt-in automatic redemption of earned rate-limit reset credits.
//!
//! Credits are a limited resource, so automation is deliberately narrow: it
//! only runs when every configured pool account is exhausted (rotating to a
//! free account is always preferred) and only when waiting for the earliest
//! natural reset would take longer than the user-configured threshold.

#[cfg(test)]
#[path = "reset_credit_operation.rs"]
mod operation;

use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_backend_client::ConsumeRateLimitResetCreditCode;
use codex_config::AutoResetCredits;
use codex_login::AccountAvailability;
use codex_login::AccountPool;
use codex_login::AccountProfileId;
use codex_login::AccountRuntimeStateStore;
use codex_login::account_runtime_state::AccountQuotaEvidence;
use codex_login::account_runtime_state::AccountQuotaProbe;
use sha1::Digest;
use tokio_util::sync::CancellationToken;

use crate::account_pool_recovery::SpendingRecoveryCoverage;
use crate::account_pool_recovery::coverage_for_spending;
use crate::config::Config;
use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthLease;
use crate::reset_credit_singleflight::ResetCreditRescueAttempt;

const REDEEM_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const REDEEM_PASS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

enum ResetCreditOutcome {
    Reset(AccountQuotaProbe),
    AlreadyUsable(
        AccountQuotaProbe,
        codex_backend_client::RateLimitsWithResetCredits,
    ),
    NoReset,
    Unknown,
}

/// A recovered pool after redemption or a concurrent recovery.
pub(crate) struct ResetCreditRescue {
    pub(crate) profile_id: AccountProfileId,
    pub(crate) redeemed_profile_id: Option<AccountProfileId>,
}

/// Pure decision rule so the waiting policy is unit-testable: redeeming is only worth it when
/// automation is enabled and the pool would otherwise stay unusable for longer than `min_wait`.
fn should_redeem(
    mode: AutoResetCredits,
    min_wait: Duration,
    earliest_reset: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    match mode {
        AutoResetCredits::Never => false,
        AutoResetCredits::WhenPoolExhausted => match earliest_reset {
            // A nearby natural reset is free; let the pool recover on its own.
            Some(reset) => reset - now > min_wait,
            // No known reset means the pool cannot recover without intervention.
            None => true,
        },
    }
}

/// Attempts one successful reset across exhausted profiles, starting with the last failed one.
/// A definitive no-credit result permits the next profile; an ambiguous result stops the pass.
pub(crate) async fn try_reset_credit_rescue(
    execution_auth: &ExecutionAuth,
    failed_lease: &ExecutionAuthLease,
    config: &Config,
    cancellation: &CancellationToken,
) -> Option<ResetCreditRescue> {
    if cancellation.is_cancelled() {
        return None;
    }
    let pool = execution_auth.account_pool()?;
    if let Ok(lease) = pool.lease() {
        return Some(ResetCreditRescue {
            profile_id: lease.profile().id.clone(),
            redeemed_profile_id: None,
        });
    }
    let mode = config.account_pool.effective_auto_reset_credits();
    if mode == AutoResetCredits::Never {
        return None;
    }
    let failed_profile_id = failed_lease.profile_id()?.clone();

    let now = Utc::now();
    let mut snapshots = pool.snapshots();
    if !snapshots.iter().any(|snapshot| {
        snapshot.profile.id == failed_profile_id
            && matches!(snapshot.availability, AccountAvailability::Exhausted { .. })
    }) || snapshots
        .iter()
        .any(|snapshot| matches!(snapshot.availability, AccountAvailability::Available))
    {
        return None;
    }
    let earliest_reset = snapshots
        .iter()
        .filter_map(|snapshot| match &snapshot.availability {
            AccountAvailability::Exhausted { .. } => snapshot.backend_resets_at,
            AccountAvailability::Available
            | AccountAvailability::AuthenticationUnavailable { .. }
            | AccountAvailability::Disabled => None,
        })
        .min();
    let min_wait = Duration::try_minutes(
        config
            .account_pool
            .effective_reset_credit_min_wait_minutes(),
    )
    .unwrap_or(Duration::MAX);
    if !should_redeem(mode, min_wait, earliest_reset, now) {
        tracing::info!(
            profile_id = %failed_profile_id,
            ?earliest_reset,
            "skipping automatic reset-credit redemption; waiting for the natural reset is cheaper"
        );
        return None;
    }

    match coverage_for_spending(execution_auth, config, cancellation).await {
        SpendingRecoveryCoverage::Recovered => {
            return pool.lease().ok().map(|lease| ResetCreditRescue {
                profile_id: lease.profile().id.clone(),
                redeemed_profile_id: None,
            });
        }
        SpendingRecoveryCoverage::CompleteUnchanged => {}
        SpendingRecoveryCoverage::Incomplete => return None,
    }
    let leader = match execution_auth.begin_reset_credit_rescue_attempt(failed_lease)? {
        ResetCreditRescueAttempt::Leader(leader) => leader,
        ResetCreditRescueAttempt::Follower(follower) => {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return None,
                _ = follower.wait() => {}
            }
            return pool.lease().ok().map(|lease| ResetCreditRescue {
                profile_id: lease.profile().id.clone(),
                redeemed_profile_id: None,
            });
        }
        ResetCreditRescueAttempt::AlreadyFinished => {
            return pool.lease().ok().map(|lease| ResetCreditRescue {
                profile_id: lease.profile().id.clone(),
                redeemed_profile_id: None,
            });
        }
    };

    let store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    let lock = tokio::time::timeout(REDEEM_REQUEST_TIMEOUT, async {
        loop {
            if let Some(lock) = store.try_lock_reset_credit()? {
                return Ok::<_, std::io::Error>(lock);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
    let _shared_lock = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return None,
        result = lock => result.ok()?.ok()?,
    };
    let deadline = tokio::time::Instant::now() + REDEEM_PASS_TIMEOUT;
    // The lock serializes spending, but another process may have restored any profile while
    // this process was waiting. Import the entire pool before choosing which credit to spend.
    let shared = load_synced_credit_state(&store, &pool, deadline, cancellation).await?;
    if let Ok(lease) = pool.lease() {
        return Some(ResetCreditRescue {
            profile_id: lease.profile().id.clone(),
            redeemed_profile_id: None,
        });
    }
    // A restored or newly added seat must win even if it changed while the spending lock was held.
    if coverage_for_spending(execution_auth, config, cancellation).await
        != SpendingRecoveryCoverage::CompleteUnchanged
    {
        return pool.lease().ok().map(|lease| ResetCreditRescue {
            profile_id: lease.profile().id.clone(),
            redeemed_profile_id: None,
        });
    }
    snapshots = pool.snapshots();
    snapshots.sort_by(|left, right| {
        (left.profile.id != failed_profile_id)
            .cmp(&(right.profile.id != failed_profile_id))
            .then_with(|| left.profile.priority.cmp(&right.profile.priority))
            .then_with(|| left.profile.id.as_str().cmp(right.profile.id.as_str()))
    });
    let excluded = shared
        .profiles
        .into_iter()
        .filter(|profile| {
            profile
                .reset_credit_excluded_until
                .is_some_and(|until| until > Utc::now())
        })
        .map(|profile| profile.profile_id)
        .collect::<std::collections::HashSet<_>>();
    for candidate in snapshots.into_iter().filter(|snapshot| {
        matches!(snapshot.availability, AccountAvailability::Exhausted { .. })
            && !excluded.contains(&snapshot.profile.id)
    }) {
        if cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline {
            return None;
        }
        // Refresh every profile, including free recoveries and entitlement exclusions,
        // before the next candidate. A per-candidate epoch check misses a restored standby.
        let shared = load_synced_credit_state(&store, &pool, deadline, cancellation).await?;
        if let Ok(lease) = pool.lease() {
            return Some(ResetCreditRescue {
                profile_id: lease.profile().id.clone(),
                redeemed_profile_id: None,
            });
        }
        let coverage = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return None,
            _ = tokio::time::sleep_until(deadline) => return None,
            coverage = coverage_for_spending(execution_auth, config, cancellation) => coverage,
        };
        if coverage != SpendingRecoveryCoverage::CompleteUnchanged {
            return pool.lease().ok().map(|lease| ResetCreditRescue {
                profile_id: lease.profile().id.clone(),
                redeemed_profile_id: None,
            });
        }
        let current = pool.snapshots();
        let earliest_reset = current
            .iter()
            .filter_map(|snapshot| {
                matches!(snapshot.availability, AccountAvailability::Exhausted { .. })
                    .then_some(snapshot.backend_resets_at)
                    .flatten()
            })
            .min();
        if !should_redeem(mode, min_wait, earliest_reset, Utc::now()) {
            return None;
        }
        if shared.profiles.iter().any(|profile| {
            profile.profile_id == candidate.profile.id
                && profile
                    .reset_credit_excluded_until
                    .is_some_and(|until| until > Utc::now())
        }) {
            continue;
        }
        let profile_id = candidate.profile.id;
        // Reuse an id for an ambiguous, recent attempt rather than spend another
        // credit after a transport timeout. This file contains no credentials.
        let profile_key = format!("{:x}", sha1::Sha1::digest(profile_id.as_str().as_bytes()));
        let attempt_path = config
            .codex_home
            .join(format!(".rate-limit-reset-credit-{profile_key}.json"));
        let Some(failed_snapshot) = pool
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.profile.id == profile_id)
        else {
            continue;
        };
        let reset_key = match failed_snapshot.availability {
            AccountAvailability::Exhausted { .. } => failed_snapshot
                .backend_resets_at
                .map(|reset| reset.timestamp() / 60),
            AccountAvailability::Available
            | AccountAvailability::AuthenticationUnavailable { .. }
            | AccountAvailability::Disabled => continue,
        };
        let quota_epoch = failed_snapshot
            .quota_reset_at
            .map(|reset| reset.timestamp_millis());
        let previous = std::fs::metadata(&attempt_path)
            .ok()
            .filter(|metadata| metadata.len() <= 4096)
            .and_then(|_| std::fs::read(&attempt_path).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let previous = previous.as_ref().filter(|attempt| {
            attempt["profileId"].as_str() == Some(profile_id.as_str())
                && attempt["resetKey"].as_i64() == reset_key
                && attempt["quotaEpoch"].as_i64() == quota_epoch
                && attempt["attemptedAt"]
                    .as_i64()
                    .is_some_and(|time| now.timestamp().saturating_sub(time) < 300)
        });
        let first_attempt_at = previous
            .and_then(|attempt| attempt["attemptedAt"].as_i64())
            .unwrap_or_else(|| now.timestamp());
        let new_request_id = if profile_id == failed_profile_id {
            leader.redeem_request_id().to_string()
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        let request_id = previous
            .and_then(|attempt| attempt["requestId"].as_str())
            .unwrap_or(&new_request_id)
            .to_string();
        std::fs::write(
            &attempt_path,
            serde_json::to_vec(&serde_json::json!({
                "profileId": profile_id.as_str(), "resetKey": reset_key, "quotaEpoch": quota_epoch,
                "attemptedAt": first_attempt_at, "requestId": request_id,
            }))
            .ok()?,
        )
        .ok()?;

        let outcome = tokio::select! {
            biased;
            _ = cancellation.cancelled() => ResetCreditOutcome::Unknown,
            result = tokio::time::timeout_at(deadline,
                consume_reset_credit_for_profile(&pool, &profile_id, config, &request_id, cancellation)) =>
                result.unwrap_or(ResetCreditOutcome::Unknown),
        };
        // Keep the persisted request ID whenever a POST might already have escaped cancellation.
        if cancellation.is_cancelled() {
            return None;
        }
        let (redeemed, applied) = match outcome {
            ResetCreditOutcome::Reset(probe) => (
                true,
                store.confirm_quota_reset(&pool, probe, Utc::now()).ok()?,
            ),
            ResetCreditOutcome::AlreadyUsable(probe, observed) => (
                false,
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
                    .ok()?,
            ),
            ResetCreditOutcome::NoReset => {
                let _ = std::fs::remove_file(&attempt_path);
                continue;
            }
            ResetCreditOutcome::Unknown => return None,
        };
        if !applied {
            // A concurrent refusal can correctly reject the old probe after the backend reset.
            // Confirm a newer free recovery without erasing that refusal or sending another POST.
            let coverage = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return None,
                _ = tokio::time::sleep_until(deadline) => return None,
                coverage = coverage_for_spending(execution_auth, config, cancellation) => coverage,
            };
            if coverage != SpendingRecoveryCoverage::Recovered {
                return None;
            }
        }

        let mut rescue = reactivate_redeemed_profile(&pool, profile_id.clone())?;
        rescue.redeemed_profile_id = redeemed.then_some(profile_id.clone());
        let _ = std::fs::remove_file(&attempt_path);
        return Some(rescue);
    }
    None
}

async fn load_synced_credit_state(
    store: &AccountRuntimeStateStore,
    pool: &AccountPool,
    deadline: tokio::time::Instant,
    cancellation: &CancellationToken,
) -> Option<codex_login::AccountRuntimeState> {
    loop {
        if cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline {
            return None;
        }
        if store.try_synchronize(pool).ok()?
            && let Some(state) = store.try_load().ok()?
        {
            return Some(state);
        }
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return None,
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
    }
}

async fn consume_reset_credit_for_profile(
    pool: &AccountPool,
    profile_id: &AccountProfileId,
    config: &Config,
    redeem_request_id: &str,
    cancellation: &CancellationToken,
) -> ResetCreditOutcome {
    if cancellation.is_cancelled()
        || codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home)
    {
        return ResetCreditOutcome::Unknown;
    }
    let Some(manager) = pool
        .auth_managers()
        .into_iter()
        .find_map(|(id, manager)| (id == *profile_id).then_some(manager))
    else {
        return ResetCreditOutcome::Unknown;
    };
    let expected_auth = manager.auth_cached();
    manager.reload().await;
    let Some((auth, mut factory)) = manager.auth_with_http_client_factory().await else {
        return ResetCreditOutcome::Unknown;
    };
    if !auth.uses_codex_backend()
        || expected_auth.as_ref().is_some_and(|expected| {
            expected.get_account_id() != auth.get_account_id()
                || expected.get_chatgpt_user_id() != auth.get_chatgpt_user_id()
        })
    {
        return ResetCreditOutcome::Unknown;
    }
    let changes = manager.auth_change_state_receiver();
    let owner_generation = changes.borrow().owner_generation;
    let still_owned = || {
        changes.borrow().owner_generation == owner_generation
            && manager.auth_cached().is_some_and(|current| {
                current.get_account_id() == auth.get_account_id()
                    && current.get_chatgpt_user_id() == auth.get_chatgpt_user_id()
            })
    };
    let mut base_url = config.chatgpt_base_url.clone();
    match manager.maintenance_clients(&auth).await {
        Ok(Some(clients)) => {
            factory = clients.http_client_factory;
            base_url = clients.chatgpt_base_url;
        }
        Ok(None) => {}
        Err(error) => {
            tracing::info!(%profile_id, %error, "automatic reset-credit maintenance policy unavailable");
            return ResetCreditOutcome::Unknown;
        }
    }
    if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
        return ResetCreditOutcome::Unknown;
    }
    if cancellation.is_cancelled() || !still_owned() {
        return ResetCreditOutcome::Unknown;
    }
    let store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    let Ok(Some(probe)) = store.capture_quota_probe(pool, profile_id, &auth) else {
        return ResetCreditOutcome::Unknown;
    };
    let client = codex_backend_client::Client::from_auth(base_url, &auth, factory);

    let response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return ResetCreditOutcome::Unknown,
        result = tokio::time::timeout(REDEEM_REQUEST_TIMEOUT,
            client.consume_rate_limit_reset_credit(redeem_request_id)) => result,
    };
    manager.reload().await;
    if cancellation.is_cancelled() || !still_owned() {
        return ResetCreditOutcome::Unknown;
    }
    match response {
        Ok(Ok(response))
            if response.code == ConsumeRateLimitResetCreditCode::Reset
                && response.windows_reset >= 2 =>
        {
            ResetCreditOutcome::Reset(probe)
        }
        Ok(Ok(response)) if response.code == ConsumeRateLimitResetCreditCode::Reset => {
            ResetCreditOutcome::Unknown
        }
        Ok(Ok(response))
            if matches!(
                response.code,
                ConsumeRateLimitResetCreditCode::AlreadyRedeemed
                    | ConsumeRateLimitResetCreditCode::NothingToReset
            ) =>
        {
            // The previous POST may have succeeded despite a lost response.
            // Confirm the bound seat's current quota before unblocking it.
            let observed = tokio::time::timeout(
                REDEEM_REQUEST_TIMEOUT,
                client.get_rate_limits_with_reset_credits(),
            )
            .await;
            manager.reload().await;
            if !still_owned() {
                return ResetCreditOutcome::Unknown;
            }
            match observed {
                Ok(Ok(observed))
                    if observed.account_id.as_ref() == auth.get_account_id().as_ref()
                        && observed.user_id.as_ref() == auth.get_chatgpt_user_id().as_ref()
                        && observed.ordinary_usage_allowed == Some(true)
                        && observed.rate_limits.iter().any(|snapshot| {
                            snapshot.limit_id.as_deref() == Some("codex")
                                && snapshot.spend_control_reached != Some(true)
                                && snapshot.primary.as_ref().is_some_and(|window| {
                                    window.used_percent.is_finite()
                                        && window.used_percent >= 0.0
                                        && window.used_percent < 100.0
                                        && window.window_minutes == Some(300)
                                })
                                && snapshot.secondary.as_ref().is_none_or(|window| {
                                    window.used_percent.is_finite()
                                        && window.used_percent >= 0.0
                                        && window.used_percent < 100.0
                                })
                        }) =>
                {
                    ResetCreditOutcome::AlreadyUsable(probe, observed)
                }
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => ResetCreditOutcome::Unknown,
            }
        }
        Ok(Ok(response)) => {
            tracing::info!(
                %profile_id,
                code = ?response.code,
                "automatic reset-credit redemption did not reset a rate-limit window"
            );
            ResetCreditOutcome::NoReset
        }
        Ok(Err(error)) => {
            tracing::info!(
                %profile_id,
                %error,
                "automatic reset-credit redemption failed; surfacing the original usage-limit error"
            );
            ResetCreditOutcome::Unknown
        }
        Err(_) => {
            tracing::warn!(%profile_id, "automatic reset-credit redemption timed out");
            ResetCreditOutcome::Unknown
        }
    }
}

fn reactivate_redeemed_profile(
    pool: &AccountPool,
    profile_id: AccountProfileId,
) -> Option<ResetCreditRescue> {
    match pool.lease() {
        Ok(lease) => {
            tracing::info!(%profile_id, "confirmed rate-limit recovery is available in the account pool");
            Some(ResetCreditRescue {
                profile_id: lease.profile().id.clone(),
                redeemed_profile_id: Some(profile_id),
            })
        }
        Err(error) => {
            tracing::warn!(
                %profile_id,
                %error,
                "reset credit was redeemed but the profile could not be reactivated"
            );
            None
        }
    }
}

#[cfg(test)]
#[path = "reset_credit_rescue_tests.rs"]
mod tests;
