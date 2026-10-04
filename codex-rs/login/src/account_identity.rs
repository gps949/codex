use std::path::Path;

use codex_config::types::AuthCredentialsStoreMode;

use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AccountProfileState;
use crate::AccountProfileStore;
use crate::AccountProfileStoreError;
use crate::account_store::AccountProfileEnrollment;
use crate::auth::AuthDotJson;
use crate::auth::AuthKeyringBackendKind;
use crate::auth::load_auth_dot_json;

/// Stable ChatGPT seat identity used to detect duplicate account-pool logins.
///
/// `chatgpt_user_id` identifies the person. `chatgpt_account_id` (when present) identifies the
/// workspace/seat so the same person can keep separate personal and work profiles without one
/// login refreshing and overwriting the other. Two Business seats in one workspace still stay
/// distinct because they have different user ids.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountLoginIdentity {
    pub chatgpt_user_id: String,
    pub chatgpt_account_id: Option<String>,
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
    crate::primary_login::validate_stored_mode(auth).ok()?;
    let tokens = auth.tokens.as_ref()?;
    let chatgpt_user_id = tokens
        .id_token
        .chatgpt_user_id
        .clone()
        .filter(|user_id| !user_id.trim().is_empty())?;
    let chatgpt_account_id = tokens
        .account_id
        .clone()
        .or_else(|| tokens.id_token.chatgpt_account_id.clone())
        .filter(|account_id| !account_id.trim().is_empty());
    let email = tokens.id_token.email.clone();
    Some(AccountLoginIdentity {
        chatgpt_user_id,
        chatgpt_account_id,
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
    find_matching_profile(
        store.load_profile_records()?,
        exclude_profile_id,
        identity,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    )
}

fn find_matching_profile(
    records: Vec<crate::AccountProfileRecord>,
    exclude_profile_id: &AccountProfileId,
    identity: &AccountLoginIdentity,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
) -> Result<Option<AccountProfile>, AccountProfileStoreError> {
    for record in records {
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
        if existing_identity.chatgpt_user_id == identity.chatgpt_user_id
            && existing_identity.chatgpt_account_id == identity.chatgpt_account_id
        {
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
    crate::account_credentials::save_login_auth(
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
    let mut transaction = store.enrollment_transaction()?;
    reconcile_duplicate_login(
        &mut transaction,
        new_profile,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    )
}

/// Reconciles a completed new login while the caller retains the transaction through promotion.
pub(crate) fn reconcile_duplicate_login(
    transaction: &mut AccountProfileEnrollment<'_>,
    new_profile: &AccountProfile,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
) -> Result<Option<AccountProfile>, AccountProfileStoreError> {
    let records = transaction.records()?;
    let record = records
        .iter()
        .find(|record| record.profile.id == new_profile.id)
        .ok_or_else(|| AccountProfileStoreError::UnknownProfile(new_profile.id.clone()))?;
    if record.state != AccountProfileState::PendingLogin {
        return Err(AccountProfileStoreError::ProfileNotPending(
            new_profile.id.clone(),
        ));
    }
    let Some(identity) = load_login_identity(
        &new_profile.credential_home,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    )
    .map_err(AccountProfileStoreError::Io)?
    else {
        return Ok(None);
    };

    let Some(existing) = find_matching_profile(
        records,
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
    transaction.abandon_pending_profile(&new_profile.id, |home| {
        remove_pending_credentials(home, auth_credentials_store_mode, auth_keyring_backend_kind)
    })?;
    Ok(Some(existing))
}

/// Deletes credentials and metadata for an unfinished login without revoking its OAuth token.
/// Reconciliation may have copied the same token into the retained profile. Failed cleanup keeps
/// the pending profile visible so cancellation can be retried.
pub fn abandon_pending_login(
    store: &AccountProfileStore,
    profile_id: &AccountProfileId,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
) -> Result<bool, AccountProfileStoreError> {
    store
        .enrollment_transaction()?
        .abandon_pending_profile(profile_id, |home| {
            remove_pending_credentials(home, auth_credentials_store_mode, auth_keyring_backend_kind)
        })
}

fn remove_pending_credentials(
    home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let _refresh_lock = crate::account_file::refresh_lock(home)?;
    crate::auth::logout(home, auth_credentials_store_mode, auth_keyring_backend_kind)?;
    if auth_credentials_store_mode != AuthCredentialsStoreMode::Ephemeral {
        crate::auth::logout(
            home,
            AuthCredentialsStoreMode::Ephemeral,
            auth_keyring_backend_kind,
        )?;
    }
    if auth_credentials_store_mode != AuthCredentialsStoreMode::File {
        crate::auth::logout(
            home,
            AuthCredentialsStoreMode::File,
            auth_keyring_backend_kind,
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "account_identity_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "account_identity_keyring_tests.rs"]
mod keyring_tests;
