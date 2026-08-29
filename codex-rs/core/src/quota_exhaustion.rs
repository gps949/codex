//! Validates that a backend usage-limit rejection belongs to the execution profile that
//! actually sent the request.
//!
//! After account-pool rotation, a stale websocket or auth snapshot can still present the
//! previous account's credentials. The backend then returns that account's quota error while
//! the pool has already bound the turn to a different profile — incorrectly marking the new
//! profile exhausted with the old account's reset timestamp.

use codex_login::AccountLease;
use codex_protocol::account::PlanType;
use codex_protocol::error::UsageLimitReachedError;
use codex_protocol::protocol::RateLimitReachedType;

/// Returns `false` when the usage-limit payload likely reflects a different ChatGPT identity
/// than the profile bound to `lease`.
pub(crate) async fn usage_limit_matches_profile(
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
