use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::account_pool::AccountPoolError;
use crate::account_pool::AccountProfile;
use crate::account_pool::AccountProfileId;

const ACCOUNT_PROFILES_VERSION: u32 = 1;
const ACCOUNT_PROFILES_MANIFEST: &str = "account-profiles.json";
const ACCOUNT_CREDENTIALS_DIR: &str = "auth-profiles";
const LEGACY_ROOT_PROFILE_ID: &str = "legacy-root";
const PROFILE_ALLOCATION_ATTEMPTS: usize = 8;

/// Persistent login lifecycle for an account profile.
///
/// A profile is allocated before OAuth begins so credentials are written directly to their final
/// storage location, but it is not eligible for scheduling until login has completed successfully.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountProfileState {
    PendingLogin,
    Ready,
}

fn default_profile_state() -> AccountProfileState {
    // Profiles written before the lifecycle field existed represented completed profiles.
    AccountProfileState::Ready
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountProfileRecord {
    pub profile: AccountProfile,
    pub state: AccountProfileState,
}

/// Owns the persistent account-profile layout inside one shared `CODEX_HOME`.
///
/// Conversation history, state DBs, configuration and app-server state remain in the root
/// `CODEX_HOME`. Only account credentials are placed in per-profile credential homes.
#[derive(Clone, Debug)]
pub struct AccountProfileStore {
    codex_home: PathBuf,
}

impl AccountProfileStore {
    pub fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    pub fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.codex_home.join(ACCOUNT_PROFILES_MANIFEST)
    }

    pub fn managed_credentials_root(&self) -> PathBuf {
        self.codex_home.join(ACCOUNT_CREDENTIALS_DIR)
    }

    /// Loads profiles that completed login and are eligible to be materialized into the account
    /// pool. Pending profiles remain visible through [`Self::load_profile_records`].
    pub fn load_profiles(&self) -> Result<Vec<AccountProfile>, AccountProfileStoreError> {
        Ok(self
            .load_profile_records()?
            .into_iter()
            .filter(|record| record.state == AccountProfileState::Ready)
            .map(|record| record.profile)
            .collect())
    }

    /// Loads every persisted profile, including interrupted/pending login attempts.
    pub fn load_profile_records(
        &self,
    ) -> Result<Vec<AccountProfileRecord>, AccountProfileStoreError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        self.load_profile_records_unlocked()
    }

    pub(crate) fn load_profile_records_unlocked(
        &self,
    ) -> Result<Vec<AccountProfileRecord>, AccountProfileStoreError> {
        let manifest = self.load_manifest()?;
        manifest
            .profiles
            .into_iter()
            .map(|stored| self.resolve_record(stored))
            .collect()
    }

    /// Allocates a permanent profile id and credential directory before OAuth starts.
    ///
    /// The manifest entry starts as `pending_login`; callers must invoke [`Self::complete_profile`]
    /// only after the official login flow has persisted usable credentials.
    pub fn allocate_profile(
        &self,
        label: Option<String>,
        priority: u32,
    ) -> Result<AccountProfile, AccountProfileStoreError> {
        fs::create_dir_all(self.managed_credentials_root())?;
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let mut manifest = self.load_manifest()?;

        for _ in 0..PROFILE_ALLOCATION_ATTEMPTS {
            let id = AccountProfileId::generate();
            if manifest.profiles.iter().any(|profile| profile.id == id) {
                continue;
            }

            let credential_home = self.credential_home_for(&id);
            match fs::create_dir(&credential_home) {
                Ok(()) => {
                    let stored = StoredAccountProfile {
                        id: id.clone(),
                        label: label.clone(),
                        priority,
                        credential_location: CredentialLocation::ManagedProfile,
                        state: AccountProfileState::PendingLogin,
                        disabled: false,
                    };
                    manifest.profiles.push(stored);
                    if let Err(error) = self.save_manifest(&manifest) {
                        let _ = fs::remove_dir(&credential_home);
                        return Err(error);
                    }
                    return Ok(AccountProfile::new(id, credential_home, priority, label));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }

        Err(AccountProfileStoreError::ProfileIdAllocationExhausted)
    }

    /// Marks a profile eligible for scheduling after its OAuth flow has completed.
    pub fn complete_profile(
        &self,
        id: &AccountProfileId,
    ) -> Result<AccountProfile, AccountProfileStoreError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let mut manifest = self.load_manifest()?;
        let stored = manifest
            .profiles
            .iter_mut()
            .find(|profile| &profile.id == id)
            .ok_or_else(|| AccountProfileStoreError::UnknownProfile(id.clone()))?;
        stored.state = AccountProfileState::Ready;
        let resolved = stored.clone();
        self.save_manifest(&manifest)?;
        self.resolve_profile(resolved)
    }

    /// Removes an unfinished managed profile and its credential directory. Ready profiles require
    /// the explicit metadata-removal and credential-purge operations so a normal logout/remove UI
    /// can control destructive behavior deliberately.
    pub fn abandon_pending_profile(
        &self,
        id: &AccountProfileId,
    ) -> Result<bool, AccountProfileStoreError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let mut manifest = self.load_manifest()?;
        let Some(index) = manifest
            .profiles
            .iter()
            .position(|profile| &profile.id == id)
        else {
            return Ok(false);
        };
        let profile = &manifest.profiles[index];
        if profile.state != AccountProfileState::PendingLogin {
            return Err(AccountProfileStoreError::ProfileNotPending(id.clone()));
        }
        if profile.credential_location == CredentialLocation::LegacyRoot {
            return Err(AccountProfileStoreError::CannotPurgeLegacyRoot);
        }

        manifest.profiles.remove(index);
        self.save_manifest(&manifest)?;
        let path = self.credential_home_for(id);
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    /// Adds the existing root credentials as a compatibility profile without moving or copying
    /// them. Callers should only invoke this after confirming the root AuthManager actually has
    /// usable credentials.
    pub fn ensure_legacy_root_profile(
        &self,
        label: Option<String>,
        priority: u32,
    ) -> Result<AccountProfile, AccountProfileStoreError> {
        let legacy_id = AccountProfileId::new(LEGACY_ROOT_PROFILE_ID)
            .map_err(AccountProfileStoreError::Pool)?;
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let mut manifest = self.load_manifest()?;

        if let Some(existing) = manifest
            .profiles
            .iter()
            .find(|profile| profile.id == legacy_id)
            .cloned()
        {
            if existing.credential_location != CredentialLocation::LegacyRoot {
                return Err(AccountProfileStoreError::LegacyProfileConflict);
            }
            return self.resolve_profile(existing);
        }

        manifest.profiles.push(StoredAccountProfile {
            id: legacy_id.clone(),
            label: label.clone(),
            priority,
            credential_location: CredentialLocation::LegacyRoot,
            state: AccountProfileState::Ready,
            disabled: false,
        });
        self.save_manifest(&manifest)?;
        Ok(AccountProfile::new(
            legacy_id,
            self.codex_home.clone(),
            priority,
            label,
        ))
    }

    /// Applies user-editable scheduling metadata to an existing profile and returns the updated
    /// resolved profile. Unset fields keep their current values.
    pub fn update_profile_metadata(
        &self,
        id: &AccountProfileId,
        update: AccountProfileMetadataUpdate,
    ) -> Result<AccountProfile, AccountProfileStoreError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let mut manifest = self.load_manifest()?;
        let stored = manifest
            .profiles
            .iter_mut()
            .find(|profile| &profile.id == id)
            .ok_or_else(|| AccountProfileStoreError::UnknownProfile(id.clone()))?;
        if let Some(priority) = update.priority {
            stored.priority = priority;
        }
        match update.label {
            Some(AccountLabelUpdate::Set(label)) => stored.label = Some(label),
            Some(AccountLabelUpdate::Clear) => stored.label = None,
            None => {}
        }
        if let Some(disabled) = update.disabled {
            stored.disabled = disabled;
        }
        let updated = stored.clone();
        self.save_manifest(&manifest)?;
        self.resolve_profile(updated)
    }

    /// Removes only profile metadata. Credential deletion is intentionally separate so a profile
    /// cannot lose OAuth material as a side effect of an in-memory pool operation.
    pub fn remove_profile_metadata(
        &self,
        id: &AccountProfileId,
    ) -> Result<bool, AccountProfileStoreError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let mut manifest = self.load_manifest()?;
        let before = manifest.profiles.len();
        manifest.profiles.retain(|profile| &profile.id != id);
        if manifest.profiles.len() == before {
            return Ok(false);
        }
        self.save_manifest(&manifest)?;
        Ok(true)
    }

    /// Deletes credentials for a managed profile after its metadata has been removed. The root
    /// compatibility profile is never recursively deleted through this API.
    pub fn purge_managed_credentials(
        &self,
        id: &AccountProfileId,
    ) -> Result<bool, AccountProfileStoreError> {
        if id.as_str() == LEGACY_ROOT_PROFILE_ID {
            return Err(AccountProfileStoreError::CannotPurgeLegacyRoot);
        }
        let path = self.credential_home_for(id);
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn credential_home_for(&self, id: &AccountProfileId) -> PathBuf {
        self.managed_credentials_root().join(id.as_str())
    }

    fn resolve_record(
        &self,
        stored: StoredAccountProfile,
    ) -> Result<AccountProfileRecord, AccountProfileStoreError> {
        let state = stored.state;
        Ok(AccountProfileRecord {
            profile: self.resolve_profile(stored)?,
            state,
        })
    }

    fn resolve_profile(
        &self,
        stored: StoredAccountProfile,
    ) -> Result<AccountProfile, AccountProfileStoreError> {
        let credential_home = match stored.credential_location {
            CredentialLocation::LegacyRoot => self.codex_home.clone(),
            CredentialLocation::ManagedProfile => self.credential_home_for(&stored.id),
        };
        let mut profile =
            AccountProfile::new(stored.id, credential_home, stored.priority, stored.label);
        profile.disabled = stored.disabled;
        Ok(profile)
    }

    fn load_manifest(&self) -> Result<AccountProfilesManifest, AccountProfileStoreError> {
        let path = self.manifest_path();
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(AccountProfilesManifest::default());
            }
            Err(error) => return Err(error.into()),
        };

        let manifest: AccountProfilesManifest = serde_json::from_str(&content)?;
        if manifest.version != ACCOUNT_PROFILES_VERSION {
            return Err(AccountProfileStoreError::UnsupportedManifestVersion(
                manifest.version,
            ));
        }
        validate_manifest(&manifest)?;
        Ok(manifest)
    }

    fn save_manifest(
        &self,
        manifest: &AccountProfilesManifest,
    ) -> Result<(), AccountProfileStoreError> {
        validate_manifest(manifest)?;
        fs::create_dir_all(&self.codex_home)?;
        let final_path = self.manifest_path();
        let temporary_path = self.codex_home.join(format!(
            ".{ACCOUNT_PROFILES_MANIFEST}.tmp-{}",
            std::process::id()
        ));
        let bytes = serde_json::to_vec_pretty(manifest)?;
        fs::write(&temporary_path, bytes)?;

        if let Err(error) = fs::rename(&temporary_path, &final_path) {
            if cfg!(windows) && final_path.exists() {
                fs::remove_file(&final_path)?;
                fs::rename(&temporary_path, &final_path)?;
            } else {
                let _ = fs::remove_file(&temporary_path);
                return Err(error.into());
            }
        }
        Ok(())
    }
}

/// User-editable metadata changes applied through `update_profile_metadata`. Unset fields keep
/// their current manifest values.
#[derive(Debug, Default)]
pub struct AccountProfileMetadataUpdate {
    pub priority: Option<u32>,
    pub label: Option<AccountLabelUpdate>,
    pub disabled: Option<bool>,
}

/// Distinguishes "set a new label" from "remove the label" without nesting options.
#[derive(Debug)]
pub enum AccountLabelUpdate {
    Set(String),
    Clear,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AccountProfilesManifest {
    version: u32,
    profiles: Vec<StoredAccountProfile>,
}

impl Default for AccountProfilesManifest {
    fn default() -> Self {
        Self {
            version: ACCOUNT_PROFILES_VERSION,
            profiles: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAccountProfile {
    id: AccountProfileId,
    label: Option<String>,
    priority: u32,
    credential_location: CredentialLocation,
    #[serde(default = "default_profile_state")]
    state: AccountProfileState,
    #[serde(default)]
    disabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CredentialLocation {
    LegacyRoot,
    ManagedProfile,
}

fn validate_manifest(manifest: &AccountProfilesManifest) -> Result<(), AccountProfileStoreError> {
    let mut seen = HashSet::new();
    for profile in &manifest.profiles {
        AccountProfileId::new(profile.id.as_str().to_string())
            .map_err(AccountProfileStoreError::Pool)?;
        if !seen.insert(profile.id.clone()) {
            return Err(AccountProfileStoreError::DuplicateProfile(
                profile.id.clone(),
            ));
        }
        if profile.credential_location == CredentialLocation::LegacyRoot
            && profile.id.as_str() != LEGACY_ROOT_PROFILE_ID
        {
            return Err(AccountProfileStoreError::InvalidLegacyProfileId(
                profile.id.clone(),
            ));
        }
        if profile.credential_location == CredentialLocation::LegacyRoot
            && profile.state != AccountProfileState::Ready
        {
            return Err(AccountProfileStoreError::LegacyProfileMustBeReady);
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum AccountProfileStoreError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Pool(#[from] AccountPoolError),
    #[error("unsupported account profile manifest version: {0}")]
    UnsupportedManifestVersion(u32),
    #[error("duplicate account profile in manifest: {0}")]
    DuplicateProfile(AccountProfileId),
    #[error("unknown account profile: {0}")]
    UnknownProfile(AccountProfileId),
    #[error("account profile is not pending login: {0}")]
    ProfileNotPending(AccountProfileId),
    #[error(
        "legacy root credential location requires profile id {LEGACY_ROOT_PROFILE_ID}, found {0}"
    )]
    InvalidLegacyProfileId(AccountProfileId),
    #[error("legacy root account profile must always be ready")]
    LegacyProfileMustBeReady,
    #[error("the legacy root profile id is already used by a managed credential profile")]
    LegacyProfileConflict,
    #[error("could not allocate a unique account profile id")]
    ProfileIdAllocationExhausted,
    #[error(
        "legacy root credentials cannot be recursively purged through the account profile store"
    )]
    CannotPurgeLegacyRoot,
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn managed_profile_is_pending_until_login_completes() {
        let temp = TempDir::new().expect("temp dir");
        let store = AccountProfileStore::new(temp.path().join("codex"));
        let profile = store
            .allocate_profile(Some("work".to_string()), 20)
            .expect("allocate profile");

        assert!(profile.credential_home.is_dir());
        let manifest = fs::read_to_string(store.manifest_path()).expect("read manifest");
        assert!(!manifest.contains(profile.credential_home.to_string_lossy().as_ref()));
        assert!(store.load_profiles().expect("ready profiles").is_empty());
        assert_eq!(
            store.load_profile_records().expect("all profiles"),
            vec![AccountProfileRecord {
                profile: profile.clone(),
                state: AccountProfileState::PendingLogin,
            }]
        );

        let completed = store
            .complete_profile(&profile.id)
            .expect("complete profile");
        assert_eq!(completed, profile);
        assert_eq!(
            store.load_profiles().expect("ready profiles"),
            vec![profile]
        );
    }

    #[test]
    fn abandoned_pending_profile_removes_metadata_and_directory() {
        let temp = TempDir::new().expect("temp dir");
        let store = AccountProfileStore::new(temp.path().join("codex"));
        let profile = store.allocate_profile(None, 10).expect("allocate profile");
        assert!(profile.credential_home.exists());

        assert!(
            store
                .abandon_pending_profile(&profile.id)
                .expect("abandon profile")
        );
        assert!(!profile.credential_home.exists());
        assert!(
            store
                .load_profile_records()
                .expect("all profiles")
                .is_empty()
        );
    }

    #[test]
    fn legacy_profile_keeps_root_credential_home_and_is_ready() {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path().join("codex");
        let store = AccountProfileStore::new(root.clone());
        let profile = store
            .ensure_legacy_root_profile(Some("existing login".to_string()), 0)
            .expect("legacy profile");

        assert_eq!(profile.id.as_str(), LEGACY_ROOT_PROFILE_ID);
        assert_eq!(profile.credential_home, root);
        assert_eq!(store.load_profiles().expect("load profiles"), vec![profile]);
    }

    #[test]
    fn purge_refuses_legacy_root() {
        let temp = TempDir::new().expect("temp dir");
        let store = AccountProfileStore::new(temp.path().join("codex"));
        let legacy = AccountProfileId::new(LEGACY_ROOT_PROFILE_ID).expect("legacy id");
        assert!(matches!(
            store.purge_managed_credentials(&legacy),
            Err(AccountProfileStoreError::CannotPurgeLegacyRoot)
        ));
    }

    #[test]
    fn rejects_duplicate_profile_ids_in_legacy_manifest_shape() {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path().join("codex");
        fs::create_dir_all(&root).expect("create root");
        let store = AccountProfileStore::new(root);
        fs::write(
            store.manifest_path(),
            r#"{
  "version": 1,
  "profiles": [
    {"id":"same","label":null,"priority":1,"credential_location":"managed_profile"},
    {"id":"same","label":null,"priority":2,"credential_location":"managed_profile"}
  ]
}"#,
        )
        .expect("write manifest");

        assert!(matches!(
            store.load_profiles(),
            Err(AccountProfileStoreError::DuplicateProfile(_))
        ));
    }
}
