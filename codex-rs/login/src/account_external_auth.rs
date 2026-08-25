use std::io;
use std::sync::Arc;

use crate::AccountLease;
use crate::AccountPool;
use crate::AccountPoolError;
use crate::CodexAuth;
use crate::ExternalAuth;
use crate::ExternalAuthFuture;
use crate::ExternalAuthRefreshContext;
use crate::RefreshTokenError;

/// Presents the active native account-pool identity through Codex's existing `ExternalAuth`
/// abstraction.
///
/// This deliberately keeps the rest of Codex on the normal `AuthManager` path: model providers,
/// request auth, agent identity and unauthorized recovery continue to consume one shared
/// `AuthManager`, while this source resolves that manager's current execution identity from the
/// pool. Each profile still owns a normal managed `AuthManager`, including its independent OAuth
/// refresh lifecycle and credential store.
pub struct AccountPoolExternalAuth {
    pool: Arc<AccountPool>,
}

impl AccountPoolExternalAuth {
    pub fn new(pool: Arc<AccountPool>) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> Arc<AccountPool> {
        Arc::clone(&self.pool)
    }

    async fn resolve_usable_auth(&self) -> io::Result<(AccountLease, CodexAuth)> {
        // Every rejection below makes the current lease ineligible, so the number of registered
        // profiles is a hard upper bound on attempts and prevents malformed state from looping.
        let attempts = self.pool.snapshots().len().max(1);
        let mut last_reason = "no eligible account is available".to_string();

        for _ in 0..attempts {
            let lease = match self.pool.lease() {
                Ok(lease) => lease,
                Err(AccountPoolError::NoEligibleAccount) => {
                    return Err(io::Error::other(last_reason));
                }
                Err(error) => return Err(pool_error(error)),
            };
            let manager = lease.auth_manager();
            let Some(auth) = manager.auth().await else {
                last_reason = format!("account profile {} has no usable auth", lease.profile().id);
                let _ = self
                    .pool
                    .mark_authentication_unavailable(&lease, last_reason.clone());
                continue;
            };

            if !matches!(auth, CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_)) {
                last_reason = format!(
                    "account profile {} is not authenticated with ChatGPT",
                    lease.profile().id
                );
                let _ = self
                    .pool
                    .mark_authentication_unavailable(&lease, last_reason.clone());
                continue;
            }

            if let Some(failure) = manager.refresh_failure_for_auth(&auth) {
                last_reason = failure.to_string();
                let _ = self
                    .pool
                    .mark_authentication_unavailable(&lease, last_reason.clone());
                continue;
            }

            return Ok((lease, auth));
        }

        Err(io::Error::other(last_reason))
    }

    async fn refresh_active_auth(
        &self,
        context: ExternalAuthRefreshContext,
    ) -> io::Result<CodexAuth> {
        let (lease, auth) = self.resolve_usable_auth().await?;
        let current_account_id = auth.get_account_id();

        // A concurrent quota/auth failover may already have moved the pool from account A to B
        // while an older A request is reporting 401. In that case B is already the correct fresh
        // identity; never refresh or disable A based on stale work.
        if context.previous_account_id.is_some()
            && context.previous_account_id != current_account_id
        {
            return Ok(auth);
        }

        let manager = lease.auth_manager();
        let mut recovery = manager.unauthorized_recovery();
        while recovery.has_next() {
            match recovery.next().await {
                Ok(step) => {
                    let Some(current) = manager.auth().await else {
                        return self
                            .fail_over_unusable_lease(
                                &lease,
                                "account auth disappeared during unauthorized recovery".to_string(),
                            )
                            .await;
                    };
                    if step.auth_state_changed() == Some(true) {
                        return Ok(current);
                    }
                }
                Err(RefreshTokenError::Permanent(error)) => {
                    self.pool
                        .mark_authentication_unavailable(&lease, error.to_string())
                        .map_err(pool_error)?;
                    return match self.resolve_usable_auth().await {
                        Ok((_, replacement)) => Ok(replacement),
                        Err(_) => Err(io::Error::other(RefreshTokenError::Permanent(error))),
                    };
                }
                Err(error @ RefreshTokenError::Transient(_)) => {
                    return Err(io::Error::other(error));
                }
            }
        }

        manager.auth().await.ok_or_else(|| {
            io::Error::other("account unauthorized recovery completed without usable auth")
        })
    }

    async fn fail_over_unusable_lease(
        &self,
        lease: &AccountLease,
        reason: String,
    ) -> io::Result<CodexAuth> {
        self.pool
            .mark_authentication_unavailable(lease, reason)
            .map_err(pool_error)?;
        self.resolve_usable_auth().await.map(|(_, auth)| auth)
    }
}

impl ExternalAuth for AccountPoolExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async move { self.resolve_usable_auth().await.map(|(_, auth)| auth) })
    }

    fn refresh(&self, context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async move { self.refresh_active_auth(context).await })
    }

    fn classify_error(&self, error: io::Error) -> RefreshTokenError {
        // Per-profile unauthorized recovery preserves its native permanent/transient error inside
        // `io::Error::other`; recover that exact classification for the outer AuthManager.
        let message = error.to_string();
        if let Some(source) = error.into_inner()
            && let Ok(refresh_error) = source.downcast::<RefreshTokenError>()
        {
            return *refresh_error;
        }
        RefreshTokenError::Transient(io::Error::other(message))
    }
}

fn pool_error(error: AccountPoolError) -> io::Error {
    io::Error::other(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_errors_remain_io_errors_without_losing_message() {
        let error = pool_error(AccountPoolError::NoEligibleAccount);
        assert_eq!(error.to_string(), "no eligible account is available");
    }
}
