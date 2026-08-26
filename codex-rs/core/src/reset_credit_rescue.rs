//! Opt-in automatic redemption of earned rate-limit reset credits.
//!
//! Credits are a limited resource, so automation is deliberately narrow: it
//! only runs when every configured pool account is exhausted (rotating to a
//! free account is always preferred) and only when waiting for the earliest
//! natural reset would take longer than the user-configured threshold.

use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_config::AutoResetCredits;
use codex_login::AccountAvailability;
use codex_login::AccountProfileId;

use crate::config::Config;
use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthLease;

const REDEEM_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A successfully redeemed credit: the failed profile is available and active again.
pub(crate) struct ResetCreditRescue {
    pub(crate) profile_id: AccountProfileId,
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

/// Attempts to redeem one reset credit for the account that just exhausted the pool. Returns
/// `Some` only when the credit was consumed and the profile has been force-activated again.
pub(crate) async fn try_reset_credit_rescue(
    execution_auth: &ExecutionAuth,
    failed_lease: &ExecutionAuthLease,
    config: &Config,
) -> Option<ResetCreditRescue> {
    let mode = config.account_pool.effective_auto_reset_credits();
    if mode == AutoResetCredits::Never {
        return None;
    }
    let pool = execution_auth.account_pool()?;
    let profile_id = failed_lease.profile_id()?.clone();

    let now = Utc::now();
    let earliest_reset = pool
        .snapshots()
        .into_iter()
        .filter_map(|snapshot| match snapshot.availability {
            AccountAvailability::Exhausted { resets_at } => resets_at,
            AccountAvailability::Available
            | AccountAvailability::AuthenticationUnavailable { .. }
            | AccountAvailability::Disabled => None,
        })
        .min();
    let min_wait = Duration::minutes(
        config
            .account_pool
            .effective_reset_credit_min_wait_minutes(),
    );
    if !should_redeem(mode, min_wait, earliest_reset, now) {
        tracing::info!(
            %profile_id,
            ?earliest_reset,
            "skipping automatic reset-credit redemption; waiting for the natural reset is cheaper"
        );
        return None;
    }

    let manager = pool
        .auth_managers()
        .into_iter()
        .find_map(|(id, manager)| (id == profile_id).then_some(manager))?;
    let auth = manager.auth().await?;
    if !auth.uses_codex_backend() {
        return None;
    }
    let client = codex_backend_client::Client::from_auth(
        config.chatgpt_base_url.clone(),
        &auth,
        config.http_client_factory(),
    );

    let redeem_request_id = uuid::Uuid::new_v4().to_string();
    match tokio::time::timeout(
        REDEEM_REQUEST_TIMEOUT,
        client.consume_rate_limit_reset_credit(&redeem_request_id),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::info!(
                %profile_id,
                %error,
                "automatic reset-credit redemption failed; surfacing the original usage-limit error"
            );
            return None;
        }
        Err(_) => {
            tracing::warn!(%profile_id, "automatic reset-credit redemption timed out");
            return None;
        }
    }

    match pool.force_activate(&profile_id) {
        Ok(_) => {
            tracing::info!(%profile_id, "redeemed one rate-limit reset credit and reactivated the account");
            Some(ResetCreditRescue { profile_id })
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
