use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use chrono::Utc;
use thiserror::Error;
use tokio::task::JoinHandle;

use crate::AccountPool;
use crate::AccountPoolError;
use crate::AccountPoolExternalAuth;
use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AccountProfileStore;
use crate::AccountProfileStoreError;
use crate::AccountRuntimeState;
use crate::AccountRuntimeStateStore;
use crate::AuthConfig;
use crate::AuthManager;
use crate::AuthManagerConfig;
use crate::AuthManagerInitializationError;
use crate::CodexAuth;
use crate::RefreshTokenError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountPoolRuntimeProfileIssue {
    pub profile: AccountProfile,
    pub reason: String,
}

/// Installed native multi-account runtime for one Codex process.
///
/// The runtime owns only execution-auth orchestration. The root `CODEX_HOME`, thread history,
/// configuration, state DBs and app-server lifecycle remain owned by their existing Codex
/// components.
pub struct AccountPoolRuntime {
    pool: Arc<AccountPool>,
    store: AccountProfileStore,
    runtime_state_store: AccountRuntimeStateStore,
    auth_config: AuthConfig,
    outer_auth_manager: Arc<AuthManager>,
    profile_issues: Vec<AccountPoolRuntimeProfileIssue>,
    runtime_state_issue: Option<String>,
    auth_sync_task: JoinHandle<()>,
    keepalive_task: JoinHandle<()>,
    suspended: Arc<AtomicBool>,
    lifecycle_lock: Arc<tokio::sync::Mutex<()>>,
}

/// ChatGPT refresh tokens can expire when a profile stays idle for weeks. Touching every
/// profile's AuthManager on this cadence lets its own 8-day proactive-refresh policy keep
/// stand-by credentials alive long before they are needed.
const AUTH_KEEPALIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);
const SUSPENDED_MARKER: &str = ".account-pool-suspended";

impl AccountPoolRuntime {
    /// Installs native account pooling only when the user has already configured the account
    /// profile manifest. Stock/single-account Codex therefore keeps its original auth behavior and
    /// this probe does not create any files as a side effect.
    pub async fn try_install_from_config(
        outer_auth_manager: Arc<AuthManager>,
        config: &impl AuthManagerConfig,
        include_existing_root_login: bool,
    ) -> Result<Option<Self>, AccountPoolRuntimeError> {
        let store = AccountProfileStore::new(config.codex_home());
        if !store.manifest_path().is_file() || Self::is_home_suspended(&config.codex_home()) {
            return Ok(None);
        }

        let auth_config = AuthConfig {
            codex_home: config.codex_home(),
            auth_credentials_store_mode: config.cli_auth_credentials_store_mode(),
            keyring_backend_kind: config.auth_keyring_backend_kind(),
            forced_login_method: config.forced_login_method(),
            chatgpt_base_url: Some(config.chatgpt_base_url()),
            forced_chatgpt_workspace_id: config.forced_chatgpt_workspace_id(),
            managed_auth_policy: config.managed_auth_policy(),
            auth_route_config: config.auth_route_config(),
        };

        Self::install(outer_auth_manager, auth_config, include_existing_root_login)
            .await
            .map(Some)
    }

    /// Builds the account pool from the profile manifest and installs it into the existing root
    /// AuthManager using Codex's native `ExternalAuth` extension point.
    ///
    /// When `include_existing_root_login` is true, a normal existing ChatGPT OAuth login in the
    /// root `CODEX_HOME` is imported only when first creating a pool. An existing manifest is
    /// authoritative, so explicitly removed root profiles are never silently re-added.
    pub async fn install(
        outer_auth_manager: Arc<AuthManager>,
        auth_config: AuthConfig,
        include_existing_root_login: bool,
    ) -> Result<Self, AccountPoolRuntimeError> {
        if Self::is_home_suspended(&auth_config.codex_home) {
            return Err(AccountPoolRuntimeError::Suspended);
        }
        if outer_auth_manager.is_workload_identity_selected() {
            return Err(AccountPoolRuntimeError::WorkloadIdentitySelected);
        }
        if outer_auth_manager.has_external_auth() {
            return Err(AccountPoolRuntimeError::ExistingExternalAuth);
        }

        let store = AccountProfileStore::new(auth_config.codex_home.clone());
        let runtime_state_store = AccountRuntimeStateStore::new(auth_config.codex_home.clone());
        let (mut runtime_state, runtime_state_issue) = match runtime_state_store.load() {
            Ok(state) => (state, None),
            Err(error) => {
                tracing::warn!("ignoring invalid account runtime state: {error}");
                (AccountRuntimeState::default(), Some(error.to_string()))
            }
        };

        if include_existing_root_login
            && !store.manifest_path().exists()
            && outer_auth_manager
                .auth()
                .await
                .is_some_and(|auth| matches!(auth, CodexAuth::Chatgpt(_)))
        {
            store.ensure_legacy_root_profile(Some("Existing login".to_string()), 0)?;
        }

        let profiles = store.load_profiles()?;
        if profiles.is_empty() {
            return Err(AccountPoolRuntimeError::NoConfiguredProfiles);
        }

        let pool = Arc::new(AccountPool::new());
        let mut profile_issues = Vec::new();
        for profile in profiles {
            match materialize_profile(&pool, &auth_config, profile).await? {
                MaterializeProfile::Registered | MaterializeProfile::Duplicate => {}
                MaterializeProfile::Unsupported(issue) => profile_issues.push(issue),
            }
        }

        if pool.snapshots().is_empty() {
            return Err(AccountPoolRuntimeError::NoUsableProfiles(profile_issues));
        }

        restore_runtime_state(&pool, &runtime_state)?;

        // Resolve and validate an initial pooled identity through the existing AuthManager policy
        // before returning the runtime. This preserves forced login/workspace policy enforcement.
        outer_auth_manager
            .set_external_auth(Arc::new(AccountPoolExternalAuth::new(Arc::clone(&pool))))
            .await?;
        if Self::is_home_suspended(&auth_config.codex_home) {
            outer_auth_manager.suspend_pool_auth();
            return Err(AccountPoolRuntimeError::Suspended);
        }

        let initial_generation = pool
            .lease()
            .map(|lease| lease.generation())
            .unwrap_or_default();
        let suspended = Arc::new(AtomicBool::new(false));
        let lifecycle_lock = Arc::new(tokio::sync::Mutex::new(()));
        if let Err(error) = runtime_state_store.synchronize_pool(&pool, &mut runtime_state) {
            tracing::warn!("failed to persist initial account runtime state: {error}");
        }
        let auth_sync_task = spawn_auth_sync_task(
            Arc::clone(&pool),
            Arc::clone(&outer_auth_manager),
            runtime_state_store.clone(),
            initial_generation,
            runtime_state,
            Arc::clone(&suspended),
            Arc::clone(&lifecycle_lock),
        );
        let keepalive_task = spawn_auth_keepalive_task(
            Arc::clone(&pool),
            auth_config.codex_home.clone(),
            Arc::clone(&suspended),
            Arc::clone(&lifecycle_lock),
        );

        Ok(Self {
            pool,
            store,
            runtime_state_store,
            auth_config,
            outer_auth_manager,
            profile_issues,
            runtime_state_issue,
            auth_sync_task,
            keepalive_task,
            suspended,
            lifecycle_lock,
        })
    }

    /// Reports a persisted logout without loading any profile credentials.
    pub fn is_home_suspended(codex_home: &Path) -> bool {
        codex_home
            .join(SUSPENDED_MARKER)
            .try_exists()
            .unwrap_or(true)
    }

    /// Persists a pool logout while retaining profile enrollment and managed credentials.
    pub fn suspend_home(codex_home: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(codex_home)?;
        std::fs::write(
            codex_home.join(SUSPENDED_MARKER),
            b"Account pool suspended after logout.\n",
        )
    }

    /// Re-enables account pooling after an explicit successful account selection.
    pub fn resume_home(codex_home: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(codex_home.join(SUSPENDED_MARKER)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended.load(Ordering::Acquire)
            || Self::is_home_suspended(&self.auth_config.codex_home)
    }

    /// Detaches execution auth before the caller clears the ordinary root login.
    pub async fn suspend_for_logout(&self) -> std::io::Result<()> {
        let _guard = self.lifecycle_lock.lock().await;
        Self::suspend_home(&self.auth_config.codex_home)?;
        self.suspended.store(true, Ordering::Release);
        self.outer_auth_manager.suspend_pool_auth();
        Ok(())
    }

    /// Restores the bridge only after validating the selected profile through normal auth policy.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "serialize authentication bridge replacement with logout and profile refresh"
    )]
    pub async fn resume(&self) -> Result<(), AccountPoolRuntimeError> {
        let _guard = self.lifecycle_lock.lock().await;
        if self.outer_auth_manager.has_external_auth() && self.suspended.load(Ordering::Acquire) {
            Self::suspend_home(&self.auth_config.codex_home)?;
            return Err(AccountPoolRuntimeError::ExistingExternalAuth);
        }
        Self::resume_home(&self.auth_config.codex_home)?;
        if self.suspended.load(Ordering::Acquire) {
            let state = match self.runtime_state_store.load() {
                Ok(state) => state,
                Err(error) => {
                    Self::suspend_home(&self.auth_config.codex_home)?;
                    return Err(std::io::Error::other(error).into());
                }
            };
            self.pool
                .merge_runtime_state(&state, &AccountRuntimeState::default(), None);
            let _ = crate::recover_pool_auth_from_disk(&self.pool).await;
            self.outer_auth_manager.resume_pool_auth();
            match self
                .outer_auth_manager
                .set_external_auth(Arc::new(AccountPoolExternalAuth::new(Arc::clone(
                    &self.pool,
                ))))
                .await
            {
                Ok(()) => self.suspended.store(false, Ordering::Release),
                Err(error) => {
                    Self::suspend_home(&self.auth_config.codex_home)?;
                    self.outer_auth_manager.suspend_pool_auth();
                    return Err(error.into());
                }
            }
        }
        if Self::is_home_suspended(&self.auth_config.codex_home) {
            self.suspended.store(true, Ordering::Release);
            self.outer_auth_manager.suspend_pool_auth();
            return Err(AccountPoolRuntimeError::Suspended);
        }
        Ok(())
    }

    /// Registers Ready profiles that appeared on disk after this process installed the pool.
    ///
    /// Long-lived TUI/app-server daemons otherwise keep serving the startup snapshot, so
    /// `codex account add` is invisible to `/account` until restart.
    pub async fn sync_missing_profiles(
        &self,
    ) -> Result<Vec<AccountProfileId>, AccountPoolRuntimeError> {
        if self.is_suspended() {
            return Ok(Vec::new());
        }
        let known = self
            .pool
            .snapshots()
            .into_iter()
            .map(|snapshot| snapshot.profile.id)
            .collect::<std::collections::HashSet<_>>();
        let mut added = Vec::new();
        for profile in self.store.load_profiles()? {
            if known.contains(&profile.id) {
                continue;
            }
            let id = profile.id.clone();
            match materialize_profile(&self.pool, &self.auth_config, profile).await? {
                MaterializeProfile::Registered => added.push(id),
                MaterializeProfile::Duplicate => {}
                MaterializeProfile::Unsupported(issue) => {
                    tracing::warn!(
                        profile_id = %issue.profile.id,
                        reason = %issue.reason,
                        "skipping newly added account profile"
                    );
                }
            }
        }
        if !added.is_empty()
            && let Ok(state) = self.runtime_state_store.load()
            && let Some(selected) = state.active_profile_id
            && added.contains(&selected)
        {
            let _ = self.pool.activate(&selected);
        }
        Ok(added)
    }

    pub fn pool(&self) -> Arc<AccountPool> {
        Arc::clone(&self.pool)
    }

    pub fn store(&self) -> &AccountProfileStore {
        &self.store
    }

    pub fn runtime_state_store(&self) -> &AccountRuntimeStateStore {
        &self.runtime_state_store
    }

    pub fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.outer_auth_manager)
    }

    pub fn profile_issues(&self) -> &[AccountPoolRuntimeProfileIssue] {
        &self.profile_issues
    }

    pub fn runtime_state_issue(&self) -> Option<&str> {
        self.runtime_state_issue.as_deref()
    }
}

impl Drop for AccountPoolRuntime {
    fn drop(&mut self) {
        self.auth_sync_task.abort();
        self.keepalive_task.abort();
    }
}

enum MaterializeProfile {
    Registered,
    Duplicate,
    Unsupported(AccountPoolRuntimeProfileIssue),
}

async fn materialize_profile(
    pool: &AccountPool,
    auth_config: &AuthConfig,
    profile: AccountProfile,
) -> Result<MaterializeProfile, AccountPoolRuntimeError> {
    let mut profile_auth_config = auth_config.clone();
    profile_auth_config.codex_home = profile.credential_home.clone();
    let manager = AuthManager::shared_managed_profile_from_auth_config(profile_auth_config).await;

    // A disabled profile keeps its slot without an auth probe: the user parked it on
    // purpose and its credentials must not be touched until it is re-enabled.
    if profile.disabled {
        return match pool.register(profile, manager) {
            Ok(()) => Ok(MaterializeProfile::Registered),
            Err(AccountPoolError::DuplicateProfile(_)) => Ok(MaterializeProfile::Duplicate),
            Err(error) => Err(error.into()),
        };
    }

    match manager.auth().await {
        Some(auth) if auth.is_chatgpt_auth() => match pool.register(profile, manager) {
            Ok(()) => Ok(MaterializeProfile::Registered),
            Err(AccountPoolError::DuplicateProfile(_)) => Ok(MaterializeProfile::Duplicate),
            Err(error) => Err(error.into()),
        },
        Some(auth) => Ok(MaterializeProfile::Unsupported(
            AccountPoolRuntimeProfileIssue {
                profile,
                reason: format!(
                    "profile uses unsupported auth mode {:?}; native subscription pooling requires ChatGPT auth",
                    auth.api_auth_mode()
                ),
            },
        )),
        None => Ok(MaterializeProfile::Unsupported(
            AccountPoolRuntimeProfileIssue {
                profile,
                reason: "profile has no usable persisted ChatGPT OAuth credentials".to_string(),
            },
        )),
    }
}

fn restore_runtime_state(
    pool: &AccountPool,
    runtime_state: &AccountRuntimeState,
) -> Result<(), AccountPoolError> {
    let known_profiles = pool
        .snapshots()
        .into_iter()
        .map(|snapshot| snapshot.profile.id)
        .collect::<std::collections::HashSet<_>>();

    for profile_state in &runtime_state.profiles {
        if !known_profiles.contains(&profile_state.profile_id) {
            continue;
        }
        pool.update_rate_limits(&profile_state.profile_id, profile_state.rate_limits.clone())?;
        if let Some(observation) = profile_state.window_warmup.clone() {
            let _ = pool.record_window_warmup(&profile_state.profile_id, observation);
        }
    }

    // Recreate known future cooldowns before selecting the persisted active account. We use the
    // normal lease/generation path so restored state obeys exactly the same invariants as live
    // quota failures and does not require a second private mutation API on AccountPool.
    for profile_state in &runtime_state.profiles {
        let Some(reset_at) = profile_state.exhausted_until else {
            continue;
        };
        if reset_at <= Utc::now() || !known_profiles.contains(&profile_state.profile_id) {
            continue;
        }
        if let Ok(lease) = pool.activate(&profile_state.profile_id) {
            let recovery = match profile_state.backend_resets_at {
                Some(backend_reset) => crate::AccountQuotaRecovery::BackendReset {
                    resets_at: backend_reset,
                    retry_at: reset_at,
                },
                None => crate::AccountQuotaRecovery::Reprobe { retry_at: reset_at },
            };
            let _ =
                pool.mark_exhausted_for_recovery(&lease, recovery, /*rate_limits*/ None)?;
        }
    }

    // Restore soft early-switch preferences after authoritative cooldowns. They retain usable
    // quota and must remain distinct from exhaustion across restarts and processes.
    pool.merge_runtime_state(runtime_state, &AccountRuntimeState::default(), None);

    if let Some(active_profile_id) = runtime_state.active_profile_id.as_ref()
        && known_profiles.contains(active_profile_id)
        && pool.activate(active_profile_id).is_ok()
    {
        return Ok(());
    }

    // Establish a deterministic fill-first active account if the saved account is unavailable.
    // A pool whose every profile is cooling down is still a valid pool: it installs, reports
    // the cooldowns, and recovers on its own once a reset passes. Only unexpected errors fail
    // the install.
    match pool.lease() {
        Ok(_) | Err(AccountPoolError::NoEligibleAccount) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "account_runtime_tests.rs"]
mod tests;

#[expect(
    clippy::await_holding_invalid_type,
    reason = "prevent late background credential refresh from restoring authentication after logout"
)]
fn spawn_auth_sync_task(
    pool: Arc<AccountPool>,
    auth_manager: Arc<AuthManager>,
    runtime_state_store: AccountRuntimeStateStore,
    mut observed_generation: u64,
    mut runtime_state: AccountRuntimeState,
    suspended: Arc<AtomicBool>,
    lifecycle_lock: Arc<tokio::sync::Mutex<()>>,
) -> JoinHandle<()> {
    let mut changes = pool.change_receiver();
    tokio::spawn(async move {
        let mut codex_home = runtime_state_store.path();
        codex_home.pop();
        let mut poll = tokio::time::interval(std::time::Duration::from_millis(250));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut credential_versions = std::collections::HashMap::new();
        loop {
            tokio::select! {
                result = changes.changed() => { if result.is_err() { break; } }
                _ = poll.tick() => {}
            }
            {
                let _guard = lifecycle_lock.lock().await;
                let marker_present = AccountPoolRuntime::is_home_suspended(&codex_home);
                if marker_present {
                    if !suspended.swap(true, Ordering::AcqRel) {
                        auth_manager.suspend_pool_auth();
                    }
                    continue;
                }
                if suspended.load(Ordering::Acquire) {
                    if auth_manager.has_external_auth() {
                        continue;
                    }
                    if let Err(error) =
                        runtime_state_store.synchronize_pool(&pool, &mut runtime_state)
                    {
                        tracing::warn!("failed to load selected account before resuming: {error}");
                        let _ = AccountPoolRuntime::suspend_home(&codex_home);
                        continue;
                    }
                    let _ = crate::recover_pool_auth_from_disk(&pool).await;
                    auth_manager.resume_pool_auth();
                    match auth_manager
                        .set_external_auth(Arc::new(AccountPoolExternalAuth::new(Arc::clone(
                            &pool,
                        ))))
                        .await
                    {
                        Ok(()) => suspended.store(false, Ordering::Release),
                        Err(error) => {
                            tracing::warn!("failed to resume account pooling: {error}");
                            let _ = AccountPoolRuntime::suspend_home(&codex_home);
                            continue;
                        }
                    }
                    if AccountPoolRuntime::is_home_suspended(&codex_home) {
                        suspended.store(true, Ordering::Release);
                        auth_manager.suspend_pool_auth();
                        continue;
                    }
                }
            }
            if let Err(error) = runtime_state_store.synchronize_pool(&pool, &mut runtime_state) {
                tracing::warn!("failed to persist account runtime state: {error}");
            }

            let enabled: std::collections::HashMap<_, _> = pool
                .snapshots()
                .into_iter()
                .filter(|snapshot| !snapshot.profile.disabled)
                .map(|snapshot| (snapshot.profile.id, snapshot.profile.credential_home))
                .collect();
            let mut credentials_changed = false;
            for (profile_id, manager) in pool.auth_managers() {
                let _guard = lifecycle_lock.lock().await;
                if suspended.load(Ordering::Acquire)
                    || AccountPoolRuntime::is_home_suspended(&codex_home)
                {
                    break;
                }
                let Some(home) = enabled.get(&profile_id) else {
                    continue;
                };
                let version = crate::account_credentials::credential_version(home);
                if credential_versions.get(&profile_id) == Some(&version) {
                    continue;
                }
                credential_versions.insert(profile_id.clone(), version.clone());
                let _ = crate::refresh_profile_auth_from_disk(&pool, &profile_id, &manager).await;
                // The marker is published only by a completed login, including
                // keyring-backed logins. A previously loaded new token is still
                // proof of repair even if this reload itself changes nothing.
                if version.is_some()
                    && let Some(auth) = manager.auth_cached().filter(CodexAuth::is_chatgpt_auth)
                    && manager.refresh_failure_for_auth(&auth).is_none()
                {
                    let _ = pool.clear_authentication_unavailable(&profile_id);
                }
                credentials_changed = true;
            }
            credential_versions.retain(|profile_id, _| enabled.contains_key(profile_id));
            if credentials_changed {
                let _guard = lifecycle_lock.lock().await;
                if !suspended.load(Ordering::Acquire)
                    && !AccountPoolRuntime::is_home_suspended(&codex_home)
                {
                    auth_manager.reload().await;
                }
            }

            if suspended.load(Ordering::Acquire)
                || AccountPoolRuntime::is_home_suspended(&codex_home)
            {
                continue;
            }

            // A fully exhausted pool has no schedulable identity to sync; skipping the reload
            // avoids hammering the outer AuthManager on every bookkeeping notification.
            let Ok(lease) = pool.lease() else {
                continue;
            };
            let current_generation = lease.generation();
            if current_generation == observed_generation {
                continue;
            }
            observed_generation = current_generation;
            let _guard = lifecycle_lock.lock().await;
            if !suspended.load(Ordering::Acquire)
                && !AccountPoolRuntime::is_home_suspended(&codex_home)
            {
                auth_manager.reload().await;
            }
        }
    })
}

/// Periodically touches every profile's own AuthManager so idle stand-by accounts run their
/// normal proactive token refresh instead of silently aging out while another account is active.
#[expect(
    clippy::await_holding_invalid_type,
    reason = "keep profile refresh and logout ordered while retaining enrolled credentials"
)]
fn spawn_auth_keepalive_task(
    pool: Arc<AccountPool>,
    codex_home: std::path::PathBuf,
    suspended: Arc<AtomicBool>,
    lifecycle_lock: Arc<tokio::sync::Mutex<()>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(AUTH_KEEPALIVE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires immediately; install already probed every profile.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            for (profile_id, manager) in pool.auth_managers() {
                let _guard = lifecycle_lock.lock().await;
                if suspended.load(Ordering::Acquire)
                    || AccountPoolRuntime::is_home_suspended(&codex_home)
                {
                    break;
                }
                if pool
                    .snapshots()
                    .iter()
                    .any(|snapshot| snapshot.profile.id == profile_id && snapshot.profile.disabled)
                {
                    continue;
                }
                // Reload from disk first so CLI re-login is observed and stale cached tokens cannot
                // overwrite freshly written credentials on a later refresh.
                crate::keepalive_reload_profile_auth(pool.as_ref(), &profile_id, manager).await;
            }
        }
    })
}

#[derive(Debug, Error)]
pub enum AccountPoolRuntimeError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] AccountProfileStoreError),
    #[error(transparent)]
    Pool(#[from] AccountPoolError),
    #[error(transparent)]
    AuthManager(#[from] AuthManagerInitializationError),
    #[error(transparent)]
    Refresh(#[from] RefreshTokenError),
    #[error("native account pooling is not available while workload identity is selected")]
    WorkloadIdentitySelected,
    #[error("native account pooling cannot replace an already configured external auth source")]
    ExistingExternalAuth,
    #[error("no account profiles are configured")]
    NoConfiguredProfiles,
    #[error("no usable ChatGPT account profiles are available")]
    NoUsableProfiles(Vec<AccountPoolRuntimeProfileIssue>),
    #[error("account pooling is suspended after logout; select an account to resume")]
    Suspended,
}
