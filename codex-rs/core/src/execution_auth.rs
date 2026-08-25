use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::config::Config;
use chrono::DateTime;
use chrono::Utc;
use codex_login::AccountLease;
use codex_login::AccountPool;
use codex_login::AccountPoolRuntime;
use codex_login::AccountPoolRuntimeError;
use codex_login::AccountProfileId;
use codex_login::AccountRateLimitWindow;
use codex_login::AccountRateLimits;
use codex_login::AuthManager;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use tokio::sync::OnceCell;
use tokio::sync::watch;

/// Process-local registry that preserves one execution-auth coordinator for every live AuthManager.
///
/// This deliberately keys by Arc allocation identity rather than account id: before pooled auth is
/// installed an AuthManager may not have a resolved ChatGPT account yet, and callers that supply a
/// distinct AuthManager must keep their auth lifecycle isolated. The registry intentionally owns
/// a strong process-lifetime reference: the coordinator in turn owns the AuthManager, so pointer
/// identity cannot be recycled while the process is alive and scheduler state survives between turns.
static EXECUTION_AUTH_REGISTRY: OnceLock<StdMutex<HashMap<usize, Arc<ExecutionAuth>>>> =
    OnceLock::new();

/// Process/session-facing authentication facade for inference execution.
///
/// The logical Codex thread, app-server and local state stay account-independent. Every concrete
/// model client/request instead captures an immutable [`ExecutionAuthLease`]. Switching accounts
/// never mutates the AuthManager underneath an already account-bound ModelClient.
///
/// Construction is synchronous so existing ThreadManager creation sites do not need to become
/// async. Native account pooling is installed lazily from the resolved Config when the first
/// session starts. Sessions that already share the same AuthManager automatically share this
/// coordinator through [`Self::shared`], so root threads and subagents do not need another plumbing
/// field just for multi-account execution.
pub(crate) struct ExecutionAuth {
    legacy_manager: Arc<AuthManager>,
    runtime: OnceCell<Arc<AccountPoolRuntime>>,
    change_tx: watch::Sender<u64>,
}

/// Private outcome of one lazy pool-install attempt. `NotConfigured` keeps the cell empty so a
/// manifest created later (for example by `codex account add`) is picked up without a restart.
enum RuntimeInitError {
    NotConfigured,
    Failed(AccountPoolRuntimeError),
}

/// Immutable execution identity captured by one account-bound model client/request path.
#[derive(Clone)]
pub(crate) struct ExecutionAuthLease {
    account: Option<AccountLease>,
}

/// A pre-emptive rotation lacking an authoritative reset timestamp re-probes the parked account
/// after this delay, mirroring the hard-failure reprobe policy in the failover coordinator.
const PREEMPTIVE_UNKNOWN_RESET_REPROBE_DELAY: chrono::Duration = chrono::Duration::minutes(10);

/// Describes one completed pre-emptive account rotation for logging and user notification.
#[derive(Clone, Debug)]
pub(crate) struct PreemptiveSwitch {
    pub(crate) from_profile: AccountProfileId,
    pub(crate) to_profile: AccountProfileId,
    pub(crate) used_percent: f64,
    pub(crate) resets_at: DateTime<Utc>,
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
    /// Returns the process-local execution coordinator associated with this exact AuthManager.
    ///
    /// Existing Codex ownership already propagates an AuthManager from a root session to its
    /// descendants. Reusing that identity here therefore gives the whole thread tree one account
    /// pool without changing ThreadManager/SessionSpawnArgs APIs. A caller that intentionally uses
    /// a different AuthManager receives an independent coordinator and keeps legacy behavior.
    pub(crate) fn shared(legacy_manager: Arc<AuthManager>) -> Arc<Self> {
        let key = Arc::as_ptr(&legacy_manager) as usize;
        let registry = EXECUTION_AUTH_REGISTRY.get_or_init(|| StdMutex::new(HashMap::new()));
        let mut registry = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(existing) = registry.get(&key) {
            return Arc::clone(existing);
        }

        let coordinator = Arc::new(Self::legacy(legacy_manager));
        registry.insert(key, Arc::clone(&coordinator));
        coordinator
    }

    pub(crate) fn legacy(legacy_manager: Arc<AuthManager>) -> Self {
        let (change_tx, _change_rx) = watch::channel(0);
        Self {
            legacy_manager,
            runtime: OnceCell::new(),
            change_tx,
        }
    }

    /// Installs native account pooling once a profile manifest exists. Repeated calls are cheap and
    /// safe, and a process that started in stock single-account mode can enable pooling later after
    /// `account add` creates the manifest without requiring a restart.
    pub(crate) async fn ensure_runtime_from_config(
        &self,
        config: &Config,
    ) -> Result<bool, AccountPoolRuntimeError> {
        if let Some(runtime) = self.runtime() {
            runtime
                .pool()
                .set_return_to_preferred(config.account_pool.effective_return_to_preferred());
            return Ok(true);
        }

        // OnceCell serializes concurrent initializers without holding a guard across await.
        let newly_installed = AtomicBool::new(false);
        let result = self
            .runtime
            .get_or_try_init(|| async {
                match AccountPoolRuntime::try_install_from_config(
                    Arc::clone(&self.legacy_manager),
                    config,
                    /*include_existing_root_login*/ true,
                )
                .await
                {
                    Ok(Some(runtime)) => {
                        newly_installed.store(true, Ordering::Release);
                        Ok(Arc::new(runtime))
                    }
                    Ok(None) => Err(RuntimeInitError::NotConfigured),
                    Err(error) => Err(RuntimeInitError::Failed(error)),
                }
            })
            .await;

        match result {
            Ok(runtime) => {
                let pool = runtime.pool();
                pool.set_return_to_preferred(config.account_pool.effective_return_to_preferred());
                if newly_installed.load(Ordering::Acquire) {
                    self.notify_change();
                    self.spawn_pool_change_bridge(pool);
                }
                Ok(true)
            }
            Err(RuntimeInitError::NotConfigured) => Ok(false),
            Err(RuntimeInitError::Failed(error)) => Err(error),
        }
    }

    pub(crate) fn runtime(&self) -> Option<Arc<AccountPoolRuntime>> {
        self.runtime.get().cloned()
    }

    pub(crate) fn account_pool(&self) -> Option<Arc<AccountPool>> {
        self.runtime().map(|runtime| runtime.pool())
    }

    /// Whether this logical session can move between more than one configured execution identity.
    pub(crate) fn multi_account_enabled(&self) -> bool {
        self.account_pool()
            .is_some_and(|pool| pool.snapshots().len() > 1)
    }

    /// Stable profile that owns rollout items created by stock Codex before execution provenance
    /// existed. The root profile remains meaningful even after another profile becomes active.
    pub(crate) fn legacy_unattributed_profile_id(&self) -> Option<AccountProfileId> {
        self.account_pool()?
            .snapshots()
            .into_iter()
            .find_map(|snapshot| {
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
            Some(pool) => pool
                .lease()
                .ok()
                .map(ExecutionAuthLease::from_account_lease),
            None => Some(ExecutionAuthLease::legacy()),
        }
    }

    /// Compatibility manager for account-aware side systems that have not yet moved to leases.
    /// In pooled mode the AccountPoolRuntime keeps this outer manager synchronized with the active
    /// account; inference itself must use [`Self::active_lease`] instead.
    pub(crate) fn compatibility_auth_manager(&self) -> Arc<AuthManager> {
        self.runtime()
            .map(|runtime| runtime.auth_manager())
            .unwrap_or_else(|| Arc::clone(&self.legacy_manager))
    }

    /// Unified change stream for future app-server/TUI status surfaces. It remains valid across the
    /// transition from legacy auth to pooled auth because `ExecutionAuth` owns the channel itself.
    pub(crate) fn active_auth_change_receiver(&self) -> watch::Receiver<u64> {
        self.change_tx.subscribe()
    }

    /// Associates a backend rate-limit snapshot with the exact lease that observed it. Cached
    /// snapshots are advisory; a real UsageLimitReached response remains authoritative.
    /// Rotates away from the active account before it hits a hard usage limit.
    ///
    /// The decision uses the pool's cached per-account rate-limit windows, which are refreshed on
    /// every successful response. The rotation is skipped when no window has crossed `threshold`,
    /// when the data is stale or already reset, or when no other account could take over.
    pub(crate) fn preemptive_rotation(&self, threshold: f64) -> Option<PreemptiveSwitch> {
        const STALE_OBSERVATION_CUTOFF: chrono::Duration = chrono::Duration::minutes(30);

        let pool = self.account_pool()?;
        let lease = pool.lease().ok()?;
        let snapshot = pool
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.profile.id == lease.profile().id)?;

        let now = Utc::now();
        let observed_recently = snapshot
            .rate_limits
            .observed_at
            .is_some_and(|observed_at| now - observed_at < STALE_OBSERVATION_CUTOFF);
        let depleted_window = [
            snapshot.rate_limits.primary.as_ref(),
            snapshot.rate_limits.secondary.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter(|window| window.used_percent >= threshold)
        .filter(|window| match window.resets_at {
            // A window whose reset already passed no longer constrains the account.
            Some(resets_at) => resets_at > now,
            None => observed_recently,
        })
        .max_by(|left, right| left.used_percent.total_cmp(&right.used_percent))?;

        let used_percent = depleted_window.used_percent;
        let resets_at = depleted_window
            .resets_at
            .unwrap_or_else(|| now + PREEMPTIVE_UNKNOWN_RESET_REPROBE_DELAY);
        let next = pool.rotate_preemptively(&lease, resets_at)?;
        Some(PreemptiveSwitch {
            from_profile: lease.profile().id.clone(),
            to_profile: next.profile().id.clone(),
            used_percent,
            resets_at,
        })
    }

    pub(crate) fn observe_rate_limits(
        &self,
        lease: &ExecutionAuthLease,
        snapshot: &RateLimitSnapshot,
    ) -> std::io::Result<()> {
        let (Some(pool), Some(account_lease)) = (self.account_pool(), lease.account.as_ref())
        else {
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
        let (Some(pool), Some(account_lease)) =
            (self.account_pool(), failed_lease.account.as_ref())
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
        let (Some(pool), Some(account_lease)) =
            (self.account_pool(), failed_lease.account.as_ref())
        else {
            return Ok(None);
        };
        pool.mark_authentication_unavailable(account_lease, message)
            .map(|next| next.map(ExecutionAuthLease::from_account_lease))
            .map_err(std::io::Error::other)
    }

    fn spawn_pool_change_bridge(&self, pool: Arc<AccountPool>) {
        let mut changes = pool.change_receiver();
        let change_tx = self.change_tx.clone();
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                change_tx.send_modify(|revision| *revision = revision.wrapping_add(1));
            }
        });
    }

    fn notify_change(&self) {
        self.change_tx
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

impl ExecutionAuthLease {
    fn legacy() -> Self {
        Self { account: None }
    }

    fn from_account_lease(account: AccountLease) -> Self {
        Self {
            account: Some(account),
        }
    }

    pub(crate) fn profile_id(&self) -> Option<&AccountProfileId> {
        self.account.as_ref().map(|lease| &lease.profile().id)
    }

    pub(crate) fn generation(&self) -> u64 {
        self.account.as_ref().map_or(0, AccountLease::generation)
    }

    pub(crate) fn account_lease(&self) -> Option<&AccountLease> {
        self.account.as_ref()
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
