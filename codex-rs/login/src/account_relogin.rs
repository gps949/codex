//! Stages re-login separately so a wrong browser identity cannot replace a seat.

use std::io;
use std::path::Path;
use std::path::PathBuf;

use codex_config::types::AuthCredentialsStoreMode;

use crate::AccountLoginIdentity;
use crate::AccountProfile;
use crate::account_identity::load_login_identity;
use crate::account_identity::login_identity_from_auth;
use crate::auth::AuthKeyringBackendKind;
use crate::auth::load_auth_dot_json;

pub(crate) struct ReloginStaging {
    home: PathBuf,
    expected_identity: Option<AccountLoginIdentity>,
}

impl ReloginStaging {
    pub(crate) fn new(
        codex_home: &Path,
        profile: &AccountProfile,
        store_mode: AuthCredentialsStoreMode,
        keyring_backend: AuthKeyringBackendKind,
    ) -> io::Result<Self> {
        let expected_identity =
            load_login_identity(&profile.credential_home, store_mode, keyring_backend)?;
        let home = codex_home.join(format!(".account-relogin-{:032x}", rand::random::<u128>()));
        std::fs::create_dir_all(&home)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            home,
            expected_identity,
        })
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub(crate) fn commit(
        &self,
        profile: &AccountProfile,
        store_mode: AuthCredentialsStoreMode,
        keyring_backend: AuthKeyringBackendKind,
    ) -> io::Result<()> {
        let auth = load_auth_dot_json(&self.home, AuthCredentialsStoreMode::File, keyring_backend)?
            .ok_or_else(|| io::Error::other("re-login completed without stored credentials"))?;
        let identity = login_identity_from_auth(&auth)
            .ok_or_else(|| io::Error::other("re-login did not return a ChatGPT seat identity"))?;
        if self.expected_identity.as_ref().is_some_and(|expected| {
            expected.chatgpt_user_id != identity.chatgpt_user_id
                || expected.chatgpt_account_id != identity.chatgpt_account_id
        }) {
            return Err(io::Error::other(
                "Re-login selected a different account or workspace. Your existing account was kept. Use `codex account add` to add the other account.",
            ));
        }
        crate::account_credentials::save_login_auth(
            &profile.credential_home,
            &auth,
            store_mode,
            keyring_backend,
        )
    }
}

impl Drop for ReloginStaging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[cfg(test)]
#[path = "account_relogin_tests.rs"]
mod tests;
