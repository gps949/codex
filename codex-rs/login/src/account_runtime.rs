use std::sync::Arc;

use chrono::Utc;
use thiserror::Error;
use tokio::task::JoinHandle;

use crate::AccountPool;
use crate::AccountPoolError;
use crate::AccountPoolExternalAuth;
use crate::AccountProfile;
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
    outer_auth_manager: Arc<AuthManager>,
    profile_issues: Vec<AccountPoolRuntimeProfileIssue>,
    runtime_state_issue: Option<String>,
    auth_sync_task: JoinHandle<()>,
    keepalive_task: JoinHandle<()>,
}

/// ChatGPT refresh tokens can expire when a profile stays idle for weeks. Touching every
/// profile's AuthManager on this cadence lets its own 8-day proactive-refresh policy keep
/// stand-by credentials alive long before they are needed.
const AUTH_KEEPALIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);

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
        if !store.manifest_path().is_file() {
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
            let mut profile_auth_config = auth_config.clone();
            profile_auth_config.codex_home = profile.credential_home.clone();
            let manager = AuthManager::shared_from_auth_config(
                profile_auth_config,
                /*enable_codex_api_key_env*/ false,
            )
            .await?;

            if manager.is_workload_identity_selected() {
                return Err(AccountPoolRuntimeError::WorkloadIdentitySelected);
            }

            // A disabled profile keeps its slot without an auth probe: the user parked it on
            // purpose and its credentials must not be touched until it is re-enabled.
            if profile.disabled {
                pool.register(profile, manager)?;
                continue;
            }

            match manager.auth().await {
                Some(auth) if auth.is_chatgpt_auth() => {
                    pool.register(profile, manager)?;
                }
                Some(auth) => profile_issues.push(AccountPoolRuntimeProfileIssue {
                    profile,
                    reason: format!(
                        "profile uses unsupported auth mode {:?}; native subscription pooling requires ChatGPT auth",
                        auth.api_auth_mode()
                    ),
                }),
                None => profile_issues.push(AccountPoolRuntimeProfileIssue {
                    profile,
                    reason: "profile has no usable stored authentication".to_string(),
                }),
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

        let initial_generation = pool
            .lease()
            .map(|lease| lease.generation())
            .unwrap_or_default();
        if let Err(error) = runtime_state_store.synchronize_pool(&pool, &mut runtime_state) {
            tracing::warn!("failed to persist initial account runtime state: {error}");
        }
        let auth_sync_task = spawn_auth_sync_task(
            Arc::clone(&pool),
            Arc::clone(&outer_auth_manager),
            runtime_state_store.clone(),
            initial_generation,
            runtime_state,
        );
        let keepalive_task = spawn_auth_keepalive_task(Arc::clone(&pool));

        Ok(Self {
            pool,
            store,
            runtime_state_store,
            outer_auth_manager,
            profile_issues,
            runtime_state_issue,
            auth_sync_task,
            keepalive_task,
        })
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
            let _ = pool.mark_exhausted(&lease, Some(reset_at))?;
        }
    }

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

fn spawn_auth_sync_task(
    pool: Arc<AccountPool>,
    auth_manager: Arc<AuthManager>,
    runtime_state_store: AccountRuntimeStateStore,
    mut observed_generation: u64,
    mut runtime_state: AccountRuntimeState,
) -> JoinHandle<()> {
    let mut changes = pool.change_receiver();
    tokio::spawn(async move {
        let mut poll = tokio::time::interval(std::time::Duration::from_millis(250));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                result = changes.changed() => { if result.is_err() { break; } }
                _ = poll.tick() => {}
            }
            if let Err(error) = runtime_state_store.synchronize_pool(&pool, &mut runtime_state) {
                tracing::warn!("failed to persist account runtime state: {error}");
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
            auth_manager.reload().await;
        }
    })
}

/// Periodically touches every profile's own AuthManager so idle stand-by accounts run their
/// normal proactive token refresh instead of silently aging out while another account is active.
fn spawn_auth_keepalive_task(pool: Arc<AccountPool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(AUTH_KEEPALIVE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires immediately; install already probed every profile.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            for (profile_id, manager) in pool.auth_managers() {
                if manager.auth().await.is_none() {
                    tracing::debug!(%profile_id, "account keep-alive found no usable auth");
                }
            }
        }
    })
}

#[derive(Debug, Error)]
pub enum AccountPoolRuntimeError {
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
}
