use std::io;

use codex_config::types::AuthCredentialsStoreMode;
use thiserror::Error;

use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AccountProfileStore;
use crate::AccountProfileStoreError;
use crate::LoginServer;
use crate::ServerOptions;
use crate::account_identity::abandon_pending_login;
use crate::account_identity::reconcile_duplicate_login;
use crate::account_relogin::ReloginStaging;
use crate::run_login_server;

#[path = "account_device_login.rs"]
mod device;

pub use device::PendingAccountDeviceLogin;
pub use device::begin_account_device_login;
pub use device::begin_account_device_relogin;
pub use device::prepare_account_device_login;
pub use device::prepare_account_device_relogin;

/// Result of a completed account login flow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountLoginOutcome {
    pub profile: AccountProfile,
    pub kind: AccountLoginOutcomeKind,
}

/// Whether a login added a new profile or refreshed an existing one for the same ChatGPT user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountLoginOutcomeKind {
    Added,
    RefreshedExistingDuplicate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AuthPersistenceConfig {
    auth_credentials_store_mode: codex_config::types::AuthCredentialsStoreMode,
    auth_keyring_backend_kind: crate::auth::AuthKeyringBackendKind,
}

/// Starts an official browser OAuth flow for a new account profile.
///
/// The profile and its final credential home are allocated before the OAuth server starts. The
/// existing Codex login implementation remains responsible for the OAuth protocol and token
/// persistence; this wrapper only points that login at the profile-specific credential home and
/// promotes the profile to `ready` after the login server reports success.
pub fn begin_account_browser_login(
    store: AccountProfileStore,
    mut options: ServerOptions,
    label: Option<String>,
    priority: u32,
) -> Result<PendingAccountBrowserLogin, AccountLoginFlowError> {
    let profile = store.allocate_profile(label, priority)?;
    options.codex_home = profile.credential_home.clone();

    match run_login_server(options.clone()) {
        Ok(server) => Ok(PendingAccountBrowserLogin {
            store,
            profile,
            server: Some(server),
            mode: AccountLoginMode::NewProfile,
            relogin: None,
            auth: AuthPersistenceConfig {
                auth_credentials_store_mode: options.cli_auth_credentials_store_mode,
                auth_keyring_backend_kind: options.auth_keyring_backend_kind,
            },
        }),
        Err(error) => {
            abandon_after_failed_login(
                &store,
                &profile.id,
                AuthPersistenceConfig {
                    auth_credentials_store_mode: options.cli_auth_credentials_store_mode,
                    auth_keyring_backend_kind: options.auth_keyring_backend_kind,
                },
            );
            Err(error.into())
        }
    }
}

/// Re-runs the official browser OAuth flow for an existing profile in place.
///
/// This repairs a profile whose refresh token expired (or resumes an interrupted first login)
/// without losing the profile's identity, priority, or scheduling history. A failed or cancelled
/// re-login keeps the existing profile and its stored credentials untouched.
pub fn begin_account_browser_relogin(
    store: AccountProfileStore,
    mut options: ServerOptions,
    profile_id: &AccountProfileId,
) -> Result<PendingAccountBrowserLogin, AccountLoginFlowError> {
    let profile = existing_profile(&store, profile_id)?;
    let auth = AuthPersistenceConfig {
        auth_credentials_store_mode: options.cli_auth_credentials_store_mode,
        auth_keyring_backend_kind: options.auth_keyring_backend_kind,
    };
    let relogin = ReloginStaging::new(
        store.codex_home(),
        &profile,
        auth.auth_credentials_store_mode,
        auth.auth_keyring_backend_kind,
    )?;
    options.codex_home = relogin.home().to_path_buf();
    options.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;

    let server = run_login_server(options)?;
    Ok(PendingAccountBrowserLogin {
        store,
        profile,
        server: Some(server),
        mode: AccountLoginMode::Relogin,
        relogin: Some(relogin),
        auth,
    })
}

/// Whether a login flow owns a freshly allocated profile or repairs an existing one.
///
/// A new profile is abandoned (metadata and credential directory removed) when its first login
/// fails; an existing profile always survives a failed re-login with its stored state intact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccountLoginMode {
    NewProfile,
    Relogin,
}

fn existing_profile(
    store: &AccountProfileStore,
    profile_id: &AccountProfileId,
) -> Result<AccountProfile, AccountLoginFlowError> {
    store
        .load_profile_records()?
        .into_iter()
        .map(|record| record.profile)
        .find(|profile| &profile.id == profile_id)
        .ok_or_else(|| {
            AccountLoginFlowError::Store(AccountProfileStoreError::UnknownProfile(
                profile_id.clone(),
            ))
        })
}

pub struct PendingAccountBrowserLogin {
    store: AccountProfileStore,
    profile: AccountProfile,
    server: Option<LoginServer>,
    mode: AccountLoginMode,
    auth: AuthPersistenceConfig,
    relogin: Option<ReloginStaging>,
}

impl PendingAccountBrowserLogin {
    pub fn profile(&self) -> &AccountProfile {
        &self.profile
    }

    pub fn auth_url(&self) -> Option<&str> {
        self.server.as_ref().map(|server| server.auth_url.as_str())
    }

    pub fn actual_port(&self) -> Option<u16> {
        self.server.as_ref().map(|server| server.actual_port)
    }

    /// Waits for the existing Codex OAuth server to finish and then makes the profile schedulable.
    ///
    /// If OAuth fails, the still-pending profile is removed. If OAuth succeeds but manifest
    /// promotion fails, the credential directory is deliberately preserved in `pending_login`
    /// state so credentials are recoverable instead of being deleted after a successful login.
    pub async fn complete(mut self) -> Result<AccountLoginOutcome, AccountLoginFlowError> {
        let server = self
            .server
            .take()
            .ok_or(AccountLoginFlowError::FlowAlreadyConsumed)?;
        match server.block_until_done().await {
            Ok(()) => {
                finish_successful_login(
                    self.store.clone(),
                    self.profile.clone(),
                    self.mode,
                    self.auth,
                    self.relogin.take(),
                )
                .await
            }
            Err(error) => {
                if self.mode == AccountLoginMode::NewProfile {
                    abandon_after_failed_login(&self.store, &self.profile.id, self.auth);
                }
                Err(error.into())
            }
        }
    }

    /// Cancels the running callback server, waits for it to exit, and removes a pending
    /// newly-allocated profile. An existing profile being re-logged-in is left untouched.
    pub async fn cancel(mut self) -> Result<(), AccountLoginFlowError> {
        let server = self
            .server
            .take()
            .ok_or(AccountLoginFlowError::FlowAlreadyConsumed)?;
        server.cancel();
        let _ = server.block_until_done().await;
        if self.mode == AccountLoginMode::NewProfile {
            abandon_pending_login(
                &self.store,
                &self.profile.id,
                self.auth.auth_credentials_store_mode,
                self.auth.auth_keyring_backend_kind,
            )?;
        }
        Ok(())
    }
}

impl Drop for PendingAccountBrowserLogin {
    fn drop(&mut self) {
        // Drop cannot await the callback task, so only request shutdown here. Leaving the profile
        // as pending is safer than racing token persistence with recursive credential deletion.
        if let Some(server) = self.server.take() {
            server.cancel();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let staging = self.relogin.take();
                runtime.spawn(async move {
                    let _ = server.block_until_done().await;
                    drop(staging);
                });
            }
        }
    }
}

fn abandon_after_failed_login(
    store: &AccountProfileStore,
    profile_id: &AccountProfileId,
    auth: AuthPersistenceConfig,
) {
    if let Err(error) = abandon_pending_login(
        store,
        profile_id,
        auth.auth_credentials_store_mode,
        auth.auth_keyring_backend_kind,
    ) {
        tracing::warn!(
            profile_id = %profile_id,
            error = %error,
            "failed to clean up pending account profile after login failure"
        );
    }
}

async fn finish_successful_login(
    store: AccountProfileStore,
    profile: AccountProfile,
    mode: AccountLoginMode,
    auth: AuthPersistenceConfig,
    relogin: Option<ReloginStaging>,
) -> Result<AccountLoginOutcome, AccountLoginFlowError> {
    tokio::task::spawn_blocking(move || {
        let mut transaction = store.enrollment_transaction()?;
        // Recheck the profile under the manifest lock before writing credentials. A cancelled or
        // removed enrollment must not be recreated by a late OAuth completion.
        let profile = transaction
            .records()?
            .into_iter()
            .find(|record| record.profile.id == profile.id)
            .map(|record| record.profile)
            .ok_or_else(|| AccountProfileStoreError::UnknownProfile(profile.id.clone()))?;
        if let Some(relogin) = relogin.as_ref() {
            // Validate before committing so a wrong workspace never replaces the profile's auth.
            relogin.commit(
                &profile,
                auth.auth_credentials_store_mode,
                auth.auth_keyring_backend_kind,
            )?;
        }
        if mode == AccountLoginMode::NewProfile
            && let Some(existing) = reconcile_duplicate_login(
                &mut transaction,
                &profile,
                auth.auth_credentials_store_mode,
                auth.auth_keyring_backend_kind,
            )?
        {
            return Ok(AccountLoginOutcome {
                profile: existing,
                kind: AccountLoginOutcomeKind::RefreshedExistingDuplicate,
            });
        }

        let profile = transaction.complete_profile(&profile.id)?;
        Ok(AccountLoginOutcome {
            profile,
            kind: AccountLoginOutcomeKind::Added,
        })
    })
    .await
    .map_err(|error| io::Error::other(format!("account login commit task failed: {error}")))?
}

#[cfg(test)]
#[path = "account_login_completion_tests.rs"]
mod completion_tests;

#[derive(Debug, Error)]
pub enum AccountLoginFlowError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Store(#[from] AccountProfileStoreError),
    #[error("account login flow was already consumed")]
    FlowAlreadyConsumed,
    #[error("Login cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use codex_config::types::AuthCredentialsStoreMode;

    use super::*;
    use crate::AuthKeyringBackendKind;
    use crate::CLIENT_ID;

    #[test]
    fn account_login_uses_final_profile_credential_home() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let store = AccountProfileStore::new(temp.path().join("codex"));
        let profile = store
            .allocate_profile(Some("second account".to_string()), 10)
            .expect("allocate profile");
        let mut options = ServerOptions::new(
            PathBuf::from("original-home"),
            CLIENT_ID.to_string(),
            None,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
            crate::test_support::transport_default_auth_route_config(),
        );

        options.codex_home = profile.credential_home.clone();
        assert_eq!(options.codex_home, profile.credential_home);
    }
}
