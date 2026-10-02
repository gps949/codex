//! Opt-in automatic redemption of earned rate-limit reset credits.
//!
//! Credits are a limited resource, so automation is deliberately narrow: it
//! only runs when every configured pool account is exhausted (rotating to a
//! free account is always preferred) and only when waiting for the earliest
//! natural reset would take longer than the user-configured threshold.

use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_backend_client::ConsumeRateLimitResetCreditCode;
use codex_config::AutoResetCredits;
use codex_login::AccountAvailability;
use codex_login::AccountPool;
use codex_login::AccountProfileId;
use codex_login::AccountRuntimeStateStore;
use sha1::Digest;

use crate::config::Config;
use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthLease;
use crate::reset_credit_singleflight::ResetCreditRescueAttempt;

const REDEEM_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const REDEEM_PASS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

enum ResetCreditOutcome {
    Reset,
    AlreadyUsable,
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
) -> Option<ResetCreditRescue> {
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

    let leader = match execution_auth.begin_reset_credit_rescue_attempt(failed_lease)? {
        ResetCreditRescueAttempt::Leader(leader) => leader,
        ResetCreditRescueAttempt::Follower(follower) => {
            follower.wait().await;
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
    let _shared_lock = tokio::time::timeout(REDEEM_REQUEST_TIMEOUT, async {
        loop {
            if let Some(lock) = store.try_lock_reset_credit()? {
                return Ok::<_, std::io::Error>(lock);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .ok()?
    .ok()?;
    let deadline = tokio::time::Instant::now() + REDEEM_PASS_TIMEOUT;
    snapshots.sort_by(|left, right| {
        (left.profile.id != failed_profile_id)
            .cmp(&(right.profile.id != failed_profile_id))
            .then_with(|| left.profile.priority.cmp(&right.profile.priority))
            .then_with(|| left.profile.id.as_str().cmp(right.profile.id.as_str()))
    });
    let excluded = store
        .load()
        .ok()?
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
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        // Another request may discover an entitlement refusal during this rescue pass.
        if store.load().ok()?.profiles.iter().any(|profile| {
            profile.profile_id == candidate.profile.id
                && profile
                    .reset_credit_excluded_until
                    .is_some_and(|until| until > Utc::now())
        }) {
            continue;
        }
        // Prefer any free recovery that happened while the previous profile was checked.
        if let Ok(lease) = pool.lease() {
            return Some(ResetCreditRescue {
                profile_id: lease.profile().id.clone(),
                redeemed_profile_id: None,
            });
        }
        let profile_id = candidate.profile.id;
        let previous_epoch = if profile_id == failed_profile_id {
            failed_lease
                .account_lease()
                .and_then(codex_login::AccountLease::quota_reset_at)
        } else {
            candidate.quota_reset_at
        };
        // Another process may have completed the same reset while we waited.
        let saved = store.load().ok()?;
        if let Some(reset_at) = saved
            .profiles
            .iter()
            .find(|entry| entry.profile_id == profile_id)
            .and_then(|entry| entry.quota_reset_at)
            && pool
                .snapshots()
                .iter()
                .find(|snapshot| snapshot.profile.id == profile_id)
                .is_none_or(|snapshot| snapshot.quota_reset_at.is_none_or(|local| local < reset_at))
            && Some(reset_at) > previous_epoch
        {
            pool.apply_quota_reset(&profile_id, reset_at).ok()?;
            return pool.lease().ok().map(|lease| ResetCreditRescue {
                profile_id: lease.profile().id.clone(),
                redeemed_profile_id: None,
            });
        }

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

        let outcome = tokio::time::timeout_at(
            deadline,
            consume_reset_credit_for_profile(&pool, &profile_id, config, &request_id),
        )
        .await
        .unwrap_or(ResetCreditOutcome::Unknown);
        let redeemed = match outcome {
            ResetCreditOutcome::Reset => true,
            ResetCreditOutcome::AlreadyUsable => false,
            ResetCreditOutcome::NoReset => {
                let _ = std::fs::remove_file(&attempt_path);
                continue;
            }
            ResetCreditOutcome::Unknown => return None,
        };

        let mut rescue = reactivate_redeemed_profile(&pool, profile_id.clone())?;
        rescue.redeemed_profile_id = redeemed.then_some(profile_id.clone());
        if let Some(reset_at) = pool
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.profile.id == profile_id)
            .and_then(|snapshot| snapshot.quota_reset_at)
            && let Err(error) = store.record_quota_reset(&profile_id, reset_at)
        {
            tracing::warn!(%profile_id, %error, "failed to persist confirmed quota reset");
        }
        let _ = std::fs::remove_file(&attempt_path);
        return Some(rescue);
    }
    None
}

async fn consume_reset_credit_for_profile(
    pool: &AccountPool,
    profile_id: &AccountProfileId,
    config: &Config,
    redeem_request_id: &str,
) -> ResetCreditOutcome {
    if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
        return ResetCreditOutcome::Unknown;
    }
    let Some(manager) = pool
        .auth_managers()
        .into_iter()
        .find_map(|(id, manager)| (id == *profile_id).then_some(manager))
    else {
        return ResetCreditOutcome::Unknown;
    };
    let Some(auth) = manager.auth().await else {
        return ResetCreditOutcome::Unknown;
    };
    if !auth.uses_codex_backend() {
        return ResetCreditOutcome::Unknown;
    }
    if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
        return ResetCreditOutcome::Unknown;
    }
    let client = codex_backend_client::Client::from_auth(
        config.chatgpt_base_url.clone(),
        &auth,
        config.http_client_factory(),
    );

    match tokio::time::timeout(
        REDEEM_REQUEST_TIMEOUT,
        client.consume_rate_limit_reset_credit(redeem_request_id),
    )
    .await
    {
        Ok(Ok(response)) if response.code == ConsumeRateLimitResetCreditCode::Reset => {
            ResetCreditOutcome::Reset
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
            match tokio::time::timeout(
                REDEEM_REQUEST_TIMEOUT,
                client.get_rate_limits_with_reset_credits(),
            )
            .await
            {
                Ok(Ok(observed))
                    if observed.ordinary_usage_allowed != Some(false)
                        && observed.rate_limits.iter().any(|snapshot| {
                            snapshot.limit_id.as_deref() == Some("codex")
                                && snapshot.spend_control_reached != Some(true)
                                && snapshot.primary.as_ref().is_some_and(|window| {
                                    window.used_percent.is_finite() && window.used_percent < 100.0
                                })
                                && snapshot.secondary.as_ref().is_none_or(|window| {
                                    window.used_percent.is_finite() && window.used_percent < 100.0
                                })
                        }) =>
                {
                    ResetCreditOutcome::AlreadyUsable
                }
                Ok(Ok(_)) => ResetCreditOutcome::NoReset,
                Ok(Err(_)) | Err(_) => ResetCreditOutcome::Unknown,
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
    pool.reset_rate_limits(&profile_id).ok()?;
    match pool.lease() {
        Ok(lease) => {
            tracing::info!(%profile_id, "redeemed one rate-limit reset credit and reactivated the account");
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
