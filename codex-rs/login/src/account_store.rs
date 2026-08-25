use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::account_pool::AccountProfile;
use crate::account_pool::AccountProfileId;
use crate::account_pool::AccountPoolError;

const ACCOUNT_PROFILES_VERSION: u32 = 1;
const ACCOUNT_PROFILES_MANIFEST: &str = "account-profiles.json";
const ACCOUNT_CREDENTIALS_DIR: &str = "auth-profiles";
const LEGACY_ROOT_PROFILE_ID: &str = "legacy-root";
const PROFILE_ALLOCATION_ATTEMPTS: usize = 8;

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

    /// Loads all persisted profiles and resolves their credential homes against the current
    /// `CODEX_HOME`, so moving the entire Codex home does not bake stale absolute paths into the
    /// manifest.
    pub fn load_profiles(&self) -> Result<Vec<AccountProfile>, AccountProfileStoreError> {
        let manifest = self.load_manifest()?;
        manifest
            .profiles
            .into_iter()
            .map(|stored| self.resolve_profile(stored))
            .collect()
    }

    /// Allocates a permanent profile id and credential directory before OAuth starts.
    ///
    /// This guarantees file and keyring storage derive from the final credential home for the
    /// complete login and refresh lifecycle.
    pub fn allocate_profile(
        &self,
        label: Option<String>,
        priority: u32,
    ) -> Result<AccountProfile, AccountProfileStoreError> {
        fs::create_dir_all(self.managed_credentials_root())?;
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
        });
        self.save_manifest(&manifest)?;
        Ok(AccountProfile::new(
            legacy_id,
            self.codex_home.clone(),
            priority,
            label,
        ))
    }

    /// Removes only profile metadata. Credential deletion is intentionally separate so a profile
    /// cannot lose OAuth material as a side effect of an in-memory pool operation.
    pub fn remove_profile_metadata(
        &self,
        id: &AccountProfileId,
    ) -> Result<bool, AccountProfileStoreError> {
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

    fn resolve_profile(
        &self,
        stored: StoredAccountProfile,
    ) -> Result<AccountProfile, AccountProfileStoreError> {
        let credential_home = match stored.credential_location {
            CredentialLocation::LegacyRoot => self.codex_home.clone(),
            CredentialLocation::ManagedProfile => self.credential_home_for(&stored.id),
        };
        Ok(AccountProfile::new(
            stored.id,
            credential_home,
            stored.priority,
            stored.label,
        ))
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
        let temporary_path = self
            .codex_home
            .join(format!(".{ACCOUNT_PROFILES_MANIFEST}.tmp-{}", std::process::id()));
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
            return Err(AccountProfileStoreError::DuplicateProfile(profile.id.clone()));
        }
        if profile.credential_location == CredentialLocation::LegacyRoot
            && profile.id.as_str() != LEGACY_ROOT_PROFILE_ID
        {
            return Err(AccountProfileStoreError::InvalidLegacyProfileId(
                profile.id.clone(),
            ));
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
    #[error("legacy root credential location requires profile id {LEGACY_ROOT_PROFILE_ID}, found {0}")]
    InvalidLegacyProfileId(AccountProfileId),
    #[error("the legacy root profile id is already used by a managed credential profile")]
    LegacyProfileConflict,
    #[error("could not allocate a unique account profile id")]
    ProfileIdAllocationExhausted,
    #[error("legacy root credentials cannot be recursively purged through the account profile store")]
    CannotPurgeLegacyRoot,
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn managed_profile_round_trips_without_persisting_absolute_home() {
        let temp = TempDir::new().expect("temp dir");
        let store = AccountProfileStore::new(temp.path().join("codex"));
        let profile = store
            .allocate_profile(Some("work".to_string()), 20)
            .expect("allocate profile");

        assert!(profile.credential_home.is_dir());
        let manifest = fs::read_to_string(store.manifest_path()).expect("read manifest");
        assert!(!manifest.contains(profile.credential_home.to_string_lossy().as_ref()));

        let loaded = store.load_profiles().expect("load profiles");
        assert_eq!(loaded, vec![profile]);
    }

    #[test]
    fn legacy_profile_keeps_root_credential_home() {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path().join("codex");
        let store = AccountProfileStore::new(root.clone());
        let profile = store
            .ensure_legacy_root_profile(Some("existing login".to_string()), 0)
            .expect("legacy profile");

        assert_eq!(profile.id.as_str(), LEGACY_ROOT_PROFILE_ID);
        assert_eq!(profile.credential_home, root);
        assert_eq!(
            store.load_profiles().expect("load profiles"),
            vec![profile]
        );
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
    fn rejects_duplicate_profile_ids_in_manifest() {
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
