use std::path::Path;

use codex_config::types::AuthCredentialsStoreMode;

use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AccountProfileState;
use crate::AccountProfileStore;
use crate::AccountProfileStoreError;
use crate::auth::AuthDotJson;
use crate::auth::AuthKeyringBackendKind;
use crate::auth::load_auth_dot_json;
use crate::auth::save_auth;

/// Stable ChatGPT user identity used to detect duplicate account-pool logins.
///
/// `chatgpt_user_id` distinguishes individual seats/users. Workspace-level identifiers such as
/// `tokens.account_id` or `id_token.chatgpt_account_id` are intentionally ignored because multiple
/// Business seats in the same workspace share them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountLoginIdentity {
    pub chatgpt_user_id: String,
    pub email: Option<String>,
}

/// Loads the ChatGPT user identity stored in a profile credential home.
pub fn load_login_identity(
    credential_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
) -> Result<Option<AccountLoginIdentity>, std::io::Error> {
    let auth = load_auth_dot_json(
        credential_home,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    )?;
    Ok(auth.as_ref().and_then(login_identity_from_auth))
}

pub(crate) fn login_identity_from_auth(auth: &AuthDotJson) -> Option<AccountLoginIdentity> {
    let chatgpt_user_id = auth
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.id_token.chatgpt_user_id.clone())
        .filter(|user_id| !user_id.trim().is_empty())?;
    let email = auth
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.id_token.email.clone());
    Some(AccountLoginIdentity {
        chatgpt_user_id,
        email,
    })
}

/// Finds an already-schedulable profile for the same ChatGPT user.
pub fn find_existing_profile_with_identity(
    store: &AccountProfileStore,
    exclude_profile_id: &AccountProfileId,
    identity: &AccountLoginIdentity,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
) -> Result<Option<AccountProfile>, AccountProfileStoreError> {
    for record in store.load_profile_records()? {
        if &record.profile.id == exclude_profile_id || record.state != AccountProfileState::Ready {
            continue;
        }
        let Some(existing_identity) = load_login_identity(
            &record.profile.credential_home,
            auth_credentials_store_mode,
            auth_keyring_backend_kind,
        )
        .map_err(AccountProfileStoreError::Io)?
        else {
            continue;
        };
        if existing_identity.chatgpt_user_id == identity.chatgpt_user_id {
            return Ok(Some(record.profile));
        }
    }
    Ok(None)
}

/// Copies freshly persisted OAuth credentials into an existing profile home.
pub fn copy_login_credentials(
    from_home: &Path,
    to_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
) -> Result<(), std::io::Error> {
    let auth = load_auth_dot_json(
        from_home,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    )?
    .ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "login completed without persisted credentials",
        )
    })?;
    save_auth(
        to_home,
        &auth,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    )
}

/// When a new profile login resolves to an existing ChatGPT user, refresh that profile and drop
/// the duplicate pending allocation.
pub fn reconcile_duplicate_new_login(
    store: &AccountProfileStore,
    new_profile: &AccountProfile,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
) -> Result<Option<AccountProfile>, AccountProfileStoreError> {
    let Some(identity) = load_login_identity(
        &new_profile.credential_home,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    )
    .map_err(AccountProfileStoreError::Io)?
    else {
        return Ok(None);
    };

    let Some(existing) = find_existing_profile_with_identity(
        store,
        &new_profile.id,
        &identity,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    )?
    else {
        return Ok(None);
    };

    copy_login_credentials(
        &new_profile.credential_home,
        &existing.credential_home,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    )
    .map_err(AccountProfileStoreError::Io)?;
    store.abandon_pending_profile(&new_profile.id)?;
    Ok(Some(existing))
}

#[cfg(test)]
#[path = "account_identity_tests.rs"]
mod tests;
