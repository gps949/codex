use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_login::AccountProfileId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;

use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthLease;

/// Backend credit-depletion responses do not always include a reset timestamp. Such an account
/// must not become permanently unusable until process restart: credits may be replenished while
/// Codex remains open. A conservative periodic probe makes recovery automatic without hammering
/// the exhausted account on every request.
const UNKNOWN_QUOTA_RESET_REPROBE_DELAY: Duration = Duration::minutes(10);

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
        next_lease: ExecutionAuthLease,
    },
    /// Native routing recognized the failure, but every configured account is unavailable.
    PoolExhausted { cause: FailoverCause },
}

/// Coordinates account lifecycle changes without owning transport or turn replay semantics.
///
/// Current Codex `ModelClient` resolves provider/auth state on every request. The turn loop only
/// needs to capture the exact execution lease used by the failed request, rotate that lease in the
/// pool, then discard account-scoped *turn* transport state before retry/continuation. Keeping
/// transport reconstruction out of this type makes stale-worker generation handling reusable by
/// root turns, subagents, compaction and other inference surfaces.
pub(crate) struct FailoverCoordinator;

impl FailoverCoordinator {
    pub(crate) fn handle_inference_error(
        execution_auth: &ExecutionAuth,
        failed_lease: &ExecutionAuthLease,
        error: &CodexErr,
    ) -> std::io::Result<FailoverOutcome> {
        match error.details() {
            CodexErrorDetails::UsageLimitReached(limit) => {
                if let Some(rate_limits) = limit.rate_limits.as_deref() {
                    execution_auth.observe_rate_limits(failed_lease, rate_limits)?;
                }
                let reset_at = quota_reset_or_reprobe(limit.resets_at.as_ref());
                let next =
                    execution_auth.failover_after_quota_exhausted(failed_lease, Some(reset_at))?;
                Ok(Self::finish_rotation(
                    failed_lease,
                    next,
                    FailoverCause::UsageLimitReached,
                ))
            }
            CodexErrorDetails::RefreshTokenFailed(refresh_error) => {
                let next = execution_auth
                    .failover_after_auth_unavailable(failed_lease, refresh_error.to_string())?;
                Ok(Self::finish_rotation(
                    failed_lease,
                    next,
                    FailoverCause::AuthenticationUnavailable,
                ))
            }
            _ => Ok(FailoverOutcome::NotApplicable),
        }
    }

    fn finish_rotation(
        failed_lease: &ExecutionAuthLease,
        next: Option<ExecutionAuthLease>,
        cause: FailoverCause,
    ) -> FailoverOutcome {
        let Some(next_lease) = next else {
            return FailoverOutcome::PoolExhausted { cause };
        };

        let from_profile = failed_lease.profile_id().cloned();
        let from_generation = failed_lease.generation();
        let to_profile = next_lease.profile_id().cloned();
        let to_generation = next_lease.generation();

        FailoverOutcome::Rebound {
            cause,
            from_profile,
            from_generation,
            to_profile,
            to_generation,
            next_lease,
        }
    }
}

fn quota_reset_or_reprobe(resets_at: Option<&DateTime<Utc>>) -> DateTime<Utc> {
    resets_at
        .cloned()
        .unwrap_or_else(|| Utc::now() + UNKNOWN_QUOTA_RESET_REPROBE_DELAY)
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
    fn missing_reset_gets_finite_reprobe_deadline() {
        let before = Utc::now() + Duration::minutes(9);
        let reset = quota_reset_or_reprobe(None);
        let after = Utc::now() + Duration::minutes(11);
        assert!(reset > before);
        assert!(reset < after);
    }
}
