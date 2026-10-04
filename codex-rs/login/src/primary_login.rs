//! Persistent host-login selection, independent of the account used for inference.

use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::auth::AuthMode;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use crate::AccountProfileId;
use crate::AccountProfileState;
use crate::AccountProfileStore;
use crate::AuthConfig;
use crate::AuthDotJson;
use crate::CodexAuth;
use crate::TokenData;

const PRIMARY_LOGIN_VERSION: u32 = 2;
const PRIMARY_LOGIN_FILE: &str = ".primary-login.json";
const MAX_SOURCE_BYTES: u64 = 64 * 1024;

/// Credential source for application sign-in and RemoteControl; never a pool scheduling choice.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrimaryLoginSource {
    RootLogin,
    Profile {
        profile_id: AccountProfileId,
        /// Binds the reference to one ChatGPT user and workspace without persisting either ID.
        owner_hash: String,
    },
    SignedOut,
}

/// Atomic, revisioned host-login metadata. Missing files preserve the stock root-login behavior.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimaryLoginState {
    pub version: u32,
    pub revision: u64,
    pub source: PrimaryLoginSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_handover: Option<crate::PrimaryLoginHandover>,
}

impl Default for PrimaryLoginState {
    fn default() -> Self {
        Self {
            version: PRIMARY_LOGIN_VERSION,
            revision: 0,
            source: PrimaryLoginSource::RootLogin,
            remote_handover: None,
        }
    }
}

/// Owns non-secret primary-login references. Credentials remain in their original storage home.
#[derive(Clone, Debug)]
pub struct PrimaryLoginStore {
    codex_home: PathBuf,
}

impl PrimaryLoginStore {
    pub fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    pub fn path(&self) -> PathBuf {
        self.codex_home.join(PRIMARY_LOGIN_FILE)
    }

    /// Reads an atomic snapshot without waiting for a pool metadata transaction.
    pub fn load(&self) -> io::Result<PrimaryLoginState> {
        let file = match fs::File::open(self.path()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PrimaryLoginState::default());
            }
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SOURCE_BYTES {
            return Err(invalid_source(
                "primary-login metadata exceeds its size limit",
            ));
        }
        let state: PrimaryLoginState = serde_json::from_slice(&bytes).map_err(invalid_source)?;
        validate_state(&state)?;
        Ok(state)
    }

    /// Selects a ready, managed subscription profile after checking its stored owner and policy.
    /// Disabled inference profiles and exhausted quotas do not prevent application sign-in.
    pub async fn select_profile(
        &self,
        auth_config: &AuthConfig,
        id: &AccountProfileId,
    ) -> io::Result<PrimaryLoginState> {
        if crate::is_workload_identity_selected() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Host-managed workload identity cannot be replaced by primary-login selection",
            ));
        }
        if auth_config.codex_home != self.codex_home {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "primary-login store and auth configuration use different homes",
            ));
        }
        let profiles = AccountProfileStore::new(self.codex_home.clone());
        let profile = ready_profile(&profiles, id)?;
        let mut profile_config = auth_config.clone();
        profile_config.codex_home = profile.credential_home.clone();
        let auth = profile_config
            .load_managed_profile_auth()
            .await?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "this profile has no permitted managed ChatGPT login",
                )
            })?;
        let owner_hash = managed_owner_hash(&auth)?;
        let store = self.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            let _metadata_lock = crate::account_file::lock(&store.codex_home)?;
            let current = ready_profile(&profiles, &id)?;
            if current.credential_home != profile.credential_home {
                return Err(invalid_source(
                    "selected profile storage changed during sign-in",
                ));
            }
            let _credential_lock = crate::account_file::refresh_lock(&current.credential_home)?;
            if stored_owner_hash(&profile_config)? != owner_hash {
                return Err(invalid_source(
                    "selected profile identity changed during sign-in",
                ));
            }
            store.write_source_unlocked(
                PrimaryLoginSource::Profile {
                    profile_id: id,
                    owner_hash,
                },
                Some(crate::PrimaryLoginHandover::for_auth(&auth)?),
            )
        })
        .await
        .map_err(io::Error::other)?
    }

    /// Returns to the root login. This action does not create, copy or revoke credentials.
    pub fn use_root(&self) -> io::Result<PrimaryLoginState> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        self.write_source_unlocked(PrimaryLoginSource::RootLogin, /*remote_handover*/ None)
    }

    /// Signs the application out while keeping root and pool credentials available for inference.
    pub fn sign_out(&self) -> io::Result<PrimaryLoginState> {
        if crate::is_workload_identity_selected() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Host-managed workload identity cannot be signed out by primary-login selection",
            ));
        }
        let _lock = crate::account_file::lock(&self.codex_home)?;
        self.write_source_unlocked(PrimaryLoginSource::SignedOut, /*remote_handover*/ None)
    }

    /// Checks a deletion guard without acquiring another metadata lock.
    /// The caller must retain its pool transaction until the guarded mutation completes.
    pub(crate) fn is_profile_selected_unlocked(
        home: &Path,
        id: &AccountProfileId,
    ) -> io::Result<bool> {
        Ok(matches!(
            Self::new(home.to_path_buf()).load()?.source,
            PrimaryLoginSource::Profile { profile_id, .. } if &profile_id == id
        ))
    }

    pub fn is_profile_selected(home: &Path, id: &AccountProfileId) -> io::Result<bool> {
        let _lock = crate::account_file::lock(home)?;
        Self::is_profile_selected_unlocked(home, id)
    }

    /// Explicit root selection may continue an already-enabled Remote connection for this owner.
    pub async fn select_root(&self, config: &AuthConfig) -> io::Result<PrimaryLoginState> {
        if config.codex_home != self.codex_home || crate::is_workload_identity_selected() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Host-managed authentication cannot be replaced",
            ));
        }
        // Host PAT/Agent Identity hydration belongs to the request path. Selecting a source
        // must not contact a network merely to render the management page.
        let local = config.root_auth_load_is_local()?;
        let auth = if local {
            config.load_auth(/*enable_codex_api_key_env*/ false).await?
        } else {
            None
        };
        let handover = auth
            .as_ref()
            .and_then(|auth| crate::PrimaryLoginHandover::for_auth(auth).ok());
        let _lock = crate::account_file::lock(&self.codex_home)?;
        if let Some(intent) = &handover {
            let saved = crate::primary_login_runtime::fingerprint::stored_auth(
                config,
                &PrimaryLoginSource::RootLogin,
            )?;
            let owner = saved
                .as_ref()
                .and_then(|saved| saved.tokens.as_ref())
                .and_then(|tokens| tokens_owner_hash(tokens).ok());
            if owner.as_deref() != Some(&intent.target_owner_hash) {
                return Err(invalid_source("root login changed during selection"));
            }
        }
        self.write_source_unlocked(PrimaryLoginSource::RootLogin, handover)
    }

    /// Runs a short local continuation while retaining the source and credential transaction.
    /// The callback must not await or acquire another account metadata lock.
    pub fn with_current_handover<T>(
        &self,
        config: &AuthConfig,
        expected: &PrimaryLoginState,
        apply: impl FnOnce(&crate::PrimaryLoginHandover) -> io::Result<T>,
    ) -> io::Result<T> {
        let _metadata = crate::account_file::try_lock(&self.codex_home)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "host selection is busy"))?;
        if config.codex_home != self.codex_home || self.load()? != *expected {
            return Err(invalid_source(
                "host selection changed before Remote continuation",
            ));
        }
        let intent = expected
            .remote_handover
            .as_ref()
            .filter(|intent| intent.is_current())
            .ok_or_else(|| invalid_source("Remote continuation expired"))?;
        let mut selected_config = config.clone();
        if let PrimaryLoginSource::Profile { profile_id, .. } = &expected.source {
            selected_config.codex_home = ready_profile(
                &AccountProfileStore::new(self.codex_home.clone()),
                profile_id,
            )?
            .credential_home;
        }
        let _credentials = crate::account_file::try_refresh_lock(&selected_config.codex_home)?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::WouldBlock, "host credentials are busy")
            })?;
        let saved = crate::primary_login_runtime::fingerprint::stored_auth(
            &selected_config,
            &expected.source,
        )?;
        let owner = saved
            .as_ref()
            .and_then(|saved| saved.tokens.as_ref())
            .and_then(|tokens| tokens_owner_hash(tokens).ok());
        if owner.as_deref() != Some(&intent.target_owner_hash) {
            return Err(invalid_source(
                "host credentials changed before Remote continuation",
            ));
        }
        apply(intent)
    }

    fn write_source_unlocked(
        &self,
        source: PrimaryLoginSource,
        remote_handover: Option<crate::PrimaryLoginHandover>,
    ) -> io::Result<PrimaryLoginState> {
        let current = self.load()?;
        let state = PrimaryLoginState {
            version: PRIMARY_LOGIN_VERSION,
            revision: current
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid_source("primary-login revision limit reached"))?,
            source,
            remote_handover,
        };
        validate_state(&state)?;
        fs::create_dir_all(&self.codex_home)?;
        let temporary = self
            .codex_home
            .join(format!("{PRIMARY_LOGIN_FILE}.tmp-{}", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(&state).map_err(invalid_source)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, self.path())?;
            Ok(state)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

pub(crate) fn ready_profile(
    profiles: &AccountProfileStore,
    id: &AccountProfileId,
) -> io::Result<crate::AccountProfile> {
    profiles
        .load_profile_records_unlocked()
        .map_err(io::Error::other)?
        .into_iter()
        .find(|record| &record.profile.id == id && record.state == AccountProfileState::Ready)
        .map(|record| record.profile)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "primary-login profile is missing or has not completed sign-in",
            )
        })
}

pub(crate) fn managed_owner_hash(auth: &CodexAuth) -> io::Result<String> {
    if !matches!(auth, CodexAuth::Chatgpt(_)) {
        return Err(invalid_source(
            "primary-login profile requires a managed ChatGPT subscription",
        ));
    }
    let tokens = auth.get_token_data()?;
    validate_managed_tokens(&tokens)?;
    tokens_owner_hash(&tokens)
}

pub(crate) fn stored_owner_hash(config: &AuthConfig) -> io::Result<String> {
    let auth = crate::auth::load_auth_dot_json(
        &config.codex_home,
        config.auth_credentials_store_mode,
        config.keyring_backend_kind,
    )?
    .ok_or_else(|| invalid_source("primary-login profile has no stored credentials"))?;
    validate_stored_mode(&auth)?;
    let tokens = auth
        .tokens
        .as_ref()
        .ok_or_else(|| invalid_source("stored login has no token data"))?;
    validate_managed_tokens(tokens)?;
    tokens_owner_hash(tokens)
}

pub(crate) fn validate_stored_mode(auth: &AuthDotJson) -> io::Result<()> {
    let managed = auth.auth_mode == Some(AuthMode::Chatgpt)
        || (auth.auth_mode.is_none()
            && auth.openai_api_key.is_none()
            && auth.personal_access_token.is_none()
            && auth.agent_identity.is_none()
            && auth.bedrock_api_key.is_none()
            && auth.bedrock_access_keys.is_none());
    if !managed || auth.last_refresh.is_none() {
        return Err(invalid_source(
            "stored login is not a managed ChatGPT subscription",
        ));
    }
    Ok(())
}

fn validate_managed_tokens(tokens: &TokenData) -> io::Result<()> {
    if tokens.access_token.trim().is_empty() || tokens.refresh_token.trim().is_empty() {
        return Err(invalid_source(
            "ChatGPT login has incomplete managed credentials",
        ));
    }
    Ok(())
}

pub(crate) fn tokens_owner_hash(tokens: &TokenData) -> io::Result<String> {
    let user = tokens
        .id_token
        .chatgpt_user_id
        .as_deref()
        .filter(|id| !id.is_empty() && id.trim() == *id);
    let workspace = tokens
        .account_id
        .as_deref()
        .filter(|id| !id.is_empty() && id.trim() == *id);
    let (Some(user), Some(workspace)) = (user, workspace) else {
        return Err(invalid_source(
            "ChatGPT login does not identify both a user and workspace",
        ));
    };
    let mut digest = Sha256::new();
    digest.update(b"codex-primary-login-owner-v1\0");
    for part in [user, workspace] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_state(state: &PrimaryLoginState) -> io::Result<()> {
    if !matches!(state.version, 1 | PRIMARY_LOGIN_VERSION) {
        return Err(invalid_source("unsupported primary-login metadata version"));
    }
    if let Some(handover) = &state.remote_handover {
        handover.validate()?;
        if state.version == 1 || state.source == PrimaryLoginSource::SignedOut {
            return Err(invalid_source(
                "primary-login handover is not permitted for this state",
            ));
        }
    }
    if let PrimaryLoginSource::Profile {
        profile_id,
        owner_hash,
    } = &state.source
        && (profile_id.as_str().len() > 256
            || AccountProfileId::new(profile_id.as_str()).is_err()
            || owner_hash.len() != 64
            || !owner_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(invalid_source("primary-login profile reference is invalid"));
    }
    Ok(())
}

fn invalid_source(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
#[path = "primary_login_tests.rs"]
pub(crate) mod tests;
