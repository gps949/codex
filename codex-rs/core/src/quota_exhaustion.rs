//! Compares advisory usage-limit metadata with the execution profile that sent the request.
//!
//! Request-bound authentication is the attribution authority. Plan metadata remains useful as a
//! defensive mismatch signal, but multiple users and workspaces can share the same plan family,
//! so it cannot prove which profile owns a rejection.

use codex_login::AccountLease;
use codex_protocol::account::PlanType;
use codex_protocol::error::UsageLimitReachedError;
use codex_protocol::protocol::RateLimitReachedType;

/// A retained window is fresh only according to its own observation time.
pub(crate) fn rate_limit_window_is_fresh(
    window: &codex_login::AccountRateLimitWindow,
    observed_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    observed_at
        .is_some_and(|observed| observed <= now && now - observed < chrono::Duration::minutes(30))
        && window.resets_at.is_none_or(|reset| reset > now)
}

/// Returns `false` when advisory plan metadata disagrees with the bound profile.
pub(crate) async fn usage_limit_metadata_matches_profile(
    lease: &AccountLease,
    limit: &UsageLimitReachedError,
) -> bool {
    let Some(auth) = lease.auth_manager().auth().await else {
        return true;
    };
    let profile_plan = auth.account_plan_type();

    if let Some(reached_type) = limit.rate_limit_reached_type
        && is_workspace_rate_limit(reached_type)
        && profile_plan.is_some_and(|plan| !plan.is_workspace_account())
    {
        return false;
    }

    if let Some(error_plan) = limit.plan_type.as_ref()
        && let Some(profile_plan) = profile_plan
    {
        let error_plan = PlanType::from(error_plan.clone());
        if !plans_share_quota_bucket(error_plan, profile_plan) {
            return false;
        }
    }

    if let Some(snapshot) = limit.rate_limits.as_ref()
        && let Some(error_plan) = snapshot.plan_type
        && let Some(profile_plan) = profile_plan
        && !plans_share_quota_bucket(error_plan, profile_plan)
    {
        return false;
    }

    true
}

fn is_workspace_rate_limit(reached_type: RateLimitReachedType) -> bool {
    matches!(
        reached_type,
        RateLimitReachedType::WorkspaceOwnerCreditsDepleted
            | RateLimitReachedType::WorkspaceMemberCreditsDepleted
            | RateLimitReachedType::WorkspaceOwnerUsageLimitReached
            | RateLimitReachedType::WorkspaceMemberUsageLimitReached
    )
}

/// A metered model bucket does not exhaust the account's ordinary Codex allowance.
/// Workspace credit limits and reserve fallbacks still belong to account recovery.
pub(crate) fn is_model_specific_usage_limit(limit: &UsageLimitReachedError) -> bool {
    limit.is_model_specific()
}

pub(crate) fn plans_share_quota_bucket(left: PlanType, right: PlanType) -> bool {
    if left == right {
        return true;
    }
    quota_bucket(left) == quota_bucket(right)
}

fn quota_bucket(plan: PlanType) -> &'static str {
    if plan.is_workspace_account() {
        "workspace"
    } else {
        "consumer"
    }
}

#[cfg(test)]
#[path = "quota_exhaustion_tests.rs"]
mod tests;
