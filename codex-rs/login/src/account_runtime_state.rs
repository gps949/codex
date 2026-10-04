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
use crate::WindowWarmupObservation;

#[path = "account_runtime_entitlement.rs"]
mod entitlement;
#[path = "account_runtime_quota_probe.rs"]
mod quota_probe;
#[path = "account_runtime_warmup_claim.rs"]
mod warmup_claim;
pub use quota_probe::AccountQuotaEvidence;
pub use quota_probe::AccountQuotaProbe;

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
    pub selection_revision: u64,
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
    /// A natural reset reported by the backend; old records without this remain unverified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_resets_at: Option<DateTime<Utc>>,
    /// Entitlement refusals cannot be repaired by spending earned reset credits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credit_excluded_until: Option<DateTime<Utc>>,
    /// Soft scheduling preference after an early switch; remaining quota stays usable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preemptive_rotation_until: Option<DateTime<Utc>>,
    /// Logical backend-confirmed reset epoch shared with other processes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_reset_at: Option<DateTime<Utc>>,
    /// Real request cutoff for old observations; absent legacy records use quota_reset_at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_reset_observed_at: Option<DateTime<Utc>>,
    /// Arrival time of the latest authoritative refusal; survives identical cooldown updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_failure_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub rate_limits: AccountRateLimits,
    /// Latest identity-preserving 5h-window warmup observation. Shared across
    /// processes so two `codex` invocations do not independently retry the same
    /// standby account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_warmup: Option<WindowWarmupObservation>,
}

/// Whether explicit selection may clear an observed cooldown for a fresh backend probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountSelectionMode {
    AvailableOnly,
    ForceProbe,
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
        if !self.path().exists() {
            return Ok(AccountRuntimeState::default());
        }
        let _lock = crate::account_file::lock(&self.codex_home)?;
        self.load_unlocked()
    }

    /// Reads shared observations without queuing behind another process's transaction.
    pub fn try_load(&self) -> Result<Option<AccountRuntimeState>, AccountRuntimeStateError> {
        let Some(_lock) = crate::account_file::try_lock(&self.codex_home)? else {
            return Ok(None);
        };
        self.load_unlocked().map(Some)
    }

    fn load_unlocked(&self) -> Result<AccountRuntimeState, AccountRuntimeStateError> {
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
            selection_revision: wire.selection_revision,
            profiles: wire
                .profiles
                .into_iter()
                .map(|mut profile| {
                    if let Some(observation) = profile.window_warmup.as_mut() {
                        observation.infer_legacy_phase();
                    }
                    if profile
                        .exhausted_until
                        .as_ref()
                        .is_some_and(|reset| reset <= &now)
                    {
                        profile.exhausted_until = None;
                        profile.backend_resets_at = None;
                    }
                    if profile
                        .preemptive_rotation_until
                        .is_some_and(|reset| reset <= now)
                    {
                        profile.preemptive_rotation_until = None;
                    }
                    profile.backend_resets_at = profile
                        .backend_resets_at
                        .filter(|reset| *reset > now && profile.exhausted_until.is_some());
                    profile
                })
                .collect(),
        })
    }

    pub fn save(&self, state: &AccountRuntimeState) -> Result<(), AccountRuntimeStateError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        self.save_unlocked(state)
    }

    fn save_unlocked(&self, state: &AccountRuntimeState) -> Result<(), AccountRuntimeStateError> {
        fs::create_dir_all(&self.codex_home)?;
        let wire = AccountRuntimeStateWire {
            version: ACCOUNT_RUNTIME_STATE_VERSION,
            active_profile_id: state.active_profile_id.clone(),
            selection_revision: state.selection_revision,
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
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let previous = self.load_unlocked()?;
        let merged = pool.merge_runtime_state(&previous, &previous, None);
        let snapshots = pool.snapshots();
        let mut state = runtime_state_from_snapshots(&snapshots);
        for profile in &mut state.profiles {
            profile.reset_credit_excluded_until = merged
                .profiles
                .iter()
                .find(|previous| previous.profile_id == profile.profile_id)
                .and_then(|previous| previous.reset_credit_excluded_until);
        }
        state.active_profile_id = merged.active_profile_id;
        state.selection_revision = merged.selection_revision;
        for profile in merged.profiles {
            if !state
                .profiles
                .iter()
                .any(|local| local.profile_id == profile.profile_id)
            {
                state.profiles.push(profile);
            }
        }
        self.save_unlocked(&state)?;
        pool.acknowledge_runtime_state(&state);
        Ok(())
    }

    /// Blocks until this process owns the home-scoped window-warmup lock.
    pub fn lock_window_warmup(&self) -> io::Result<std::fs::File> {
        crate::account_file::warmup_lock(&self.codex_home)
    }

    /// Claims warmup only when idle; callers skip busy pools instead of queueing generating work.
    pub fn try_lock_window_warmup(&self) -> io::Result<Option<std::fs::File>> {
        crate::account_file::try_warmup_lock(&self.codex_home)
    }

    /// Serializes automatic credit redemption across processes sharing a pool.
    pub fn try_lock_reset_credit(&self) -> io::Result<Option<std::fs::File>> {
        crate::account_file::try_reset_credit_lock(&self.codex_home)
    }

    /// Publishes a confirmed reset without replacing concurrent selections or
    /// unrelated account observations.
    pub fn record_quota_reset(
        &self,
        profile_id: &AccountProfileId,
        reset_at: DateTime<Utc>,
    ) -> Result<(), AccountRuntimeStateError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let records = crate::AccountProfileStore::new(self.codex_home.clone())
            .load_profile_records_unlocked()?;
        if !records
            .iter()
            .any(|record| &record.profile.id == profile_id)
        {
            return Err(AccountRuntimeStateError::UnavailableProfile(
                profile_id.clone(),
            ));
        }
        let mut state = self.load_unlocked()?;
        if let Some(profile) = state
            .profiles
            .iter_mut()
            .find(|entry| &entry.profile_id == profile_id)
        {
            if profile
                .quota_reset_at
                .is_some_and(|current| current >= reset_at)
                || profile
                    .quota_failure_at
                    .is_some_and(|failure| failure >= reset_at)
            {
                return Ok(());
            }
            profile.exhausted_until = None;
            profile.backend_resets_at = None;
            profile.preemptive_rotation_until = None;
            profile.quota_reset_at = Some(reset_at);
            profile.quota_reset_observed_at = Some(reset_at);
            profile.rate_limits = AccountRateLimits {
                observed_at: Some(reset_at),
                ..AccountRateLimits::default()
            };
            profile.window_warmup = None;
        } else {
            state.profiles.push(AccountRuntimeProfileState {
                reset_credit_excluded_until: None,
                profile_id: profile_id.clone(),
                exhausted_until: None,
                preemptive_rotation_until: None,
                quota_reset_at: Some(reset_at),
                quota_reset_observed_at: Some(reset_at),
                quota_failure_at: None,
                rate_limits: AccountRateLimits {
                    observed_at: Some(reset_at),
                    ..AccountRateLimits::default()
                },
                window_warmup: None,

                backend_resets_at: None,
            });
        }
        self.save_unlocked(&state)
    }

    /// Imports shared quota and warmup observations without changing execution selection or
    /// treating empty in-memory warmup as an authoritative clear.
    pub fn apply_window_warmup_to_pool(
        &self,
        pool: &AccountPool,
    ) -> Result<(), AccountRuntimeStateError> {
        let state = self.load()?;
        for profile in state.profiles {
            if let Some(reset_at) = profile
                .quota_reset_at
                .filter(|reset| Some(*reset) > profile.quota_failure_at)
            {
                let cutoff = profile.quota_reset_observed_at.unwrap_or(reset_at);
                let _ = pool.apply_quota_reset_with_cutoff(
                    &profile.profile_id,
                    reset_at,
                    cutoff,
                    crate::account_pool::QuotaResetOrigin::Shared,
                );
            }
            let _ = pool.update_rate_limits(&profile.profile_id, profile.rate_limits);
            let Some(observation) = profile.window_warmup else {
                continue;
            };
            let _ = pool.record_window_warmup(&profile.profile_id, observation);
        }
        Ok(())
    }

    /// Persists one attempt without overwriting another process's active selection or quota.
    pub fn record_window_warmup(
        &self,
        profile_id: &AccountProfileId,
        mut observation: WindowWarmupObservation,
    ) -> Result<(), AccountRuntimeStateError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let profiles = crate::AccountProfileStore::new(self.codex_home.clone());
        if !profiles
            .load_profile_records_unlocked()?
            .iter()
            .any(|record| {
                &record.profile.id == profile_id
                    && record.state == crate::AccountProfileState::Ready
                    && !record.profile.disabled
            })
        {
            return Err(AccountRuntimeStateError::UnavailableProfile(
                profile_id.clone(),
            ));
        }
        observation.infer_legacy_phase();
        let mut state = self.load_unlocked()?;
        if let Some(profile) = state
            .profiles
            .iter_mut()
            .find(|profile| &profile.profile_id == profile_id)
        {
            if profile.window_warmup.as_ref().is_some_and(|current| {
                current.attempted_at <= Utc::now() && current.compare_progress(&observation).is_gt()
            }) || profile
                .quota_reset_observed_at
                .or(profile.quota_reset_at)
                .is_some_and(|reset| reset >= observation.attempted_at)
            {
                return Ok(());
            }
            profile.window_warmup = Some(observation);
        } else {
            state.profiles.push(AccountRuntimeProfileState {
                reset_credit_excluded_until: None,
                profile_id: profile_id.clone(),
                exhausted_until: None,
                preemptive_rotation_until: None,
                quota_reset_at: None,
                quota_reset_observed_at: None,
                quota_failure_at: None,
                rate_limits: AccountRateLimits::default(),
                window_warmup: Some(observation),

                backend_resets_at: None,
            });
        }
        self.save_unlocked(&state)
    }

    /// Three-way merge of the live pool against disk. Use this after mutating
    /// warmup or quota so another process sees the observation immediately.
    pub fn synchronize(&self, pool: &AccountPool) -> Result<(), AccountRuntimeStateError> {
        let mut previous = AccountRuntimeState::default();
        self.synchronize_pool(pool, &mut previous)
    }

    /// Imports and publishes shared state only when the home transaction lock is idle.
    /// A busy writer leaves the live pool and its pending refusal acknowledgements unchanged.
    pub fn try_synchronize(&self, pool: &AccountPool) -> Result<bool, AccountRuntimeStateError> {
        let Some(_lock) = crate::account_file::try_lock(&self.codex_home)? else {
            return Ok(false);
        };
        let mut previous = AccountRuntimeState::default();
        self.synchronize_pool_unlocked(pool, &mut previous)?;
        Ok(true)
    }

    /// Applies external selections and merges observations as one cross-process transaction.
    pub(crate) fn synchronize_pool(
        &self,
        pool: &AccountPool,
        previous: &mut AccountRuntimeState,
    ) -> Result<(), AccountRuntimeStateError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        self.synchronize_pool_unlocked(pool, previous)
    }

    fn synchronize_pool_unlocked(
        &self,
        pool: &AccountPool,
        previous: &mut AccountRuntimeState,
    ) -> Result<(), AccountRuntimeStateError> {
        let remote = self.load_unlocked()?;
        let profiles = crate::AccountProfileStore::new(self.codex_home.clone());
        let records = profiles
            .manifest_path()
            .exists()
            .then(|| profiles.load_profile_records_unlocked())
            .transpose()?;
        let merged = pool.merge_runtime_state(&remote, previous, records.as_deref());
        if merged != remote {
            self.save_unlocked(&merged)?;
        }
        pool.acknowledge_runtime_state(&merged);
        *previous = merged;
        Ok(())
    }

    /// Records explicit user intent without overwriting concurrently observed quota state.
    pub fn select(
        &self,
        profile_id: AccountProfileId,
        mode: AccountSelectionMode,
    ) -> Result<(), AccountRuntimeStateError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let profiles = crate::AccountProfileStore::new(self.codex_home.clone());
        if profiles.manifest_path().exists()
            && !profiles
                .load_profile_records_unlocked()?
                .iter()
                .any(|record| {
                    record.profile.id == profile_id
                        && !record.profile.disabled
                        && record.state == crate::AccountProfileState::Ready
                })
        {
            return Err(AccountRuntimeStateError::UnavailableProfile(profile_id));
        }
        let mut state = self.load_unlocked()?;
        let force = mode == AccountSelectionMode::ForceProbe;
        if let Some(profile) = state
            .profiles
            .iter_mut()
            .find(|p| p.profile_id == profile_id)
        {
            if !force
                && profile
                    .exhausted_until
                    .is_some_and(|reset| reset > Utc::now())
            {
                return Err(AccountRuntimeStateError::CoolingDown(profile_id));
            }
            if force {
                profile.exhausted_until = None;
                profile.backend_resets_at = None;
            }
            profile.preemptive_rotation_until = None;
        }
        state.active_profile_id = Some(profile_id);
        state.selection_revision = state
            .selection_revision
            .checked_add(1)
            .ok_or(AccountRuntimeStateError::RevisionOverflow)?;
        self.save_unlocked(&state)
    }

    /// Merges a quota probe without changing the selected profile or authoritative cooldown.
    pub fn record_rate_limits(
        &self,
        profile_id: &AccountProfileId,
        mut limits: AccountRateLimits,
    ) -> Result<(), AccountRuntimeStateError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let profiles = crate::AccountProfileStore::new(self.codex_home.clone());
        if !profiles
            .load_profile_records_unlocked()?
            .iter()
            .any(|record| &record.profile.id == profile_id)
        {
            return Err(AccountRuntimeStateError::UnavailableProfile(
                profile_id.clone(),
            ));
        }
        let mut state = self.load_unlocked()?;
        if let Some(profile) = state
            .profiles
            .iter_mut()
            .find(|profile| &profile.profile_id == profile_id)
        {
            let cutoff = profile.quota_reset_observed_at.or(profile.quota_reset_at);
            if cutoff.is_some() && limits.observed_at <= cutoff {
                return Ok(());
            }
            if let Some(reset_at) = cutoff {
                limits.discard_windows_before(reset_at);
            }
            profile.rate_limits =
                crate::account_pool::merge_rate_limits_monotonic(&profile.rate_limits, limits);
        } else {
            state.profiles.push(AccountRuntimeProfileState {
                reset_credit_excluded_until: None,
                profile_id: profile_id.clone(),
                exhausted_until: None,
                preemptive_rotation_until: None,
                quota_reset_at: None,
                quota_reset_observed_at: None,
                quota_failure_at: None,
                rate_limits: limits,
                window_warmup: None,

                backend_resets_at: None,
            });
        }
        self.save_unlocked(&state)
    }

    /// Removes stale runtime observations after a profile is deleted without touching OAuth
    /// credentials or the profile manifest.
    pub fn remove_profile(
        &self,
        profile_id: &AccountProfileId,
    ) -> Result<(), AccountRuntimeStateError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let mut state = self.load_unlocked()?;
        if state.active_profile_id.as_ref() == Some(profile_id) {
            state.active_profile_id = None;
        }
        state
            .profiles
            .retain(|profile| &profile.profile_id != profile_id);
        self.save_unlocked(&state)
    }
}

fn runtime_state_from_snapshots(snapshots: &[AccountPoolSnapshot]) -> AccountRuntimeState {
    let now = Utc::now();
    AccountRuntimeState {
        selection_revision: 0,
        active_profile_id: snapshots
            .iter()
            .find(|snapshot| snapshot.is_active)
            .map(|snapshot| snapshot.profile.id.clone()),
        profiles: snapshots
            .iter()
            .map(|snapshot| AccountRuntimeProfileState {
                reset_credit_excluded_until: None,
                profile_id: snapshot.profile.id.clone(),
                exhausted_until: match &snapshot.availability {
                    AccountAvailability::Exhausted {
                        resets_at: Some(reset),
                    } if reset > &now => Some(*reset),
                    _ => None,
                },
                backend_resets_at: snapshot.backend_resets_at.filter(|reset| *reset > now),
                preemptive_rotation_until: snapshot
                    .preemptive_rotation_until
                    .filter(|reset| *reset > now),
                quota_reset_at: snapshot.quota_reset_at,
                quota_reset_observed_at: snapshot.quota_reset_observed_at,
                quota_failure_at: snapshot.quota_failure_at,
                rate_limits: snapshot.rate_limits.clone(),
                window_warmup: snapshot.window_warmup.clone(),
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
    selection_revision: u64,
    #[serde(default)]
    profiles: Vec<AccountRuntimeProfileState>,
}

#[derive(Debug, Error)]
pub enum AccountRuntimeStateError {
    #[error("account {0} is missing, disabled, or has not completed login")]
    UnavailableProfile(AccountProfileId),
    #[error(transparent)]
    ProfileStore(#[from] crate::AccountProfileStoreError),
    #[error("account {0} is cooling down; use --force to probe it now")]
    CoolingDown(AccountProfileId),
    #[error("account selection revision overflow")]
    RevisionOverflow,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("unsupported account runtime state version: {0}")]
    UnsupportedVersion(u32),
}

#[cfg(test)]
#[path = "account_runtime_quota_probe_tests.rs"]
mod quota_probe_tests;

#[cfg(test)]
#[path = "account_runtime_sync_tests.rs"]
mod sync_tests;

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn missing_runtime_state_is_empty_without_creating_a_file() {
        let temp = TempDir::new().expect("temp dir");
        let store = AccountRuntimeStateStore::new(temp.path().join("codex"));
        assert_eq!(
            store.load().expect("load missing state"),
            AccountRuntimeState::default()
        );
        assert!(!store.path().exists());
    }

    #[test]
    fn expired_cooldown_is_not_restored() {
        let temp = TempDir::new().expect("temp dir");
        let store = AccountRuntimeStateStore::new(temp.path().join("codex"));
        let profile_id = AccountProfileId::new("account-a").expect("profile id");
        store
            .save(&AccountRuntimeState {
                selection_revision: 0,
                active_profile_id: Some(profile_id.clone()),
                profiles: vec![AccountRuntimeProfileState {
                    reset_credit_excluded_until: None,
                    profile_id,
                    exhausted_until: Some(Utc::now() - Duration::minutes(1)),
                    preemptive_rotation_until: None,
                    quota_reset_at: None,
                    quota_reset_observed_at: None,
                    quota_failure_at: None,
                    rate_limits: AccountRateLimits::default(),
                    window_warmup: None,

                    backend_resets_at: None,
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
            selection_revision: 0,
            active_profile_id: Some(profile_id.clone()),
            profiles: vec![AccountRuntimeProfileState {
                reset_credit_excluded_until: None,
                profile_id,
                exhausted_until: Some(reset),
                preemptive_rotation_until: None,
                quota_reset_at: None,
                quota_reset_observed_at: None,
                quota_failure_at: None,
                rate_limits: AccountRateLimits::default(),
                window_warmup: None,

                backend_resets_at: None,
            }],
        };
        store.save(&state).expect("save state");
        assert_eq!(store.load().expect("load state"), state);
    }
}
