use std::sync::Arc;

use thiserror::Error;
use tokio::task::JoinHandle;

use crate::AccountPool;
use crate::AccountPoolError;
use crate::AccountPoolExternalAuth;
use crate::AccountProfile;
use crate::AccountProfileStore;
use crate::AccountProfileStoreError;
use crate::AuthConfig;
use crate::AuthManager;
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
    outer_auth_manager: Arc<AuthManager>,
    profile_issues: Vec<AccountPoolRuntimeProfileIssue>,
    auth_sync_task: JoinHandle<()>,
}

impl AccountPoolRuntime {
    /// Builds the account pool from the profile manifest and installs it into the existing root
    /// AuthManager using Codex's native `ExternalAuth` extension point.
    ///
    /// When `include_existing_root_login` is true, a normal existing ChatGPT OAuth login in the
    /// root `CODEX_HOME` is registered as the compatibility `legacy-root` profile without moving
    /// or copying its credentials.
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
        if include_existing_root_login
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

        // Resolve and validate an initial pooled identity through the existing AuthManager policy
        // before returning the runtime. This preserves forced login/workspace policy enforcement.
        outer_auth_manager
            .set_external_auth(Arc::new(AccountPoolExternalAuth::new(Arc::clone(&pool))))
            .await?;

        let initial_generation = pool
            .lease()
            .map(|lease| lease.generation())
            .unwrap_or_default();
        let auth_sync_task = spawn_auth_sync_task(
            Arc::clone(&pool),
            Arc::clone(&outer_auth_manager),
            initial_generation,
        );

        Ok(Self {
            pool,
            store,
            outer_auth_manager,
            profile_issues,
            auth_sync_task,
        })
    }

    pub fn pool(&self) -> Arc<AccountPool> {
        Arc::clone(&self.pool)
    }

    pub fn store(&self) -> &AccountProfileStore {
        &self.store
    }

    pub fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.outer_auth_manager)
    }

    pub fn profile_issues(&self) -> &[AccountPoolRuntimeProfileIssue] {
        &self.profile_issues
    }
}

impl Drop for AccountPoolRuntime {
    fn drop(&mut self) {
        self.auth_sync_task.abort();
    }
}

fn spawn_auth_sync_task(
    pool: Arc<AccountPool>,
    auth_manager: Arc<AuthManager>,
    mut observed_generation: u64,
) -> JoinHandle<()> {
    let mut changes = pool.change_receiver();
    tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            let current_generation = pool
                .lease()
                .map(|lease| lease.generation())
                .unwrap_or_else(|_| observed_generation.wrapping_add(1));
            if current_generation == observed_generation {
                continue;
            }
            observed_generation = current_generation;
            auth_manager.reload().await;
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
