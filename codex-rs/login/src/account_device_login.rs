//! Device-code enrollment stages credentials and joins persistence before cancellation cleanup.

use std::future::Future;
use std::io;

use codex_config::types::AuthCredentialsStoreMode;

use super::AccountLoginFlowError;
use super::AccountLoginMode;
use super::AccountLoginOutcome;
use super::AuthPersistenceConfig;
use super::abandon_after_failed_login;
use super::existing_profile;
use super::finish_successful_login;
use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AccountProfileStore;
use crate::DeviceCode;
use crate::ServerOptions;
use crate::account_identity::abandon_pending_login;
use crate::account_relogin::ReloginStaging;
use crate::device_code_auth::complete_device_code_login_with_cancellation;
use crate::request_device_code;

/// Allocates a pending profile before requesting authorization from the issuer.
///
/// Managers can reserve the returned profile and a login slot before any network await.
/// Device credentials are staged separately until the enrollment transaction promotes them.
pub fn prepare_account_device_login(
    store: AccountProfileStore,
    mut options: ServerOptions,
    label: Option<String>,
    priority: u32,
) -> Result<PendingAccountDeviceLogin, AccountLoginFlowError> {
    let profile = store.allocate_profile(label, priority)?;
    let auth = AuthPersistenceConfig {
        auth_credentials_store_mode: options.cli_auth_credentials_store_mode,
        auth_keyring_backend_kind: options.auth_keyring_backend_kind,
    };
    let staging = match ReloginStaging::new(
        store.codex_home(),
        &profile,
        auth.auth_credentials_store_mode,
        auth.auth_keyring_backend_kind,
    ) {
        Ok(staging) => staging,
        Err(error) => {
            abandon_after_failed_login(&store, &profile.id, auth);
            return Err(error.into());
        }
    };
    options.codex_home = staging.home().to_path_buf();
    options.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
    Ok(PendingAccountDeviceLogin {
        store,
        profile,
        options: Some(options),
        device_code: None,
        mode: AccountLoginMode::NewProfile,
        relogin: Some(staging),
        auth,
    })
}

/// Prepares an identity-preserving device-code login for an existing profile.
pub fn prepare_account_device_relogin(
    store: AccountProfileStore,
    mut options: ServerOptions,
    profile_id: &AccountProfileId,
) -> Result<PendingAccountDeviceLogin, AccountLoginFlowError> {
    let profile = existing_profile(&store, profile_id)?;
    let auth = AuthPersistenceConfig {
        auth_credentials_store_mode: options.cli_auth_credentials_store_mode,
        auth_keyring_backend_kind: options.auth_keyring_backend_kind,
    };
    let staging = ReloginStaging::new(
        store.codex_home(),
        &profile,
        auth.auth_credentials_store_mode,
        auth.auth_keyring_backend_kind,
    )?;
    options.codex_home = staging.home().to_path_buf();
    options.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
    Ok(PendingAccountDeviceLogin {
        store,
        profile,
        options: Some(options),
        device_code: None,
        mode: AccountLoginMode::Relogin,
        relogin: Some(staging),
        auth,
    })
}

/// Starts the official device-code flow for a new profile with separately staged credentials.
pub async fn begin_account_device_login(
    store: AccountProfileStore,
    options: ServerOptions,
    label: Option<String>,
    priority: u32,
) -> Result<PendingAccountDeviceLogin, AccountLoginFlowError> {
    prepare_account_device_login(store, options, label, priority)?
        .request_code_with_cancellation(std::future::pending())
        .await
}

/// Device-code variant of [`super::begin_account_browser_relogin`].
pub async fn begin_account_device_relogin(
    store: AccountProfileStore,
    options: ServerOptions,
    profile_id: &AccountProfileId,
) -> Result<PendingAccountDeviceLogin, AccountLoginFlowError> {
    prepare_account_device_relogin(store, options, profile_id)?
        .request_code_with_cancellation(std::future::pending())
        .await
}

pub struct PendingAccountDeviceLogin {
    store: AccountProfileStore,
    profile: AccountProfile,
    options: Option<ServerOptions>,
    device_code: Option<DeviceCode>,
    mode: AccountLoginMode,
    relogin: Option<ReloginStaging>,
    auth: AuthPersistenceConfig,
}

impl PendingAccountDeviceLogin {
    pub fn profile(&self) -> &AccountProfile {
        &self.profile
    }

    pub fn verification_url(&self) -> Option<&str> {
        self.device_code
            .as_ref()
            .map(|code| code.verification_url.as_str())
    }

    pub fn user_code(&self) -> Option<&str> {
        self.device_code
            .as_ref()
            .map(|code| code.user_code.as_str())
    }

    /// Requests the browser code, removing a new pending enrollment if cancelled or failed.
    pub async fn request_code_with_cancellation(
        mut self,
        cancellation: impl Future<Output = ()> + Send,
    ) -> Result<Self, AccountLoginFlowError> {
        let options = self
            .options
            .as_ref()
            .ok_or(AccountLoginFlowError::FlowAlreadyConsumed)?;
        let result = tokio::select! {
            biased;
            _ = cancellation => Err(AccountLoginFlowError::Cancelled),
            result = request_device_code(options) => result.map_err(AccountLoginFlowError::Io),
        };
        match result {
            Ok(code) => {
                self.device_code = Some(code);
                Ok(self)
            }
            Err(error) => {
                if self.mode == AccountLoginMode::NewProfile {
                    abandon_after_failed_login(&self.store, &self.profile.id, self.auth);
                }
                Err(error)
            }
        }
    }

    pub async fn complete(self) -> Result<AccountLoginOutcome, AccountLoginFlowError> {
        self.complete_with_cancellation(std::future::pending())
            .await
    }

    /// Cancels network waits, then joins any started credential persistence before cleanup.
    ///
    /// Callers must await this method to quiescence instead of dropping it in a `select!`.
    /// Once the enrollment commit begins, it runs to completion and a successful commit wins
    /// over late cancellation, so the caller never reports a committed login as cancelled.
    pub async fn complete_with_cancellation(
        mut self,
        cancellation: impl Future<Output = ()> + Send,
    ) -> Result<AccountLoginOutcome, AccountLoginFlowError> {
        let options = self
            .options
            .take()
            .ok_or(AccountLoginFlowError::FlowAlreadyConsumed)?;
        let code = self
            .device_code
            .take()
            .ok_or(AccountLoginFlowError::FlowAlreadyConsumed)?;
        tokio::pin!(cancellation);
        let result =
            complete_device_code_login_with_cancellation(options, code, &mut cancellation).await;
        if let Err(error) = result {
            if self.mode == AccountLoginMode::NewProfile {
                abandon_after_failed_login(&self.store, &self.profile.id, self.auth);
            }
            return if error.kind() == io::ErrorKind::Interrupted {
                Err(AccountLoginFlowError::Cancelled)
            } else {
                Err(error.into())
            };
        }
        let cancelled = tokio::select! {
            biased;
            _ = &mut cancellation => true,
            _ = std::future::ready(()) => false,
        };
        if cancelled {
            self.cancel()?;
            return Err(AccountLoginFlowError::Cancelled);
        }
        finish_successful_login(
            self.store.clone(),
            self.profile.clone(),
            self.mode,
            self.auth,
            self.relogin.take(),
        )
        .await
    }

    pub fn cancel(mut self) -> Result<(), AccountLoginFlowError> {
        self.options.take();
        self.device_code.take();
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

#[cfg(test)]
#[path = "account_device_login_tests.rs"]
mod tests;
