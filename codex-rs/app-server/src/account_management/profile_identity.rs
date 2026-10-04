//! Local identity evidence for confirmations without exposing credential material.

use super::AccountManager;
use sha2::Digest;
use sha2::Sha256;

impl AccountManager {
    /// Returns an opaque user-and-workspace binding for one persisted subscription profile.
    /// Confirmation callers must compare it again immediately before a target-specific action.
    pub(crate) fn profile_identity(&self, id: &str) -> anyhow::Result<String> {
        let profile = self.profile(id)?;
        let identity = codex_login::account_identity::load_login_identity(
            &profile.profile.credential_home,
            self.config.cli_auth_credentials_store_mode,
            self.config.auth_keyring_backend_kind(),
        )?
        .ok_or_else(|| anyhow::anyhow!("Account has no persisted subscription identity"))?;
        let workspace = identity
            .chatgpt_account_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Account does not identify its workspace"))?;
        let mut digest = Sha256::new();
        digest.update(b"codex-native-account-confirmation-v1\0");
        for value in [identity.chatgpt_user_id.as_str(), workspace] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        }
        Ok(format!("{:x}", digest.finalize()))
    }
}
