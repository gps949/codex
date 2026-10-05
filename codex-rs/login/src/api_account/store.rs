use super::*;
use crate::AuthDotJson;
use crate::AuthKeyringBackendKind;
use codex_config::types::AuthCredentialsStoreMode;
use std::path::PathBuf;

/// Stores non-secret metadata atomically and keys in the existing credential storage.
pub struct ApiAccountStore {
    home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring: AuthKeyringBackendKind,
}

impl ApiAccountStore {
    pub fn new(
        home: PathBuf,
        mode: AuthCredentialsStoreMode,
        keyring: AuthKeyringBackendKind,
    ) -> Self {
        Self {
            home,
            mode,
            keyring,
        }
    }

    pub fn credential_home(&self, id: &str) -> std::io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(std::io::Error::other("Invalid API account ID"));
        }
        Ok(self.home.join("api-auth-profiles").join(id))
    }

    pub fn load(&self) -> std::io::Result<ApiAccountState> {
        let _lock = crate::account_file::lock(&self.home)?;
        self.read()
    }

    /// Captures all account descriptions and their exact key revisions in one transaction.
    /// Keys stay in this store; the snapshot contains only non-secret fingerprints.
    pub fn management_snapshot(&self) -> std::io::Result<ApiAccountInventory> {
        let _lock = crate::account_file::lock(&self.home)?;
        let state = self.read()?;
        let mut credential_revisions = BTreeMap::new();
        for account in &state.accounts {
            if let Some(revision) = self.revision_for_target(account)? {
                credential_revisions.insert(account.id.clone(), revision);
            }
        }
        Ok(ApiAccountInventory {
            state,
            credential_revisions,
        })
    }

    /// Reads metadata only when the account transaction lock is immediately available.
    pub fn try_load(&self) -> std::io::Result<Option<ApiAccountState>> {
        let Some(_lock) = crate::account_file::try_lock(&self.home)? else {
            return Ok(None);
        };
        self.read().map(Some)
    }

    /// Captures deployment metadata and its key in the same account transaction.
    /// The returned key must stay in request-owned memory and never be logged or persisted.
    pub fn capture_target(&self, id: &str) -> std::io::Result<(ApiAccount, String)> {
        let _lock = crate::account_file::lock(&self.home)?;
        let state = self.read()?;
        let account = state
            .accounts
            .into_iter()
            .find(|account| account.id == id && !account.disabled)
            .ok_or_else(|| std::io::Error::other("The selected API account is unavailable"))?;
        account.validate()?;
        let key = crate::load_auth_dot_json(&self.credential_home(id)?, self.mode, self.keyring)?
            .and_then(|auth| auth.openai_api_key)
            .filter(|key| !key.trim().is_empty() && !key.contains(['\r', '\n']))
            .ok_or_else(|| std::io::Error::other("The selected API account needs an API key"))?;
        Ok((account, key))
    }

    fn read(&self) -> std::io::Result<ApiAccountState> {
        match std::fs::read(self.home.join("api-accounts.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ApiAccountState::default())
            }
            Err(error) => Err(error),
        }
    }

    fn write(&self, state: &ApiAccountState) -> std::io::Result<()> {
        let path = self.home.join("api-accounts.json");
        let temporary = self
            .home
            .join(format!(".api-accounts-{}.tmp", uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
        std::fs::write(&temporary, bytes)?;
        if let Err(error) = std::fs::rename(&temporary, &path) {
            if cfg!(windows) && path.exists() {
                std::fs::remove_file(&path)?;
                std::fs::rename(&temporary, &path)?;
            } else {
                let _ = std::fs::remove_file(&temporary);
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn add(&self, mut account: ApiAccount, key: &str) -> std::io::Result<ApiAccount> {
        account.validate()?;
        if key.trim().is_empty() || key.len() > 16_384 || key.contains(['\r', '\n']) {
            return Err(std::io::Error::other("Invalid API key"));
        }
        let _lock = crate::account_file::lock(&self.home)?;
        let mut state = self.read()?;
        if state.accounts.len() >= 64 {
            return Err(std::io::Error::other("API account limit reached"));
        }
        account.id = format!("api-{}", uuid::Uuid::new_v4().simple());
        let home = self.credential_home(&account.id)?;
        std::fs::create_dir_all(&home)?;
        let auth = AuthDotJson {
            auth_mode: Some(codex_protocol::auth::AuthMode::ApiKey),
            openai_api_key: Some(key.into()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        };
        crate::save_auth(&home, &auth, self.mode, self.keyring)?;
        state.accounts.push(account.clone());
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("API account revision overflow"))?;
        if let Err(error) = self.write(&state) {
            let _ = crate::logout(&home, self.mode, self.keyring);
            let _ = std::fs::remove_dir_all(&home);
            return Err(error);
        }
        Ok(account)
    }

    pub fn update(&self, account: ApiAccount) -> std::io::Result<()> {
        account.validate()?;
        let _lock = crate::account_file::lock(&self.home)?;
        let mut state = self.read()?;
        let old = state
            .accounts
            .iter_mut()
            .find(|old| old.id == account.id)
            .ok_or_else(|| std::io::Error::other("API account no longer exists"))?;
        *old = account;
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("API account revision overflow"))?;
        self.write(&state)
    }

    /// Selects the current target. Interactive callers must capture a revision before confirmation
    /// and pass it to `select_checked` instead.
    pub fn select(&self, selection: ApiAccountSelection) -> std::io::Result<()> {
        let revision = match &selection {
            ApiAccountSelection::Subscription => None,
            ApiAccountSelection::Manual { profile_id } => self.credential_revision(profile_id)?,
        };
        self.select_checked(selection, revision.as_deref())
    }

    /// Validates the target/key shown for confirmation in the same transaction as selection.
    /// Manual selection requires a confirmed revision; subscription selection does not.
    pub fn select_checked(
        &self,
        selection: ApiAccountSelection,
        expected_revision: Option<&str>,
    ) -> std::io::Result<()> {
        let _lock = crate::account_file::lock(&self.home)?;
        let mut state = self.read()?;
        if let ApiAccountSelection::Manual { profile_id } = &selection {
            let account = state
                .accounts
                .iter()
                .find(|account| &account.id == profile_id && !account.disabled)
                .ok_or_else(|| std::io::Error::other("API account is unavailable"))?;
            account.validate()?;
            let expected = expected_revision.ok_or_else(|| {
                std::io::Error::other("API selection needs a confirmed credential revision")
            })?;
            let revision = self
                .revision_for_target(account)?
                .ok_or_else(|| std::io::Error::other("API account needs a key"))?;
            if revision != expected {
                return Err(std::io::Error::other(
                    "API target or key changed since confirmation; reload before selecting",
                ));
            }
        }
        state.selection = selection;
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("API account revision overflow"))?;
        self.write(&state)
    }

    /// Configures the current target. Interactive callers must use `configure_fallback_checked`
    /// with the revision captured before confirmation.
    pub fn configure_fallback(&self, fallback: ApiAccountFallback) -> std::io::Result<()> {
        let revision = match (&fallback.profile_id, fallback.enabled) {
            (Some(profile_id), true) => self.credential_revision(profile_id)?,
            (Some(_) | None, false) | (None, true) => None,
        };
        self.configure_fallback_checked(fallback, revision.as_deref())
    }

    /// Binds paid fallback enablement to its confirmed target and exact key.
    /// Disabling fallback does not require a credential revision.
    pub fn configure_fallback_checked(
        &self,
        fallback: ApiAccountFallback,
        expected_revision: Option<&str>,
    ) -> std::io::Result<()> {
        let _lock = crate::account_file::lock(&self.home)?;
        let mut state = self.read()?;
        if fallback.wait_minutes > 1440 {
            return Err(std::io::Error::other(
                "Fallback waiting limit is 1440 minutes",
            ));
        }
        if fallback.enabled {
            let account = fallback
                .profile_id
                .as_ref()
                .and_then(|id| {
                    state
                        .accounts
                        .iter()
                        .find(|account| &account.id == id && !account.disabled)
                })
                .ok_or_else(|| std::io::Error::other("Choose an enabled API fallback account"))?;
            account.validate()?;
            let expected = expected_revision.ok_or_else(|| {
                std::io::Error::other("Paid API fallback needs a confirmed credential revision")
            })?;
            if self.revision_for_target(account)?.as_deref() != Some(expected) {
                return Err(std::io::Error::other(
                    "API target or key changed since confirmation; reload before enabling fallback",
                ));
            }
        }
        state.fallback = fallback;
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("API account revision overflow"))?;
        self.write(&state)
    }

    pub fn remove(&self, id: &str) -> std::io::Result<()> {
        let home = self.credential_home(id)?;
        let _lock = crate::account_file::lock(&self.home)?;
        let mut state = self.read()?;
        if !state.accounts.iter().any(|account| account.id == id) {
            return Err(std::io::Error::other("API account no longer exists"));
        }
        crate::logout(&home, self.mode, self.keyring)?;
        if home.is_dir() {
            std::fs::remove_dir_all(&home)?;
        }
        state.accounts.retain(|account| account.id != id);
        if matches!(&state.selection, ApiAccountSelection::Manual { profile_id } if profile_id == id)
        {
            state.selection = ApiAccountSelection::Subscription;
        }
        if state.fallback.profile_id.as_deref() == Some(id) {
            state.fallback = ApiAccountFallback::default();
        }
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("API account revision overflow"))?;
        self.write(&state)
    }

    pub fn has_key(&self, id: &str) -> std::io::Result<bool> {
        Ok(
            crate::load_auth_dot_json(&self.credential_home(id)?, self.mode, self.keyring)?
                .and_then(|auth| auth.openai_api_key)
                .is_some(),
        )
    }

    /// Returns a target-bound fingerprint so confirmations detect replacement of the API key.
    /// The digest cannot authenticate requests and never includes a plaintext credential.
    pub fn credential_revision(&self, id: &str) -> std::io::Result<Option<String>> {
        let _lock = crate::account_file::lock(&self.home)?;
        let state = self.read()?;
        let account = state
            .accounts
            .iter()
            .find(|account| account.id == id)
            .ok_or_else(|| std::io::Error::other("API account no longer exists"))?;
        self.revision_for_target(account)
    }

    fn revision_for_target(&self, account: &ApiAccount) -> std::io::Result<Option<String>> {
        use sha2::Digest;
        let key = crate::load_auth_dot_json(
            &self.credential_home(&account.id)?,
            self.mode,
            self.keyring,
        )?
        .and_then(|auth| auth.openai_api_key);
        let descriptor = serde_json::to_vec(account)?;
        Ok(key.filter(|key| !key.trim().is_empty()).map(|key| {
            let mut digest = sha2::Sha256::new();
            digest.update(b"codex-api-credential-revision-v1\0");
            digest.update((descriptor.len() as u64).to_be_bytes());
            digest.update(&descriptor);
            digest.update(key.as_bytes());
            format!("{:x}", digest.finalize())
        }))
    }

    /// Replaces a key without changing the profile's target, capabilities or selection.
    pub fn replace_key(&self, id: &str, key: &str) -> std::io::Result<()> {
        if key.trim().is_empty() || key.len() > 16_384 || key.contains(['\r', '\n']) {
            return Err(std::io::Error::other("Invalid API key"));
        }
        let _lock = crate::account_file::lock(&self.home)?;
        let state = self.read()?;
        if !state.accounts.iter().any(|account| account.id == id) {
            return Err(std::io::Error::other("API account no longer exists"));
        }
        let home = self.credential_home(id)?;
        let auth = AuthDotJson {
            auth_mode: Some(codex_protocol::auth::AuthMode::ApiKey),
            openai_api_key: Some(key.into()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        };
        crate::save_auth(&home, &auth, self.mode, self.keyring)
    }
}

fn valid_id(id: &str) -> bool {
    id.starts_with("api-")
        && id.len() <= 64
        && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}
