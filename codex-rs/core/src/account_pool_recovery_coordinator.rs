//! Bounded, process-owned scheduling for passive quota discovery.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use std::time::SystemTime;

use codex_login::AccountPoolSnapshot;
use codex_login::AccountProfileId;
use codex_login::AuthChangeState;
use sha1::Digest;
use tokio::sync::watch;
use tokio::time::Instant;

pub(crate) const MAX_PROFILES_PER_PASS: usize = 4;
pub(crate) const MAX_COVERAGE_PROFILES: usize = 64;
const PROBE_CADENCE: Duration = Duration::from_secs(30);

/// Quota, profile and authentication changes invalidate a previous permission read.
#[derive(Clone, PartialEq)]
pub(crate) struct RecoveryProbeKey {
    pub(crate) snapshot: AccountPoolSnapshot,
    pub(crate) auth_revision: AuthChangeState,
    pub(crate) manifest_version: SystemTime,
    pub(crate) credential_versions: [Option<(SystemTime, u64)>; 2],
    pub(crate) quota_owner: Option<[u8; 20]>,
}

/// Keeps quota-owner coordination independent of refreshed tokens and local profile aliases.
pub(crate) fn quota_owner_key(account_id: &str, user_id: &str) -> [u8; 20] {
    let mut digest = sha1::Sha1::new();
    digest.update(account_id.len().to_le_bytes());
    digest.update(account_id.as_bytes());
    digest.update(user_id.len().to_le_bytes());
    digest.update(user_id.as_bytes());
    digest.finalize().into()
}

#[derive(Clone, Copy)]
pub(crate) enum RecoveryProbeMode {
    Batch,
    Coverage,
}

#[derive(Clone, Copy)]
pub(crate) enum RecoveryProbeVerdict {
    Recovered,
    Rejected,
    Unknown,
}

struct ProbeRecord {
    key: RecoveryProbeKey,
    attempted_at: Option<Instant>,
    rejected_at: Option<Instant>,
}

struct RecoveryGrant {
    key: RecoveryProbeKey,
    granted_at: Instant,
}

#[derive(Default)]
struct ProbeState {
    cursor: usize,
    records: HashMap<AccountProfileId, ProbeRecord>,
    recovery_grants: HashMap<[u8; 20], RecoveryGrant>,
    pending: Option<watch::Sender<bool>>,
}

#[derive(Default)]
pub(crate) struct RecoveryProbeCoordinator {
    state: Mutex<ProbeState>,
}

pub(crate) enum RecoveryProbeAttempt<'a> {
    Leader(RecoveryProbeLeader<'a>),
    Follower(watch::Receiver<bool>),
    NoWork,
}

pub(crate) struct RecoveryProbeLeader<'a> {
    coordinator: &'a RecoveryProbeCoordinator,
    pub(crate) keys: Vec<RecoveryProbeKey>,
    completion: watch::Sender<bool>,
}

impl RecoveryProbeCoordinator {
    /// Reserves profile IDs before any network await. Concurrent turns join the same pass.
    pub(crate) fn begin(
        &self,
        keys: &[RecoveryProbeKey],
        mode: RecoveryProbeMode,
    ) -> RecoveryProbeAttempt<'_> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = &state.pending {
            return RecoveryProbeAttempt::Follower(pending.subscribe());
        }
        if keys.is_empty()
            || matches!(mode, RecoveryProbeMode::Coverage) && keys.len() > MAX_COVERAGE_PROFILES
        {
            return RecoveryProbeAttempt::NoWork;
        }
        state.records.retain(|id, record| {
            keys.iter()
                .any(|key| key.snapshot.profile.id == *id && key == &record.key)
        });
        let now = Instant::now();
        state
            .recovery_grants
            .retain(|_, grant| now.duration_since(grant.granted_at) < PROBE_CADENCE);
        let limit = match mode {
            RecoveryProbeMode::Batch => MAX_PROFILES_PER_PASS,
            RecoveryProbeMode::Coverage => MAX_COVERAGE_PROFILES,
        };
        let start = state.cursor % keys.len();
        let selected: Vec<_> = (0..keys.len())
            .filter_map(|offset| {
                let index = (start + offset) % keys.len();
                let key = &keys[index];
                // A fresh GET that restored this seat already authorized one inference probe.
                // A subsequent refusal changes the conflict key, not the backend window.
                let recently_recovered = key
                    .quota_owner
                    .and_then(|owner| state.recovery_grants.get(&owner))
                    .is_some_and(|grant| {
                        grant
                            .key
                            .snapshot
                            .rate_limits
                            .primary
                            .iter()
                            .chain(grant.key.snapshot.rate_limits.secondary.iter())
                            .all(|window| {
                                window
                                    .resets_at
                                    .is_none_or(|reset| reset > chrono::Utc::now())
                            })
                    });
                if recently_recovered {
                    return None;
                }
                let due = state
                    .records
                    .get(&key.snapshot.profile.id)
                    .is_none_or(|record| {
                        record
                            .attempted_at
                            .is_none_or(|at| now.duration_since(at) >= PROBE_CADENCE)
                    });
                due.then_some((index, key.clone()))
            })
            .take(limit)
            .collect();
        let Some((last, _)) = selected.last() else {
            return RecoveryProbeAttempt::NoWork;
        };
        state.cursor = (last + 1) % keys.len();
        let selected: Vec<_> = selected.into_iter().map(|(_, key)| key).collect();
        for key in &selected {
            state.records.insert(
                key.snapshot.profile.id.clone(),
                ProbeRecord {
                    key: key.clone(),
                    attempted_at: None,
                    rejected_at: None,
                },
            );
        }
        // Large pools still rotate fairly, while process lifetime bookkeeping stays bounded.
        while state.records.len() > MAX_COVERAGE_PROFILES {
            let Some(oldest) = state
                .records
                .iter()
                .min_by_key(|(_, record)| record.attempted_at.unwrap_or(now))
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            state.records.remove(&oldest);
        }
        let (completion, _) = watch::channel(false);
        state.pending = Some(completion.clone());
        RecoveryProbeAttempt::Leader(RecoveryProbeLeader {
            coordinator: self,
            keys: selected,
            completion,
        })
    }

    /// Only fresh, owner-validated backend denials establish complete exhausted-pool coverage.
    pub(crate) fn covers(&self, keys: &[RecoveryProbeKey]) -> bool {
        !keys.is_empty() && keys.len() <= MAX_COVERAGE_PROFILES && self.checked(keys) == keys.len()
    }

    /// Counts only fresh, owner-validated denials for the current profile conflict keys.
    pub(crate) fn checked(&self, keys: &[RecoveryProbeKey]) -> usize {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        keys.iter()
            .filter(|key| {
                state
                    .records
                    .get(&key.snapshot.profile.id)
                    .is_some_and(|record| {
                        record.key == **key
                            && record.rejected_at.is_some_and(|rejected| {
                                now.duration_since(rejected) < PROBE_CADENCE
                            })
                    })
            })
            .count()
    }
}

impl RecoveryProbeLeader<'_> {
    pub(crate) fn recovered(
        &self,
        key: &RecoveryProbeKey,
        snapshot: AccountPoolSnapshot,
        quota_owner: [u8; 20],
    ) {
        let mut state = self
            .coordinator
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut key = key.clone();
        key.snapshot = snapshot;
        state.recovery_grants.insert(
            quota_owner,
            RecoveryGrant {
                key,
                granted_at: Instant::now(),
            },
        );
        while state.recovery_grants.len() > MAX_COVERAGE_PROFILES {
            let Some(oldest) = state
                .recovery_grants
                .iter()
                .min_by_key(|(_, grant)| grant.granted_at)
                .map(|(id, _)| *id)
            else {
                break;
            };
            state.recovery_grants.remove(&oldest);
        }
    }

    pub(crate) fn started(&self, key: &RecoveryProbeKey) {
        if let Some(record) = self
            .coordinator
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .get_mut(&key.snapshot.profile.id)
            && record.key == *key
        {
            record.attempted_at = Some(Instant::now());
        }
    }

    pub(crate) fn observe(&self, key: &RecoveryProbeKey, verdict: RecoveryProbeVerdict) {
        let mut state = self
            .coordinator
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(record) = state.records.get_mut(&key.snapshot.profile.id)
            && record.key == *key
            && matches!(verdict, RecoveryProbeVerdict::Rejected)
        {
            record.rejected_at = Some(Instant::now());
        }
    }
}

impl Drop for RecoveryProbeLeader<'_> {
    fn drop(&mut self) {
        {
            let mut state = self
                .coordinator
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .records
                .retain(|_, record| record.attempted_at.is_some());
            state.pending = None;
        }
        self.completion.send_replace(true);
    }
}
