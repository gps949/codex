use std::sync::Arc;

use chrono::DateTime;
use chrono::Utc;
use codex_login::AccountLease;
use codex_login::AccountPool;
use codex_login::AccountPoolRuntime;
use codex_login::AccountProfileId;
use codex_login::AccountRateLimitWindow;
use codex_login::AccountRateLimits;
use codex_login::AuthManager;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use tokio::sync::watch;

/// Process/session-facing authentication facade for inference execution.
///
/// The logical Codex thread, app-server and local state stay account-independent. Every concrete
/// model client/request instead captures an immutable [`ExecutionAuthLease`]. Switching accounts
/// never mutates the AuthManager underneath an already account-bound ModelClient.
pub(crate) struct ExecutionAuth {
    legacy_manager: Arc<AuthManager>,
    runtime: Option<AccountPoolRuntime>,
}

/// Immutable execution identity captured by one account-bound model client/request path.
#[derive(Clone)]
pub(crate) struct ExecutionAuthLease {
    account: Option<AccountLease>,
    auth_manager: Arc<AuthManager>,
}

impl std::fmt::Debug for ExecutionAuthLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionAuthLease")
            .field("profile_id", &self.profile_id())
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

impl ExecutionAuth {
    pub(crate) fn legacy(legacy_manager: Arc<AuthManager>) -> Self {
        Self {
            legacy_manager,
            runtime: None,
        }
    }

    pub(crate) fn with_runtime(
        legacy_manager: Arc<AuthManager>,
        runtime: AccountPoolRuntime,
    ) -> Self {
        Self {
            legacy_manager,
            runtime: Some(runtime),
        }
    }

    pub(crate) fn runtime(&self) -> Option<&AccountPoolRuntime> {
        self.runtime.as_ref()
    }

    pub(crate) fn account_pool(&self) -> Option<Arc<AccountPool>> {
        self.runtime.as_ref().map(AccountPoolRuntime::pool)
    }

    /// Whether this logical session can move between more than one configured execution identity.
    pub(crate) fn multi_account_enabled(&self) -> bool {
        self.account_pool()
            .is_some_and(|pool| pool.snapshots().len() > 1)
    }

    /// Stable profile that owns rollout items created by stock Codex before execution provenance
    /// existed. The root profile remains meaningful even after another profile becomes active.
    pub(crate) fn legacy_unattributed_profile_id(&self) -> Option<AccountProfileId> {
        self.account_pool()?.snapshots().into_iter().find_map(|snapshot| {
            (snapshot.profile.id.as_str() == "legacy-root").then_some(snapshot.profile.id)
        })
    }

    /// Captures the identity for a new account-bound model client/request.
    ///
    /// If a pool exists but all profiles are unavailable, this intentionally returns `None` rather
    /// than silently falling back to root credentials, because root may itself be the exhausted
    /// profile currently cooling down.
    pub(crate) fn active_lease(&self) -> Option<ExecutionAuthLease> {
        match self.account_pool() {
            Some(pool) => pool.lease().ok().map(ExecutionAuthLease::from_account_lease),
            None => Some(ExecutionAuthLease::legacy(Arc::clone(&self.legacy_manager))),
        }
    }

    /// Compatibility manager for account-aware side systems that have not yet moved to leases.
    /// In pooled mode the AccountPoolRuntime keeps this outer manager synchronized with the active
    /// account; inference itself must use [`Self::active_lease`] instead.
    pub(crate) fn compatibility_auth_manager(&self) -> Arc<AuthManager> {
        self.runtime
            .as_ref()
            .map(AccountPoolRuntime::auth_manager)
            .unwrap_or_else(|| Arc::clone(&self.legacy_manager))
    }

    /// Unified change stream for MCP/plugins/models/UI surfaces that should refresh after an
    /// account switch, cooldown/reset, or active credential refresh.
    pub(crate) fn active_auth_change_receiver(&self) -> watch::Receiver<u64> {
        match self.account_pool() {
            Some(pool) => pool.change_receiver(),
            None => self.legacy_manager.auth_change_receiver(),
        }
    }

    /// Associates a backend rate-limit snapshot with the exact lease that observed it. Cached
    /// snapshots are advisory; a real UsageLimitReached response remains authoritative.
    pub(crate) fn observe_rate_limits(
        &self,
        lease: &ExecutionAuthLease,
        snapshot: &RateLimitSnapshot,
    ) -> std::io::Result<()> {
        let (Some(pool), Some(account_lease)) = (self.account_pool(), lease.account.as_ref()) else {
            return Ok(());
        };
        pool.update_rate_limits(
            &account_lease.profile().id,
            AccountRateLimits {
                primary: snapshot.primary.as_ref().map(convert_rate_limit_window),
                secondary: snapshot.secondary.as_ref().map(convert_rate_limit_window),
                observed_at: Some(Utc::now()),
            },
        )
        .map_err(std::io::Error::other)
    }

    /// Marks only the lease that actually received the quota rejection as exhausted. Generation
    /// checks inside AccountPool make late failures from old workers harmless.
    pub(crate) fn failover_after_quota_exhausted(
        &self,
        failed_lease: &ExecutionAuthLease,
        resets_at: Option<DateTime<Utc>>,
    ) -> std::io::Result<Option<ExecutionAuthLease>> {
        let (Some(pool), Some(account_lease)) = (self.account_pool(), failed_lease.account.as_ref())
        else {
            return Ok(None);
        };
        pool.mark_exhausted(account_lease, resets_at)
            .map(|next| next.map(ExecutionAuthLease::from_account_lease))
            .map_err(std::io::Error::other)
    }

    pub(crate) fn failover_after_auth_unavailable(
        &self,
        failed_lease: &ExecutionAuthLease,
        message: impl Into<String>,
    ) -> std::io::Result<Option<ExecutionAuthLease>> {
        let (Some(pool), Some(account_lease)) = (self.account_pool(), failed_lease.account.as_ref())
        else {
            return Ok(None);
        };
        pool.mark_authentication_unavailable(account_lease, message)
            .map(|next| next.map(ExecutionAuthLease::from_account_lease))
            .map_err(std::io::Error::other)
    }
}

impl ExecutionAuthLease {
    fn legacy(auth_manager: Arc<AuthManager>) -> Self {
        Self {
            account: None,
            auth_manager,
        }
    }

    fn from_account_lease(account: AccountLease) -> Self {
        Self {
            auth_manager: account.auth_manager(),
            account: Some(account),
        }
    }

    pub(crate) fn profile_id(&self) -> Option<&AccountProfileId> {
        self.account.as_ref().map(|lease| &lease.profile().id)
    }

    pub(crate) fn generation(&self) -> u64 {
        self.account.as_ref().map_or(0, AccountLease::generation)
    }

    pub(crate) fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.auth_manager)
    }

    pub(crate) fn is_same_execution_identity(&self, other: &Self) -> bool {
        match (&self.account, &other.account) {
            (Some(left), Some(right)) => {
                left.profile().id == right.profile().id && left.generation() == right.generation()
            }
            (None, None) => Arc::ptr_eq(&self.auth_manager, &other.auth_manager),
            _ => false,
        }
    }
}

fn convert_rate_limit_window(window: &RateLimitWindow) -> AccountRateLimitWindow {
    AccountRateLimitWindow {
        used_percent: window.used_percent,
        resets_at: window
            .resets_at
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0)),
    }
}
