use chrono::DateTime;
use chrono::Utc;
use codex_login::AccountAvailability;
use codex_protocol::error::CodexErr;
use codex_protocol::error::UsageLimitReachedError;

use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthLease;
use crate::failover::FailoverCause;
use crate::failover::FailoverCoordinator;
use crate::failover::FailoverOutcome;
use crate::failover_checkpoint::FailoverRetryMode;
use crate::failover_checkpoint::SamplingAttemptCheckpoint;

/// Turn-loop action after the native account layer has inspected an inference failure.
#[derive(Clone, Debug)]
pub(crate) enum SamplingFailoverDirective {
    /// Keep normal Codex error/retry handling; this is not an account-availability failure.
    NotHandled,
    /// Submit the exact same logical prompt using the newly selected execution account.
    ReplayCurrentSamplingRequest {
        transition: AccountFailoverTransition,
    },
    /// The failed response already produced durable conversation/tool state. Rebuild the prompt
    /// from local history and continue on the newly selected execution account.
    ContinueFromDurableHistory {
        transition: AccountFailoverTransition,
    },
    /// The account pool recognized the failure but every configured account is unavailable.
    PoolExhausted {
        retry_mode: FailoverRetryMode,
        cause: FailoverCause,
    },
    /// A tool may have caused an external side effect without a durable result, or partial visible
    /// model output must first be reconciled. The pool may already have moved to another account,
    /// but automatic replay is intentionally blocked.
    ReconcileCurrentAttempt {
        transition: AccountFailoverTransition,
    },
}

/// Whether this failure mutation selected the active account or found it already selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccountFailoverTransition {
    Rebound,
    ActiveUnchanged,
}

/// Converts one model failure plus its sampling checkpoint into a turn-loop continuation policy.
///
/// The coordinator owns execution-identity switching. This adapter owns replay semantics, keeping
/// scheduling independent from the agent loop so the same failover machinery can serve subagents,
/// review workers and other inference surfaces.
pub(crate) async fn handle_sampling_failover(
    execution_auth: &ExecutionAuth,
    failed_lease: &ExecutionAuthLease,
    checkpoint: SamplingAttemptCheckpoint,
    error: &CodexErr,
) -> std::io::Result<SamplingFailoverDirective> {
    match FailoverCoordinator::handle_inference_error(execution_auth, failed_lease, error).await? {
        FailoverOutcome::NotApplicable => Ok(SamplingFailoverDirective::NotHandled),
        FailoverOutcome::PoolExhausted { cause } => {
            tracing::warn!(
                ?cause,
                "every configured Codex execution account is unavailable"
            );
            Ok(SamplingFailoverDirective::PoolExhausted {
                retry_mode: checkpoint.retry_mode(),
                cause,
            })
        }
        FailoverOutcome::Rebound {
            cause,
            from_profile,
            from_generation,
            to_profile,
            to_generation,
        } => {
            tracing::info!(
                ?cause,
                ?from_profile,
                from_generation,
                ?to_profile,
                to_generation,
                "rotated to another Codex execution account"
            );
            Ok(directive_for_retry_mode(
                checkpoint.retry_mode(),
                AccountFailoverTransition::Rebound,
            ))
        }
        FailoverOutcome::ActiveUnchanged {
            cause,
            failed_profile,
            failed_generation,
            active_profile,
            active_generation,
        } => {
            tracing::info!(
                ?cause,
                ?failed_profile,
                failed_generation,
                ?active_profile,
                active_generation,
                "attributed a late execution failure without changing the active Codex account"
            );
            Ok(directive_for_retry_mode(
                checkpoint.retry_mode(),
                AccountFailoverTransition::ActiveUnchanged,
            ))
        }
    }
}

fn directive_for_retry_mode(
    retry_mode: FailoverRetryMode,
    transition: AccountFailoverTransition,
) -> SamplingFailoverDirective {
    match retry_mode {
        FailoverRetryMode::ReplayCurrentSamplingRequest => {
            SamplingFailoverDirective::ReplayCurrentSamplingRequest { transition }
        }
        // Until the UI/protocol layer has an explicit abandoned-partial-output lifecycle, never
        // duplicate visible assistant/reasoning text just to make switching look seamless.
        FailoverRetryMode::ReplayAfterAbandoningPartialOutput
        | FailoverRetryMode::ReconcileCurrentAttempt => {
            SamplingFailoverDirective::ReconcileCurrentAttempt { transition }
        }
        FailoverRetryMode::ContinueFromDurableHistory => {
            SamplingFailoverDirective::ContinueFromDurableHistory { transition }
        }
    }
}

/// How the turn proceeds after an automatic account switch, for user-facing messaging.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AccountSwitchContinuation {
    /// The turn continues automatically on the newly selected account.
    Automatic,
    /// Visible partial output blocked automatic replay; the user must re-send their message.
    ResendRequired,
}

/// Builds the user-facing message for a completed account rotation, naming the account that took
/// over. Returns `None` when no account is schedulable; the pool-exhausted message covers that.
pub(crate) fn account_switch_message(
    execution_auth: &ExecutionAuth,
    failed_lease: &ExecutionAuthLease,
    continuation: AccountSwitchContinuation,
) -> Option<String> {
    let active = execution_auth.active_lease()?;
    let to_label = active.notice_label()?;
    let from = failed_lease
        .notice_label()
        .unwrap_or_else(|| "the previous account".to_string());
    let suffix = match continuation {
        AccountSwitchContinuation::Automatic => "The turn continues automatically.",
        AccountSwitchContinuation::ResendRequired => {
            "Partial output could not be replayed safely; re-send your message to continue on the new account."
        }
    };
    Some(format!(
        "Codex account {from} became unavailable; switched to {to_label}. {suffix}"
    ))
}

/// Earliest known cooldown or active-window reset across exhausted enabled pool profiles.
pub(crate) fn earliest_exhausted_reset(execution_auth: &ExecutionAuth) -> Option<DateTime<Utc>> {
    execution_auth
        .account_pool()
        .and_then(|pool| earliest_exhausted_reset_in_pool(&pool))
}

pub(crate) fn earliest_exhausted_reset_in_pool(
    pool: &codex_login::AccountPool,
) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    pool.snapshots()
        .into_iter()
        .filter(|snapshot| !snapshot.profile.disabled)
        .filter_map(|snapshot| match snapshot.availability {
            AccountAvailability::Exhausted { resets_at } => resets_at
                .into_iter()
                .chain(snapshot.backend_resets_at.filter(|reset| *reset > now))
                .chain(
                    snapshot
                        .rate_limits
                        .primary
                        .iter()
                        .chain(snapshot.rate_limits.secondary.iter())
                        .filter_map(|window| window.resets_at)
                        .filter(|reset| *reset > now),
                )
                .min(),
            AccountAvailability::Available
            | AccountAvailability::AuthenticationUnavailable { .. }
            | AccountAvailability::Disabled => None,
        })
        .min()
}

pub(crate) fn preemptive_switch_message(
    switch: &crate::execution_auth::PreemptiveSwitch,
) -> String {
    format!(
        "Switched to Codex account {}: account {} reached {:.0}% of a usage window (resets at {}).",
        switch.to_label,
        switch.from_label,
        switch.used_percent,
        codex_login::format_exhausted_reset(switch.resets_at),
    )
}

pub(crate) fn recovery_continuation_message(execution_auth: &ExecutionAuth) -> String {
    match execution_auth
        .account_pool()
        .and_then(|pool| pool.lease().ok().map(|lease| lease.notice_label()))
    {
        Some(label) => {
            format!("Codex account {label} became available; the turn continues automatically.")
        }
        None => "A Codex account became available for retry. The turn continues automatically."
            .to_string(),
    }
}

pub(crate) fn profile_notice_label_in_pool(
    pool: Option<&codex_login::AccountPool>,
    profile_id: &codex_login::AccountProfileId,
) -> String {
    pool.and_then(|pool| {
        let snapshot = pool
            .snapshots()
            .into_iter()
            .find(|snapshot| &snapshot.profile.id == profile_id)?;
        let email = pool
            .auth_managers()
            .into_iter()
            .find(|(id, _)| id == profile_id)
            .and_then(|(_, manager)| manager.auth_cached())
            .and_then(|auth| auth.get_account_email());
        Some(codex_login::format_account_notice_label(
            snapshot.profile.label.as_deref(),
            email.as_deref(),
            profile_id.as_str(),
        ))
    })
    .unwrap_or_else(|| format!("`{profile_id}`"))
}

pub(crate) fn reset_credit_rescue_message(
    execution_auth: &ExecutionAuth,
    redeemed_profile_id: Option<&codex_login::AccountProfileId>,
    active_profile_id: &codex_login::AccountProfileId,
    safe_to_continue: bool,
) -> String {
    let pool = execution_auth.account_pool();
    pool_reset_credit_rescue_message(
        pool.as_deref(),
        redeemed_profile_id,
        active_profile_id,
        safe_to_continue,
    )
}

pub(crate) fn pool_reset_credit_rescue_message(
    pool: Option<&codex_login::AccountPool>,
    redeemed_profile_id: Option<&codex_login::AccountProfileId>,
    active_profile_id: &codex_login::AccountProfileId,
    safe_to_continue: bool,
) -> String {
    let active_label = profile_notice_label_in_pool(pool, active_profile_id);
    let prefix = match redeemed_profile_id {
        Some(redeemed_id) => {
            let redeemed_label = profile_notice_label_in_pool(pool, redeemed_id);
            format!(
                "Redeemed one rate-limit reset credit on Codex account {redeemed_label}. Execution account: {active_label}."
            )
        }
        None => format!("Codex account recovered: {active_label}."),
    };
    let suffix = if safe_to_continue {
        "The turn continues on that account."
    } else {
        "Partial output or an unfinished tool result requires reconciliation; re-send your message to continue."
    };
    format!("{prefix} {suffix}")
}

/// Projects the active pooled account's cached rate-limit windows into a clean session snapshot so
/// UI surfaces do not remain stuck on the previous account's exhausted percentages after switching.
pub(crate) fn active_account_rate_limit_snapshot(
    execution_auth: &ExecutionAuth,
) -> Option<codex_protocol::protocol::RateLimitSnapshot> {
    let pool = execution_auth.account_pool()?;
    active_pool_rate_limit_snapshot(&pool)
}

pub(crate) fn active_pool_rate_limit_snapshot(
    pool: &codex_login::AccountPool,
) -> Option<codex_protocol::protocol::RateLimitSnapshot> {
    let account_lease = pool.lease().ok()?;
    let snapshot = pool
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.profile.id == account_lease.profile().id)?;
    let convert_window =
        |window: &codex_login::AccountRateLimitWindow| codex_protocol::protocol::RateLimitWindow {
            used_percent: window.used_percent,
            resets_at: window.resets_at.map(|reset| reset.timestamp()),
            window_minutes: window.window_minutes,
        };
    let primary = snapshot.rate_limits.primary.as_ref().map(convert_window);
    let secondary = snapshot.rate_limits.secondary.as_ref().map(convert_window);
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    let plan_type = account_lease
        .auth_manager()
        .auth_cached()
        .and_then(|auth| auth.account_plan_type());
    Some(codex_protocol::protocol::RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        normal_model_slug: None,
        primary,
        secondary,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type,
        rate_limit_reached_type: None,
    })
}

pub(crate) async fn sync_session_rate_limits_with_active_account(
    sess: &crate::session::session::Session,
    turn_context: &crate::session::turn_context::TurnContext,
    execution_auth: &ExecutionAuth,
) {
    sess.replace_rate_limits(
        turn_context,
        active_account_rate_limit_snapshot(execution_auth),
    )
    .await;
}

/// Builds the user-facing message shown when every configured account is unavailable.
pub(crate) fn pool_exhausted_message(execution_auth: &ExecutionAuth) -> String {
    match earliest_exhausted_reset(execution_auth) {
        Some(resets_at) => format!(
            "No configured Codex account is currently available. The earliest quota cooldown ends at {}. If a plan has earned rate-limit reset credits, redeeming one (for example from /status in the TUI or the app) unblocks that account immediately.",
            resets_at.format("%Y-%m-%d %H:%M UTC")
        ),
        None => "All configured Codex accounts are currently unavailable. Run `codex account list` for details.".to_string(),
    }
}

/// Error returned when a new sampling request cannot obtain any pool lease.
///
/// Must be [`CodexErr::UsageLimitReached`] (not UnsupportedOperation) so clients map it to
/// `UsageLimitExceeded` and can show the normal cooldown UI instead of a hard BadRequest.
pub(crate) fn pool_unavailable_error(execution_auth: &ExecutionAuth) -> CodexErr {
    CodexErr::UsageLimitReached(UsageLimitReachedError {
        plan_type: None,
        resets_at: earliest_exhausted_reset(execution_auth),
        limit_window_minutes: None,
        rate_limits: None,
        promo_message: None,
        rate_limit_reached_type: None,
    })
}

#[cfg(test)]
#[path = "failover_turn_tests.rs"]
mod tests;
