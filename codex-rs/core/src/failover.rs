use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_login::AccountAvailabilityMutation;
use codex_login::AccountProfileId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;

use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthLease;
use crate::quota_exhaustion::usage_limit_metadata_matches_profile;

/// Backend credit-depletion responses do not always include a reset timestamp. Such an account
/// must not become permanently unusable until process restart: credits may be replenished while
/// Codex remains open. A conservative periodic probe makes recovery automatic without hammering
/// the exhausted account on every request.
const UNKNOWN_QUOTA_RESET_REPROBE_DELAY: Duration = Duration::minutes(10);

/// A backend reset timestamp at or before "now" would make the exhausted account immediately
/// eligible again, letting the sampling loop thrash on the same failing account. Every cooldown
/// therefore lasts at least this long.
const MINIMUM_QUOTA_COOLDOWN: Duration = Duration::seconds(60);

/// Why native execution-account failover was attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailoverCause {
    /// The backend authoritatively rejected this ChatGPT subscription for its usage window.
    UsageLimitReached,
    /// The profile's own AuthManager exhausted its normal refresh/recovery path.
    AuthenticationUnavailable,
}

/// Result of handling an inference error through the native execution-account pool.
#[derive(Clone, Debug)]
pub(crate) enum FailoverOutcome {
    /// The error is unrelated to execution-account availability and keeps normal Codex handling.
    NotApplicable,
    /// The failed lease was rotated to another account generation.
    Rebound {
        cause: FailoverCause,
        from_profile: Option<AccountProfileId>,
        from_generation: u64,
        to_profile: Option<AccountProfileId>,
        to_generation: u64,
    },
    /// The failed profile observation was recorded (or safely ignored as stale) while another
    /// account was already active. No identity switch occurred as part of this mutation.
    ActiveUnchanged {
        cause: FailoverCause,
        failed_profile: Option<AccountProfileId>,
        failed_generation: u64,
        active_profile: Option<AccountProfileId>,
        active_generation: u64,
    },
    /// Native routing recognized the failure, but every configured account is unavailable.
    PoolExhausted { cause: FailoverCause },
}

/// Coordinates account lifecycle changes without owning transport or turn replay semantics.
///
/// The turn loop captures the exact execution lease used by the failed request, rotates that lease
/// in the pool, then discards account-scoped *turn* transport state before retry/continuation.
/// Keeping transport reconstruction out of this type makes stale-worker generation handling
/// reusable by root turns, subagents, compaction and other inference surfaces.
pub(crate) struct FailoverCoordinator;

impl FailoverCoordinator {
    pub(crate) async fn handle_inference_error(
        execution_auth: &ExecutionAuth,
        failed_lease: &ExecutionAuthLease,
        error: &CodexErr,
    ) -> std::io::Result<FailoverOutcome> {
        match error.details() {
            CodexErrorDetails::UsageLimitReached(limit) => {
                if let Some(account_lease) = failed_lease.account_lease()
                    && !usage_limit_metadata_matches_profile(account_lease, limit).await
                {
                    tracing::warn!(
                        profile_id = %account_lease.profile().id,
                        error_plan_type = ?limit.plan_type,
                        rate_limit_reached_type = ?limit.rate_limit_reached_type,
                        "usage-limit metadata does not match the bound execution profile; attributing the rejection to the request-bound profile"
                    );
                }
                let reset_at = quota_reset_or_reprobe(limit.resets_at.as_ref());
                let mutation = execution_auth.failover_after_usage_limit(
                    failed_lease,
                    reset_at,
                    limit.rate_limits.as_deref(),
                )?;
                Ok(Self::finish_mutation(
                    failed_lease,
                    mutation,
                    FailoverCause::UsageLimitReached,
                ))
            }
            CodexErrorDetails::QuotaExceeded => {
                // SSE/API quota rejections carry no reset timestamp; park the account on the
                // conservative reprobe delay so recovery stays automatic.
                let reset_at = quota_reset_or_reprobe(/*resets_at*/ None);
                let mutation =
                    execution_auth.failover_after_quota_exhausted(failed_lease, Some(reset_at))?;
                Ok(Self::finish_mutation(
                    failed_lease,
                    mutation,
                    FailoverCause::UsageLimitReached,
                ))
            }
            CodexErrorDetails::RefreshTokenFailed(refresh_error) => {
                let mutation = execution_auth
                    .failover_after_auth_unavailable(failed_lease, refresh_error.to_string())?;
                Ok(Self::finish_mutation(
                    failed_lease,
                    mutation,
                    FailoverCause::AuthenticationUnavailable,
                ))
            }
            _ => Ok(FailoverOutcome::NotApplicable),
        }
    }

    fn finish_mutation(
        failed_lease: &ExecutionAuthLease,
        mutation: AccountAvailabilityMutation,
        cause: FailoverCause,
    ) -> FailoverOutcome {
        match mutation {
            AccountAvailabilityMutation::Rebound(next_lease) => FailoverOutcome::Rebound {
                cause,
                from_profile: failed_lease.profile_id().cloned(),
                from_generation: failed_lease.generation(),
                to_profile: Some(next_lease.profile().id.clone()),
                to_generation: next_lease.generation(),
            },
            AccountAvailabilityMutation::InactiveProfileUpdated {
                active: Some(active),
            }
            | AccountAvailabilityMutation::StaleIgnored {
                active: Some(active),
            } => FailoverOutcome::ActiveUnchanged {
                cause,
                failed_profile: failed_lease.profile_id().cloned(),
                failed_generation: failed_lease.generation(),
                active_profile: Some(active.profile().id.clone()),
                active_generation: active.generation(),
            },
            AccountAvailabilityMutation::PoolExhausted
            | AccountAvailabilityMutation::InactiveProfileUpdated { active: None }
            | AccountAvailabilityMutation::StaleIgnored { active: None } => {
                FailoverOutcome::PoolExhausted { cause }
            }
        }
    }
}

fn quota_reset_or_reprobe(resets_at: Option<&DateTime<Utc>>) -> DateTime<Utc> {
    let now = Utc::now();
    resets_at
        .cloned()
        .unwrap_or_else(|| now + UNKNOWN_QUOTA_RESET_REPROBE_DELAY)
        .max(now + MINIMUM_QUOTA_COOLDOWN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_reset_timestamp_wins_over_client_reprobe() {
        let reset = Utc::now() + Duration::hours(2);
        assert_eq!(quota_reset_or_reprobe(Some(&reset)), reset);
    }

    #[test]
    fn past_reset_timestamp_is_clamped_to_a_minimum_cooldown() {
        let stale_reset = Utc::now() - Duration::hours(1);
        let clamped = quota_reset_or_reprobe(Some(&stale_reset));
        assert!(clamped > Utc::now());
    }

    #[test]
    fn missing_reset_gets_finite_reprobe_deadline() {
        let before = Utc::now() + Duration::minutes(9);
        let reset = quota_reset_or_reprobe(None);
        let after = Utc::now() + Duration::minutes(11);
        assert!(reset > before);
        assert!(reset < after);
    }
}
