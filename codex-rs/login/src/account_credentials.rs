//! Transactions and change notifications for account-profile login credentials.

use std::io;
use std::path::Path;

use codex_config::types::AuthCredentialsStoreMode;

use crate::auth::AuthDotJson;
use crate::auth::AuthKeyringBackendKind;

const CREDENTIAL_VERSION_FILE: &str = ".account-credentials-version";

/// Commits a completed login after any in-flight refresh has finished. The
/// marker lets running pools notice repaired credentials without polling secrets.
pub(crate) fn save_login_auth(
    home: &Path,
    auth: &AuthDotJson,
    store_mode: AuthCredentialsStoreMode,
    keyring_backend: AuthKeyringBackendKind,
) -> io::Result<()> {
    let _lock = crate::account_file::refresh_lock(home)?;
    crate::auth::save_auth(home, auth, store_mode, keyring_backend)?;
    let version = format!("{:032x}", rand::random::<u128>());
    std::fs::write(home.join(CREDENTIAL_VERSION_FILE), version)
}

pub(crate) fn credential_version(home: &Path) -> Option<String> {
    let path = home.join(CREDENTIAL_VERSION_FILE);
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > 128 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}
