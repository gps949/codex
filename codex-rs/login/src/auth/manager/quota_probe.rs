//! Checks request credentials against the profile's actual storage before confirming quota.

use std::path::Path;
use std::sync::atomic::Ordering;

use super::AuthCredentialsStoreMode;
use super::AuthKeyringBackendKind;
use super::AuthManager;
use super::CodexAuth;
use super::CredentialSource;
use super::load_auth_dot_json;

impl AuthManager {
    /// The caller holds this credential home's refresh lock through capture or commit.
    /// Match token data instead of CodexAuth equality, which only compares ChatGPT auth modes.
    pub(crate) fn stored_quota_probe_auth_matches(
        &self,
        credential_home: &Path,
        expected: &CodexAuth,
    ) -> std::io::Result<bool> {
        if credential_home != self.codex_home.as_path()
            || self.pool_suspended.load(Ordering::Acquire)
        {
            return Ok(false);
        }
        let Some(expected_tokens) = expected.get_current_token_data() else {
            return Ok(false);
        };
        let cached = self
            .inner
            .read()
            .map_err(|_| std::io::Error::other("auth state is unavailable"))?;
        if !Self::auths_equal_for_refresh(cached.auth.as_ref(), Some(expected)) {
            return Ok(false);
        }
        // Match the manager's reload source. A process-local overlay cannot replace or veto a
        // persisted subscription seat; standard managers keep their existing precedence.
        let stored = match self.credential_source {
            CredentialSource::ManagedProfile => load_auth_dot_json(
                &self.codex_home,
                self.auth_credentials_store_mode,
                self.keyring_backend_kind,
            )?,
            CredentialSource::Standard => match load_auth_dot_json(
                &self.codex_home,
                AuthCredentialsStoreMode::Ephemeral,
                AuthKeyringBackendKind::default(),
            )? {
                Some(stored) => Some(stored),
                None => load_auth_dot_json(
                    &self.codex_home,
                    self.auth_credentials_store_mode,
                    self.keyring_backend_kind,
                )?,
            },
        };
        Ok(stored.is_some_and(|stored| {
            stored.resolved_mode() == expected.api_auth_mode()
                && stored.tokens.as_ref() == Some(&expected_tokens)
        }))
    }
}
