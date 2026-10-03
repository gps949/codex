//! Bounded, process-owned scheduling for passive quota discovery.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use std::time::SystemTime;

use codex_login::AccountPoolSnapshot;
use codex_login::AccountProfileId;
use codex_login::AuthChangeState;
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

#[derive(Default)]
struct ProbeState {
    cursor: usize,
    records: HashMap<AccountProfileId, ProbeRecord>,
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
        let limit = match mode {
            RecoveryProbeMode::Batch => MAX_PROFILES_PER_PASS,
            RecoveryProbeMode::Coverage => MAX_COVERAGE_PROFILES,
        };
        let start = state.cursor % keys.len();
        let selected: Vec<_> = (0..keys.len())
            .filter_map(|offset| {
                let index = (start + offset) % keys.len();
                let key = &keys[index];
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
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        !keys.is_empty()
            && keys.len() <= MAX_COVERAGE_PROFILES
            && keys.iter().all(|key| {
                state
                    .records
                    .get(&key.snapshot.profile.id)
                    .is_some_and(|record| {
                        record.key == *key
                            && record.rejected_at.is_some_and(|rejected| {
                                now.duration_since(rejected) < PROBE_CADENCE
                            })
                    })
            })
    }
}

impl RecoveryProbeLeader<'_> {
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
