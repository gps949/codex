use std::fs;
use std::io;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::AccountAvailability;
use crate::AccountPool;
use crate::AccountPoolSnapshot;
use crate::AccountProfileId;
use crate::AccountRateLimits;

const ACCOUNT_RUNTIME_STATE_VERSION: u32 = 1;
const ACCOUNT_RUNTIME_STATE_FILE: &str = "account-runtime-state.json";

/// Persisted scheduler state that is safe to reuse after a Codex restart.
///
/// This is intentionally separate from `account-profiles.json`: profiles describe user-owned
/// authentication configuration, while this file contains disposable runtime observations such as
/// cooldowns and cached quota snapshots.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountRuntimeState {
    #[serde(default)]
    pub active_profile_id: Option<AccountProfileId>,
    #[serde(default)]
    pub profiles: Vec<AccountRuntimeProfileState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountRuntimeProfileState {
    pub profile_id: AccountProfileId,
    /// Only authoritative backend exhaustion with a known future reset is persisted. Permanent
    /// auth failures are re-evaluated from credentials on startup and unknown-reset quota failures
    /// are retried rather than becoming an accidental permanent local ban.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exhausted_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub rate_limits: AccountRateLimits,
}

#[derive(Clone, Debug)]
pub struct AccountRuntimeStateStore {
    codex_home: PathBuf,
}

impl AccountRuntimeStateStore {
    pub fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    pub fn path(&self) -> PathBuf {
        self.codex_home.join(ACCOUNT_RUNTIME_STATE_FILE)
    }

    pub fn load(&self) -> Result<AccountRuntimeState, AccountRuntimeStateError> {
        let content = match fs::read_to_string(self.path()) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(AccountRuntimeState::default());
            }
            Err(error) => return Err(error.into()),
        };

        let wire: AccountRuntimeStateWire = serde_json::from_str(&content)?;
        if wire.version != ACCOUNT_RUNTIME_STATE_VERSION {
            return Err(AccountRuntimeStateError::UnsupportedVersion(wire.version));
        }

        let now = Utc::now();
        Ok(AccountRuntimeState {
            active_profile_id: wire.active_profile_id,
            profiles: wire
                .profiles
                .into_iter()
                .map(|mut profile| {
                    if profile
                        .exhausted_until
                        .as_ref()
                        .is_some_and(|reset| reset <= &now)
                    {
                        profile.exhausted_until = None;
                    }
                    profile
                })
                .collect(),
        })
    }

    pub fn save(&self, state: &AccountRuntimeState) -> Result<(), AccountRuntimeStateError> {
        fs::create_dir_all(&self.codex_home)?;
        let wire = AccountRuntimeStateWire {
            version: ACCOUNT_RUNTIME_STATE_VERSION,
            active_profile_id: state.active_profile_id.clone(),
            profiles: state.profiles.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&wire)?;
        let final_path = self.path();
        let temporary_path = self.codex_home.join(format!(
            ".{ACCOUNT_RUNTIME_STATE_FILE}.tmp-{}",
            std::process::id()
        ));
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

    /// Serializes only restart-safe observations from the live pool.
    pub fn save_pool(&self, pool: &AccountPool) -> Result<(), AccountRuntimeStateError> {
        let snapshots = pool.snapshots();
        self.save(&runtime_state_from_snapshots(&snapshots))
    }

    /// Removes stale runtime observations after a profile is deleted without touching OAuth
    /// credentials or the profile manifest.
    pub fn remove_profile(
        &self,
        profile_id: &AccountProfileId,
    ) -> Result<(), AccountRuntimeStateError> {
        let mut state = self.load()?;
        if state.active_profile_id.as_ref() == Some(profile_id) {
            state.active_profile_id = None;
        }
        state.profiles.retain(|profile| &profile.profile_id != profile_id);
        self.save(&state)
    }
}

fn runtime_state_from_snapshots(snapshots: &[AccountPoolSnapshot]) -> AccountRuntimeState {
    let now = Utc::now();
    AccountRuntimeState {
        active_profile_id: snapshots
            .iter()
            .find(|snapshot| snapshot.is_active)
            .map(|snapshot| snapshot.profile.id.clone()),
        profiles: snapshots
            .iter()
            .map(|snapshot| AccountRuntimeProfileState {
                profile_id: snapshot.profile.id.clone(),
                exhausted_until: match &snapshot.availability {
                    AccountAvailability::Exhausted {
                        resets_at: Some(reset),
                    } if reset > &now => Some(reset.clone()),
                    _ => None,
                },
                rate_limits: snapshot.rate_limits.clone(),
            })
            .collect(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AccountRuntimeStateWire {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_profile_id: Option<AccountProfileId>,
    #[serde(default)]
    profiles: Vec<AccountRuntimeProfileState>,
}

#[derive(Debug, Error)]
pub enum AccountRuntimeStateError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("unsupported account runtime state version: {0}")]
    UnsupportedVersion(u32),
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn missing_runtime_state_is_empty_without_creating_a_file() {
        let temp = TempDir::new().expect("temp dir");
        let store = AccountRuntimeStateStore::new(temp.path().join("codex"));
        assert_eq!(store.load().expect("load missing state"), AccountRuntimeState::default());
        assert!(!store.path().exists());
    }

    #[test]
    fn expired_cooldown_is_not_restored() {
        let temp = TempDir::new().expect("temp dir");
        let store = AccountRuntimeStateStore::new(temp.path().join("codex"));
        let profile_id = AccountProfileId::new("account-a").expect("profile id");
        store
            .save(&AccountRuntimeState {
                active_profile_id: Some(profile_id.clone()),
                profiles: vec![AccountRuntimeProfileState {
                    profile_id,
                    exhausted_until: Some(Utc::now() - Duration::minutes(1)),
                    rate_limits: AccountRateLimits::default(),
                }],
            })
            .expect("save state");

        let loaded = store.load().expect("load state");
        assert_eq!(loaded.profiles[0].exhausted_until, None);
    }

    #[test]
    fn future_cooldown_and_active_profile_round_trip() {
        let temp = TempDir::new().expect("temp dir");
        let store = AccountRuntimeStateStore::new(temp.path().join("codex"));
        let profile_id = AccountProfileId::new("account-a").expect("profile id");
        let reset = Utc::now() + Duration::minutes(30);
        let state = AccountRuntimeState {
            active_profile_id: Some(profile_id.clone()),
            profiles: vec![AccountRuntimeProfileState {
                profile_id,
                exhausted_until: Some(reset),
                rate_limits: AccountRateLimits::default(),
            }],
        };
        store.save(&state).expect("save state");
        assert_eq!(store.load().expect("load state"), state);
    }
}
