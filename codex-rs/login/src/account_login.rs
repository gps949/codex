use std::io;

use thiserror::Error;

use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AccountProfileStore;
use crate::AccountProfileStoreError;
use crate::DeviceCode;
use crate::LoginServer;
use crate::ServerOptions;
use crate::complete_device_code_login;
use crate::request_device_code;
use crate::run_login_server;

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

    match run_login_server(options) {
        Ok(server) => Ok(PendingAccountBrowserLogin {
            store,
            profile,
            server: Some(server),
            mode: AccountLoginMode::NewProfile,
        }),
        Err(error) => {
            abandon_after_failed_login(&store, &profile.id);
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
    options.codex_home = profile.credential_home.clone();

    let server = run_login_server(options)?;
    Ok(PendingAccountBrowserLogin {
        store,
        profile,
        server: Some(server),
        mode: AccountLoginMode::Relogin,
    })
}

/// Starts the official device-code flow for a new account profile.
///
/// No token exchange is performed until [`PendingAccountDeviceLogin::complete`] is called. As with
/// browser login, the profile-specific credential home is selected before the first auth request.
pub async fn begin_account_device_login(
    store: AccountProfileStore,
    mut options: ServerOptions,
    label: Option<String>,
    priority: u32,
) -> Result<PendingAccountDeviceLogin, AccountLoginFlowError> {
    let profile = store.allocate_profile(label, priority)?;
    options.codex_home = profile.credential_home.clone();

    match request_device_code(&options).await {
        Ok(device_code) => Ok(PendingAccountDeviceLogin {
            store,
            profile,
            options: Some(options),
            device_code: Some(device_code),
            mode: AccountLoginMode::NewProfile,
        }),
        Err(error) => {
            abandon_after_failed_login(&store, &profile.id);
            Err(error.into())
        }
    }
}

/// Device-code variant of [`begin_account_browser_relogin`].
pub async fn begin_account_device_relogin(
    store: AccountProfileStore,
    mut options: ServerOptions,
    profile_id: &AccountProfileId,
) -> Result<PendingAccountDeviceLogin, AccountLoginFlowError> {
    let profile = existing_profile(&store, profile_id)?;
    options.codex_home = profile.credential_home.clone();

    let device_code = request_device_code(&options).await?;
    Ok(PendingAccountDeviceLogin {
        store,
        profile,
        options: Some(options),
        device_code: Some(device_code),
        mode: AccountLoginMode::Relogin,
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
    pub async fn complete(mut self) -> Result<AccountProfile, AccountLoginFlowError> {
        let server = self
            .server
            .take()
            .ok_or(AccountLoginFlowError::FlowAlreadyConsumed)?;
        match server.block_until_done().await {
            Ok(()) => self
                .store
                .complete_profile(&self.profile.id)
                .map_err(AccountLoginFlowError::from),
            Err(error) => {
                if self.mode == AccountLoginMode::NewProfile {
                    abandon_after_failed_login(&self.store, &self.profile.id);
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
            self.store.abandon_pending_profile(&self.profile.id)?;
        }
        Ok(())
    }
}

impl Drop for PendingAccountBrowserLogin {
    fn drop(&mut self) {
        // Drop cannot await the callback task, so only request shutdown here. Leaving the profile
        // as pending is safer than racing token persistence with recursive credential deletion.
        if let Some(server) = self.server.as_ref() {
            server.cancel();
        }
    }
}

pub struct PendingAccountDeviceLogin {
    store: AccountProfileStore,
    profile: AccountProfile,
    options: Option<ServerOptions>,
    device_code: Option<DeviceCode>,
    mode: AccountLoginMode,
}

impl PendingAccountDeviceLogin {
    pub fn profile(&self) -> &AccountProfile {
        &self.profile
    }

    pub fn verification_url(&self) -> Option<&str> {
        self.device_code
            .as_ref()
            .map(|device_code| device_code.verification_url.as_str())
    }

    pub fn user_code(&self) -> Option<&str> {
        self.device_code
            .as_ref()
            .map(|device_code| device_code.user_code.as_str())
    }

    pub async fn complete(mut self) -> Result<AccountProfile, AccountLoginFlowError> {
        let options = self
            .options
            .take()
            .ok_or(AccountLoginFlowError::FlowAlreadyConsumed)?;
        let device_code = self
            .device_code
            .take()
            .ok_or(AccountLoginFlowError::FlowAlreadyConsumed)?;

        match complete_device_code_login(options, device_code).await {
            Ok(()) => self
                .store
                .complete_profile(&self.profile.id)
                .map_err(AccountLoginFlowError::from),
            Err(error) => {
                if self.mode == AccountLoginMode::NewProfile {
                    abandon_after_failed_login(&self.store, &self.profile.id);
                }
                Err(error.into())
            }
        }
    }

    pub fn cancel(mut self) -> Result<(), AccountLoginFlowError> {
        self.options.take();
        self.device_code.take();
        if self.mode == AccountLoginMode::NewProfile {
            self.store.abandon_pending_profile(&self.profile.id)?;
        }
        Ok(())
    }
}

fn abandon_after_failed_login(store: &AccountProfileStore, profile_id: &AccountProfileId) {
    if let Err(error) = store.abandon_pending_profile(profile_id) {
        tracing::warn!(
            profile_id = %profile_id,
            error = %error,
            "failed to clean up pending account profile after login failure"
        );
    }
}

#[derive(Debug, Error)]
pub enum AccountLoginFlowError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Store(#[from] AccountProfileStoreError),
    #[error("account login flow was already consumed")]
    FlowAlreadyConsumed,
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
