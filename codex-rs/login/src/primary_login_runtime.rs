//! Keeps application sign-in stable while inference rotates through the account pool.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::Weak;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;
use tokio::task::AbortHandle;

use crate::AccountProfileStore;
use crate::AuthConfig;
use crate::AuthManager;
use crate::CodexAuth;
use crate::ExternalAuth;
use crate::ExternalAuthFuture;
use crate::ExternalAuthRefreshContext;
use crate::RefreshTokenError;
use crate::primary_login::PrimaryLoginSource;
use crate::primary_login::PrimaryLoginState;
use crate::primary_login::PrimaryLoginStore;
use crate::primary_login::managed_owner_hash;
use crate::primary_login::ready_profile;
use crate::primary_login::stored_owner_hash;

#[path = "primary_login_fingerprint.rs"]
mod fingerprint;
use fingerprint::fingerprint_auth;
use fingerprint::storage_fingerprint;
use fingerprint::stored_auth;

/// Publishes the selected host account's destination requirements before a Remote request.
/// Implementations must keep this policy owner separate from inference and must never
/// call the host facade while preparing its concrete credential source.
pub trait PrimaryLoginPolicyLoader: Send + Sync {
    fn prepare(&self, source_manager: Arc<AuthManager>) -> ExternalAuthFuture<'_, ()>;
}

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const OBSERVATION_BUDGET: Duration = Duration::from_secs(1);

enum Resolution {
    Request,
    Observe,
    Recover(ExternalAuthRefreshContext),
}

struct CachedSource {
    revision: u64,
    root_external_revision: u64,
    source: PrimaryLoginSource,
    credential_home: PathBuf,
    config: AuthConfig,
    manager: Arc<AuthManager>,
    fingerprint: Option<[u8; 32]>,
}

struct PrimaryLoginResolver {
    config: AuthConfig,
    store: PrimaryLoginStore,
    selected: Arc<AsyncMutex<Option<CachedSource>>>,
    policy_loader: RwLock<Option<Weak<dyn PrimaryLoginPolicyLoader>>>,
    root_external_auth: RwLock<Option<Arc<dyn ExternalAuth>>>,
    root_external_revision: AtomicU64,
}

impl PrimaryLoginResolver {
    async fn resolve_selected(&self, resolution: Resolution) -> io::Result<CodexAuth> {
        // A single selected manager serializes refresh and observations without retaining managers
        // for every inference account. Source changes are rechecked after every asynchronous load.
        let mut selected = Arc::clone(&self.selected).lock_owned().await;
        let state = self.store.load()?;
        let root_external_revision = self.root_external_revision.load(Ordering::Acquire);
        if state.source == PrimaryLoginSource::SignedOut {
            *selected = None;
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "application sign-in is signed out",
            ));
        }
        let mut config = self.config.clone();
        if let PrimaryLoginSource::Profile {
            profile_id,
            owner_hash,
        } = &state.source
        {
            let profile = ready_profile(
                &AccountProfileStore::new(self.config.codex_home.clone()),
                profile_id,
            )?;
            config.codex_home = profile.credential_home;
            if stored_owner_hash(&config)? != *owner_hash {
                return Err(source_changed());
            }
        }
        let fingerprint = storage_fingerprint(&config, &state.source)?;
        let source_needs_load = selected.as_ref().is_none_or(|cached| {
            cached.revision != state.revision
                || cached.root_external_revision != root_external_revision
                || cached.source != state.source
                || cached.credential_home != config.codex_home
                || cached.fingerprint != fingerprint
        });
        if matches!(resolution, Resolution::Observe)
            && state.source == PrimaryLoginSource::RootLogin
            && source_needs_load
            && !config.root_auth_load_is_local()?
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "host authentication requires request-time hydration",
            ));
        }
        if selected.as_ref().is_none_or(|cached| {
            cached.revision != state.revision
                || cached.root_external_revision != root_external_revision
                || cached.source != state.source
                || cached.credential_home != config.codex_home
        }) {
            let manager = match &state.source {
                PrimaryLoginSource::RootLogin => AuthManager::shared_from_auth_config(
                    config.clone(),
                    /*enable_codex_api_key_env*/ false,
                )
                .await
                .map_err(io::Error::other)?,
                PrimaryLoginSource::Profile { .. } => {
                    AuthManager::shared_managed_profile_from_auth_config(config.clone()).await
                }
                PrimaryLoginSource::SignedOut => {
                    unreachable!("signed-out source was handled above")
                }
            };
            if state.source == PrimaryLoginSource::RootLogin {
                let external = self
                    .root_external_auth
                    .read()
                    .map_err(|_| source_changed())?
                    .clone();
                if let Some(external) = external {
                    manager
                        .set_external_auth(external)
                        .await
                        .map_err(io::Error::other)?;
                }
            }
            if manager.is_workload_identity_selected() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "workload identity authentication cannot be replaced by primary-login selection",
                ));
            }
            self.verify_source(&state, &config)?;
            *selected = Some(CachedSource {
                revision: state.revision,
                root_external_revision,
                source: state.source.clone(),
                credential_home: config.codex_home.clone(),
                config,
                manager,
                fingerprint,
            });
        }
        let cached = selected.as_mut().ok_or_else(source_changed)?;
        if cached.fingerprint != fingerprint {
            cached.manager.reload().await;
            self.verify_source(&state, &cached.config)?;
        }
        let loader = self
            .policy_loader
            .read()
            .map_err(|_| source_changed())?
            .as_ref()
            .and_then(Weak::upgrade);
        if !matches!(resolution, Resolution::Observe)
            && let Some(loader) = loader
        {
            let policy = loader.prepare(Arc::clone(&cached.manager)).await;
            self.verify_source(&state, &cached.config)?;
            if policy.is_err() {
                // The policy owner closes network permits. Preserve this login lifetime so the
                // Remote transport can retry without forgetting its enable state or pairing.
                return Err(io::Error::other(RefreshTokenError::Policy(
                    codex_http_client::NetworkPolicyDenied::Unavailable,
                )));
            }
        }
        let mut auth = match resolution {
            Resolution::Observe => cached.manager.auth_cached(),
            Resolution::Request | Resolution::Recover(_) => cached.manager.auth().await,
        }
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "selected primary-login source has no usable authentication",
            )
        })?;
        if let Some(error) = cached.manager.refresh_failure_for_auth(&auth) {
            return Err(io::Error::other(error));
        }
        self.verify_source(&state, &cached.config)?;
        if let Resolution::Recover(context) = resolution {
            // An older request can report unauthorized after the application selected another
            // workspace. The new login is already the correct identity and needs no refresh.
            if context.previous_account_id.is_none()
                || context.previous_account_id == auth.get_account_id()
            {
                let mut recovery = cached.manager.unauthorized_recovery();
                while recovery.has_next() {
                    let step = recovery.next().await.map_err(io::Error::other)?;
                    self.verify_source(&state, &cached.config)?;
                    if step.auth_state_changed() == Some(true) {
                        break;
                    }
                }
                auth = cached.manager.auth().await.ok_or_else(source_changed)?;
            }
        }
        self.verify_source(&state, &cached.config)?;
        if let PrimaryLoginSource::Profile { owner_hash, .. } = &state.source
            && managed_owner_hash(&auth)? != *owner_hash
        {
            return Err(source_changed());
        }
        let current = stored_auth(&cached.config, &state.source)?;
        if matches!(
            auth,
            CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_)
        ) {
            // A same-owner re-login can replace tokens while proactive refresh awaits. Do not
            // return that older credential snapshot, even though its workspace still matches.
            if current.as_ref().and_then(|auth| auth.tokens.as_ref())
                != Some(&auth.get_token_data()?)
            {
                return Err(source_changed());
            }
        } else if fingerprint_auth(current.as_ref())? != fingerprint {
            return Err(source_changed());
        }
        if let Some(error) = cached.manager.refresh_failure_for_auth(&auth) {
            return Err(io::Error::other(error));
        }
        if state.source == PrimaryLoginSource::RootLogin
            && self.root_external_revision.load(Ordering::Acquire) != root_external_revision
        {
            return Err(source_changed());
        }
        cached.fingerprint = fingerprint_auth(current.as_ref())?;
        Ok(auth)
    }

    fn cached_owner_is_current(&self, auth: Option<&CodexAuth>) -> io::Result<bool> {
        let state = self.store.load()?;
        match state.source {
            PrimaryLoginSource::SignedOut => Ok(false),
            PrimaryLoginSource::Profile {
                profile_id,
                owner_hash,
            } => {
                let profile = ready_profile(
                    &AccountProfileStore::new(self.config.codex_home.clone()),
                    &profile_id,
                )?;
                let mut config = self.config.clone();
                config.codex_home = profile.credential_home;
                if stored_owner_hash(&config)? != owner_hash {
                    return Err(source_changed());
                }
                Ok(auth.is_some_and(|auth| {
                    managed_owner_hash(auth).is_ok_and(|current| current == owner_hash)
                }))
            }
            PrimaryLoginSource::RootLogin => {
                let Some(auth) = auth else {
                    return Ok(true);
                };
                if matches!(
                    auth,
                    CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_)
                ) {
                    let stored = stored_auth(&self.config, &PrimaryLoginSource::RootLogin)?;
                    let tokens = stored
                        .as_ref()
                        .and_then(|auth| auth.tokens.as_ref())
                        .ok_or_else(source_changed)?;
                    return Ok(crate::primary_login::tokens_owner_hash(tokens)?
                        == crate::primary_login::tokens_owner_hash(&auth.get_token_data()?)?);
                }
                // Hydrating a new root PAT can await HTTP. Retire the old cached identity before
                // that wait while retaining stock environment-token precedence in the resolver.
                let stored = stored_auth(&self.config, &PrimaryLoginSource::RootLogin)?;
                if auth.is_personal_access_token_auth()
                    && let Some(previous) = stored
                        .as_ref()
                        .and_then(|stored| stored.personal_access_token.as_ref())
                {
                    return Ok(auth.get_token().is_ok_and(|current| &current == previous));
                }
                Ok(true)
            }
        }
    }

    fn verify_source(&self, expected: &PrimaryLoginState, config: &AuthConfig) -> io::Result<()> {
        if self.store.load()? != *expected {
            return Err(source_changed());
        }
        if let PrimaryLoginSource::Profile {
            profile_id,
            owner_hash,
        } = &expected.source
        {
            let profile = ready_profile(
                &AccountProfileStore::new(self.config.codex_home.clone()),
                profile_id,
            )?;
            if profile.credential_home != config.codex_home
                || stored_owner_hash(config)? != *owner_hash
            {
                return Err(source_changed());
            }
        }
        Ok(())
    }
}

impl ExternalAuth for PrimaryLoginResolver {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(self.resolve_selected(Resolution::Request))
    }

    fn refresh(&self, context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(self.resolve_selected(Resolution::Recover(context)))
    }

    fn classify_error(&self, error: io::Error) -> RefreshTokenError {
        let message = error.to_string();
        if let Some(source) = error.into_inner()
            && let Ok(refresh_error) = source.downcast::<RefreshTokenError>()
        {
            return *refresh_error;
        }
        // Invalid/missing references clear the facade on reload instead of retaining an old login.
        RefreshTokenError::Transient(io::Error::other(message))
    }
}

/// Application-owned auth facade used by RemoteControl, independent of inference pool selection.
///
/// Keep this runtime alive for the transport lifetime. Its watcher only inspects local credentials
/// and stops on drop; request resolution and unauthorized recovery own any OAuth refresh work.
pub struct PrimaryLoginRuntime {
    facade: Arc<AuthManager>,
    resolver: Option<Arc<PrimaryLoginResolver>>,
    watcher: Mutex<Option<AbortHandle>>,
}

impl PrimaryLoginRuntime {
    /// Starts with the caller's control-plane auth route and managed policy. The caller must supply
    /// an independent policy owner so a primary-login change cannot revoke inference requests.
    pub async fn start(config: AuthConfig) -> io::Result<Arc<Self>> {
        let facade = AuthManager::shared_host_login_facade_from_auth_config(config.clone())
            .await
            .map_err(io::Error::other)?;
        if facade.is_workload_identity_selected() {
            let state = PrimaryLoginStore::new(config.codex_home).load()?;
            if state.source != PrimaryLoginSource::RootLogin {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "primary-login selection cannot override host-managed workload identity",
                ));
            }
            return Ok(Arc::new(Self {
                facade,
                resolver: None,
                watcher: Mutex::new(None),
            }));
        }
        let resolver = Arc::new(PrimaryLoginResolver {
            store: PrimaryLoginStore::new(config.codex_home.clone()),
            config,
            selected: Arc::new(AsyncMutex::new(None)),
            policy_loader: RwLock::new(None),
            root_external_auth: RwLock::new(None),
            root_external_revision: AtomicU64::new(0),
        });
        facade
            .install_host_login_source(resolver.clone())
            .map_err(io::Error::other)?;
        let runtime = Arc::new(Self {
            facade,
            resolver: Some(resolver),
            watcher: Mutex::new(None),
        });
        let _ = runtime.sync().await;
        let weak = Arc::downgrade(&runtime);
        let watcher = tokio::spawn(async move {
            let mut interval = tokio::time::interval(POLL_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await;
            loop {
                interval.tick().await;
                let Some(runtime) = weak.upgrade() else {
                    break;
                };
                let _ = tokio::time::timeout(OBSERVATION_BUDGET, runtime.sync()).await;
                drop(runtime);
            }
        });
        *runtime
            .watcher
            .lock()
            .map_err(|_| io::Error::other("primary-login watcher lock is poisoned"))? =
            Some(watcher.abort_handle());
        Ok(runtime)
    }

    /// Retains the client-owned refresh bridge for an external root login. It is never used by
    /// managed pool profiles, and changing it invalidates in-flight root-source observations.
    pub fn set_root_external_auth(
        &self,
        external: Option<Arc<dyn ExternalAuth>>,
    ) -> io::Result<()> {
        if let Some(resolver) = &self.resolver {
            *resolver
                .root_external_auth
                .write()
                .map_err(|_| source_changed())? = external;
            resolver
                .root_external_revision
                .fetch_add(1, Ordering::Release);
        }
        Ok(())
    }

    pub fn set_policy_loader(&self, loader: Weak<dyn PrimaryLoginPolicyLoader>) {
        if let Some(resolver) = &self.resolver
            && let Ok(mut current) = resolver.policy_loader.write()
        {
            *current = Some(loader);
        }
    }

    pub fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.facade)
    }

    /// Applies external source/credential changes immediately without proactive refresh. This is
    /// also useful after successful application sign-in or an explicit source-selection action.
    pub async fn sync(&self) -> io::Result<()> {
        let Some(resolver) = &self.resolver else {
            return Ok(());
        };
        // Revocation must not queue behind an OAuth request in the selected manager. This local
        // check still clears the old owner when the observation budget cancels the later wait.
        if !resolver
            .cached_owner_is_current(self.facade.auth_cached().as_ref())
            .unwrap_or(false)
        {
            self.facade.sync_host_login_cached_auth(None)?;
        }
        match resolver.resolve_selected(Resolution::Observe).await {
            Ok(auth) => self.facade.sync_host_login_cached_auth(Some(auth)),
            Err(error) => {
                self.facade.sync_host_login_cached_auth(None)?;
                if error.kind() == io::ErrorKind::NotConnected
                    && resolver
                        .store
                        .load()
                        .is_ok_and(|state| state.source == PrimaryLoginSource::SignedOut)
                {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }
}

impl Drop for PrimaryLoginRuntime {
    fn drop(&mut self) {
        if let Ok(mut watcher) = self.watcher.lock()
            && let Some(watcher) = watcher.take()
        {
            watcher.abort();
        }
    }
}

fn source_changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "primary-login source or identity changed; select the account again",
    )
}

#[cfg(test)]
#[path = "primary_login_runtime_tests.rs"]
mod tests;
