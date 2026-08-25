use codex_login::AccountProfileId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;

use crate::execution_auth::ExecutionAuthLease;
use crate::execution_model_client::ExecutionModelClient;
use crate::execution_model_client::ExecutionModelClientSession;

/// Why native execution-account failover was attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailoverCause {
    /// The backend authoritatively rejected this ChatGPT subscription for its usage window.
    UsageLimitReached,
    /// The profile's own AuthManager exhausted its normal refresh/recovery path.
    AuthenticationUnavailable,
}

/// Result of handling an inference error through the native execution-account pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FailoverOutcome {
    /// The error is unrelated to execution-account availability and keeps normal Codex handling.
    NotApplicable,
    /// The failed turn session was rebound to another account generation.
    Rebound {
        cause: FailoverCause,
        from_profile: Option<AccountProfileId>,
        from_generation: u64,
        to_profile: Option<AccountProfileId>,
        to_generation: u64,
    },
    /// Native routing recognized the failure, but every configured account is unavailable.
    PoolExhausted { cause: FailoverCause },
}

/// Coordinates account lifecycle changes with reconstruction of account-bound model transport.
///
/// This layer deliberately does not decide whether replaying the current sampling attempt is safe.
/// `SamplingAttemptCheckpoint` owns that decision because the turn loop alone knows whether output
/// or tool side effects escaped the failed request.
pub(crate) struct FailoverCoordinator;

impl FailoverCoordinator {
    pub(crate) fn handle_inference_error(
        model_client: &ExecutionModelClient,
        client_session: &mut ExecutionModelClientSession,
        error: &CodexErr,
    ) -> std::io::Result<FailoverOutcome> {
        match error.details() {
            CodexErrorDetails::UsageLimitReached(limit) => {
                let failed_lease = client_session.execution_lease().clone();
                if let Some(rate_limits) = limit.rate_limits.as_deref() {
                    model_client
                        .execution_auth()
                        .observe_rate_limits(&failed_lease, rate_limits)?;
                }
                let next = model_client
                    .execution_auth()
                    .failover_after_quota_exhausted(&failed_lease, limit.resets_at)?;
                Self::finish_rebind(
                    model_client,
                    client_session,
                    failed_lease,
                    next,
                    FailoverCause::UsageLimitReached,
                )
            }
            CodexErrorDetails::RefreshTokenFailed(refresh_error) => {
                let failed_lease = client_session.execution_lease().clone();
                let next = model_client
                    .execution_auth()
                    .failover_after_auth_unavailable(&failed_lease, refresh_error.to_string())?;
                Self::finish_rebind(
                    model_client,
                    client_session,
                    failed_lease,
                    next,
                    FailoverCause::AuthenticationUnavailable,
                )
            }
            _ => Ok(FailoverOutcome::NotApplicable),
        }
    }

    fn finish_rebind(
        model_client: &ExecutionModelClient,
        client_session: &mut ExecutionModelClientSession,
        failed_lease: ExecutionAuthLease,
        next: Option<ExecutionAuthLease>,
        cause: FailoverCause,
    ) -> std::io::Result<FailoverOutcome> {
        let Some(next) = next else {
            return Ok(FailoverOutcome::PoolExhausted { cause });
        };

        let from_profile = failed_lease.profile_id().cloned();
        let from_generation = failed_lease.generation();
        let to_profile = next.profile_id().cloned();
        let to_generation = next.generation();

        // Stale failures are safe: AccountPool returns the already-current lease instead of
        // advancing again, so this simply catches the old worker up to the selected identity.
        model_client.rebind_session(client_session, next)?;

        Ok(FailoverOutcome::Rebound {
            cause,
            from_profile,
            from_generation,
            to_profile,
            to_generation,
        })
    }
}
