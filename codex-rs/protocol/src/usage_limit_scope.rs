//! Shared usage scope for recovery decisions and user-facing guidance.

use super::UsageLimitReachedError;
use crate::protocol::RateLimitReachedType;

impl UsageLimitReachedError {
    /// Model allocations do not exhaust ordinary Codex usage. Workspace restrictions and
    /// reserve fallbacks remain account-scoped even when the display name suggests a model.
    pub fn is_model_specific(&self) -> bool {
        let Some(snapshot) = self.rate_limits.as_ref() else {
            return false;
        };
        if self
            .rate_limit_reached_type
            .into_iter()
            .chain(snapshot.rate_limit_reached_type)
            .any(|kind| {
                matches!(
                    kind,
                    RateLimitReachedType::WorkspaceOwnerCreditsDepleted
                        | RateLimitReachedType::WorkspaceMemberCreditsDepleted
                        | RateLimitReachedType::WorkspaceOwnerUsageLimitReached
                        | RateLimitReachedType::WorkspaceMemberUsageLimitReached
                )
            })
        {
            return false;
        }
        let id = snapshot
            .limit_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let name = snapshot
            .limit_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        if id
            .into_iter()
            .chain(name)
            .any(|scope| scope.eq_ignore_ascii_case("gpt-reserve"))
        {
            return false;
        }
        id.or(name)
            .is_some_and(|scope| !scope.eq_ignore_ascii_case("codex"))
    }
}

#[cfg(test)]
#[path = "usage_limit_scope_tests.rs"]
mod tests;
