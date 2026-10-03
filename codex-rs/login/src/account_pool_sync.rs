use super::*;
use crate::AccountRuntimeProfileState;
use crate::AccountRuntimeState;

impl AccountPool {
    pub(crate) fn acknowledge_runtime_state(&self, saved: &AccountRuntimeState) {
        let mut state = self.lock_state();
        for account in state.accounts.values_mut() {
            if saved.profiles.iter().any(|profile| {
                profile.profile_id == account.profile.id
                    && profile.quota_failure_at == account.quota_failure_at
            }) {
                account.quota_failure_pending = false;
            }
        }
    }

    /// Three-way merge under the pool mutex. Disk changes win selection conflicts, while
    /// unchanged local observations never overwrite another process's newer quota state.
    pub(crate) fn merge_runtime_state(
        &self,
        remote: &AccountRuntimeState,
        previous: &AccountRuntimeState,
        profiles: Option<&[crate::AccountProfileRecord]>,
    ) -> AccountRuntimeState {
        let mut state = self.lock_state();
        let now = Utc::now();
        let external_selection = remote.selection_revision != previous.selection_revision;
        let mut merged = remote.clone();
        let mut changed = false;
        let mut active_quota_reset = false;
        let active_profile = state.active_profile.clone();
        if let Some(profiles) = profiles {
            let before = state.accounts.len();
            state.accounts.retain(|id, _| {
                profiles.iter().any(|record| {
                    &record.profile.id == id && record.state == crate::AccountProfileState::Ready
                })
            });
            changed |= before != state.accounts.len();
            merged.profiles.retain(|entry| {
                profiles
                    .iter()
                    .any(|record| record.profile.id == entry.profile_id)
            });
        }
        for account in state.accounts.values_mut() {
            let incoming = remote
                .profiles
                .iter()
                .find(|entry| entry.profile_id == account.profile.id);
            if account.quota_failure_pending {
                // Import the shared logical clock before publishing an unsaved local refusal.
                // This also orders it after a concurrent recovery when the wall clock moved back.
                let next =
                    account
                        .quota_failure_at
                        .max(account.quota_reset_at)
                        .max(incoming.and_then(|profile| {
                            profile.quota_failure_at.max(profile.quota_reset_at)
                        }))
                        .map(|time| {
                            time.checked_add_signed(chrono::Duration::nanoseconds(1))
                                .unwrap_or(DateTime::<Utc>::MAX_UTC)
                        });
                account.quota_failure_at =
                    Some(Utc::now().max(next.unwrap_or(DateTime::<Utc>::MIN_UTC)));
                changed = true;
            }
            if let Some(record) = profiles.and_then(|profiles| {
                profiles
                    .iter()
                    .find(|record| record.profile.id == account.profile.id)
            }) {
                let was_disabled = account.profile.disabled;
                changed |= account.profile != record.profile;
                account.profile = record.profile.clone();
                if account.profile.disabled {
                    changed |= account.availability != AccountAvailability::Disabled;
                    account.availability = AccountAvailability::Disabled;
                    account.preemptive_rotation_until = None;
                    account.last_active_generation = None;
                } else if was_disabled {
                    account.availability = match incoming
                        .and_then(|entry| entry.exhausted_until)
                        .filter(|reset| *reset > now)
                    {
                        Some(reset) => AccountAvailability::Exhausted {
                            resets_at: Some(reset),
                        },
                        None => AccountAvailability::Available,
                    };
                }
            }
            if account.profile.disabled {
                account.quota_failure_pending = false;
            }
            let local = AccountRuntimeProfileState {
                reset_credit_excluded_until: incoming
                    .and_then(|entry| entry.reset_credit_excluded_until),
                profile_id: account.profile.id.clone(),
                exhausted_until: match account.availability {
                    AccountAvailability::Exhausted {
                        resets_at: Some(reset),
                    } if reset > now => Some(reset),
                    AccountAvailability::Disabled => {
                        incoming.and_then(|entry| entry.exhausted_until)
                    }
                    _ => None,
                },
                backend_resets_at: account.backend_resets_at,
                preemptive_rotation_until: account.preemptive_rotation_until,
                quota_reset_at: account.quota_reset_at,
                quota_reset_observed_at: account.quota_reset_observed_at,
                quota_failure_at: account.quota_failure_at,
                rate_limits: account.rate_limits.clone(),
                window_warmup: account.window_warmup.clone(),
            };
            let old = previous
                .profiles
                .iter()
                .find(|p| p.profile_id == local.profile_id);
            let mut result = local.clone();
            if let Some(incoming) = incoming {
                if old == Some(&local) {
                    result = incoming.clone();
                } else {
                    // Cooldown clears (force / reset-credit) must win over a stale disk exhaustion.
                    // Taking max() previously resurrected exhausted_until after a successful redeem.
                    let old_exhausted = old.and_then(|profile| profile.exhausted_until);
                    if local.exhausted_until != old_exhausted {
                        result.exhausted_until = local.exhausted_until;
                    } else if incoming.exhausted_until != old_exhausted {
                        result.exhausted_until = incoming.exhausted_until;
                    }
                    let old_preemptive = old.and_then(|profile| profile.preemptive_rotation_until);
                    let old_backend_reset = old.and_then(|profile| profile.backend_resets_at);
                    if local.backend_resets_at != old_backend_reset {
                        result.backend_resets_at = local.backend_resets_at;
                    } else if incoming.backend_resets_at != old_backend_reset {
                        result.backend_resets_at = incoming.backend_resets_at;
                    }
                    if local.preemptive_rotation_until != old_preemptive {
                        result.preemptive_rotation_until = local.preemptive_rotation_until;
                    } else if incoming.preemptive_rotation_until != old_preemptive {
                        result.preemptive_rotation_until = incoming.preemptive_rotation_until;
                    }
                    result.window_warmup = merge_window_warmup(
                        local.window_warmup.clone(),
                        incoming.window_warmup.clone(),
                        old.and_then(|profile| profile.window_warmup.clone()),
                    );
                }
                if external_selection
                    && remote.active_profile_id.as_ref() == Some(&local.profile_id)
                {
                    // An explicit selection from another process may clear or set cooldown.
                    result.exhausted_until = incoming.exhausted_until;
                    result.backend_resets_at = incoming.backend_resets_at;
                    result.preemptive_rotation_until = incoming.preemptive_rotation_until;
                }
            }
            if let Some(incoming) = incoming {
                result.quota_reset_at = local.quota_reset_at.max(incoming.quota_reset_at);
                // Keep the real cutoff paired with the winning logical reset epoch.
                result.quota_reset_observed_at =
                    match incoming.quota_reset_at.cmp(&local.quota_reset_at) {
                        std::cmp::Ordering::Greater => incoming.quota_reset_observed_at,
                        std::cmp::Ordering::Less => local.quota_reset_observed_at,
                        std::cmp::Ordering::Equal => local
                            .quota_reset_observed_at
                            .max(incoming.quota_reset_observed_at),
                    };
                result.quota_failure_at = local.quota_failure_at.max(incoming.quota_failure_at);
                let local_reset = local
                    .quota_reset_at
                    .filter(|reset| Some(*reset) > incoming.quota_failure_at);
                let incoming_reset = incoming
                    .quota_reset_at
                    .filter(|reset| Some(*reset) > local.quota_failure_at);
                result.rate_limits = match incoming_reset.cmp(&local_reset) {
                    std::cmp::Ordering::Greater => {
                        result.exhausted_until = incoming.exhausted_until;
                        result.backend_resets_at = incoming.backend_resets_at;
                        result.preemptive_rotation_until = incoming.preemptive_rotation_until;
                        result.window_warmup = incoming.window_warmup.clone();
                        incoming.rate_limits.clone()
                    }
                    std::cmp::Ordering::Less => {
                        result.exhausted_until = local.exhausted_until;
                        result.backend_resets_at = local.backend_resets_at;
                        result.preemptive_rotation_until = local.preemptive_rotation_until;
                        result.window_warmup = local.window_warmup.clone();
                        local.rate_limits.clone()
                    }
                    std::cmp::Ordering::Equal => {
                        let mut local_limits = local.rate_limits.clone();
                        let mut incoming_limits = incoming.rate_limits.clone();
                        if let Some(reset_at) =
                            result.quota_reset_observed_at.or(result.quota_reset_at)
                        {
                            local_limits.discard_windows_before(reset_at);
                            incoming_limits.discard_windows_before(reset_at);
                        }
                        merge_rate_limits_monotonic(&local_limits, incoming_limits)
                    }
                };
                // A refusal received after the recovery request began wins across processes.
                let failure = if local.quota_failure_at > incoming.quota_failure_at {
                    &local
                } else {
                    incoming
                };
                if local.quota_failure_at != incoming.quota_failure_at
                    && failure.quota_failure_at >= result.quota_reset_at
                {
                    result.exhausted_until = failure.exhausted_until;
                    result.backend_resets_at = failure.backend_resets_at;
                    result.preemptive_rotation_until = failure.preemptive_rotation_until;
                }
            }
            if let Some(observation) = result.window_warmup.as_mut() {
                observation.infer_legacy_phase();
            }
            if let Some(reset_at) = result.quota_reset_observed_at.or(result.quota_reset_at) {
                result.rate_limits.discard_windows_before(reset_at);
            }
            if result.window_warmup.as_ref().is_some_and(|observation| {
                result
                    .quota_reset_observed_at
                    .or(result.quota_reset_at)
                    .is_some_and(|reset| reset >= observation.attempted_at)
            }) {
                result.window_warmup = None;
            }
            if account.quota_reset_at != result.quota_reset_at {
                account.quota_reset_at = result.quota_reset_at;
                account.last_active_generation = None;
                active_quota_reset |= active_profile.as_ref() == Some(&account.profile.id);
                changed = true;
            }
            if account.quota_reset_observed_at != result.quota_reset_observed_at {
                account.quota_reset_observed_at = result.quota_reset_observed_at;
                changed = true;
            }
            if account.quota_failure_at != result.quota_failure_at {
                account.quota_failure_at = result.quota_failure_at;
                changed = true;
            }
            result.preemptive_rotation_until = result
                .preemptive_rotation_until
                .filter(|reset| *reset > now && result.exhausted_until.is_none());
            result.backend_resets_at = result
                .backend_resets_at
                .filter(|reset| *reset > now && result.exhausted_until.is_some());
            if matches!(
                account.availability,
                AccountAvailability::Disabled
                    | AccountAvailability::AuthenticationUnavailable { .. }
            ) {
                result.backend_resets_at = None;
            }
            if account.backend_resets_at != result.backend_resets_at {
                account.backend_resets_at = result.backend_resets_at;
                changed = true;
            }
            if account.preemptive_rotation_until != result.preemptive_rotation_until {
                account.preemptive_rotation_until = result.preemptive_rotation_until;
                changed = true;
            }
            if account.rate_limits != result.rate_limits {
                account.rate_limits = result.rate_limits.clone();
                changed = true;
            }
            if account.window_warmup != result.window_warmup {
                account.window_warmup = result.window_warmup.clone();
                changed = true;
            }
            if confirm_started_window_warmup(account) {
                result.window_warmup = account.window_warmup.clone();
                changed = true;
            }
            if (local.exhausted_until != result.exhausted_until
                || (external_selection
                    && remote.active_profile_id.as_ref() == Some(&local.profile_id)))
                && matches!(
                    account.availability,
                    AccountAvailability::Available | AccountAvailability::Exhausted { .. }
                )
            {
                account.availability = match result.exhausted_until.filter(|reset| *reset > now) {
                    Some(reset) => AccountAvailability::Exhausted {
                        resets_at: Some(reset),
                    },
                    None => AccountAvailability::Available,
                };
                changed = true;
            }
            if let Some(entry) = merged
                .profiles
                .iter_mut()
                .find(|p| p.profile_id == result.profile_id)
            {
                *entry = result;
            } else {
                merged.profiles.push(result);
            }
        }
        if active_quota_reset {
            state.generation = state.generation.wrapping_add(1);
            let generation = state.generation;
            if let Some(account) = active_profile.and_then(|id| state.accounts.get_mut(&id)) {
                account.last_active_generation = Some(generation);
            }
        }
        let desired =
            if external_selection || remote.active_profile_id != previous.active_profile_id {
                remote.active_profile_id.as_ref()
            } else {
                state.active_profile.as_ref()
            };
        // Keep the selected profile even while it cools down. Clearing it made restart look
        // "logged out" whenever every account was exhausted, and undid force/reset reactivation.
        let desired = desired
            .filter(|id| state.accounts.contains_key(id))
            .cloned()
            .or_else(|| select_eligible_account(&state, &now))
            .or_else(|| {
                state
                    .active_profile
                    .clone()
                    .filter(|id| state.accounts.contains_key(id))
            });
        if let Some(id) = desired {
            changed |= set_active_profile(&mut state, &id);
        } else if state
            .active_profile
            .as_ref()
            .is_some_and(|id| !state.accounts.contains_key(id))
        {
            state.active_profile = None;
            state.generation = state.generation.wrapping_add(1);
            changed = true;
        }
        // A process started before a profile was added cannot activate it yet. Never let
        // that stale pool erase the user's selection for newer processes.
        if remote.active_profile_id.as_ref().is_none_or(|id| {
            state.accounts.contains_key(id)
                || profiles
                    .is_some_and(|records| !records.iter().any(|record| &record.profile.id == id))
        }) {
            merged.active_profile_id = state.active_profile.clone();
        }
        // HashMap iteration order is nondeterministic; keep persisted profiles stable and
        // aligned with AccountPool::snapshots (priority, then id).
        merged.profiles.sort_by(|left, right| {
            let priority_for = |profile_id: &AccountProfileId| {
                state
                    .accounts
                    .get(profile_id)
                    .map(|account| account.profile.priority)
                    .or_else(|| {
                        profiles.and_then(|records| {
                            records
                                .iter()
                                .find(|record| &record.profile.id == profile_id)
                                .map(|record| record.profile.priority)
                        })
                    })
                    .unwrap_or(u32::MAX)
            };
            priority_for(&left.profile_id)
                .cmp(&priority_for(&right.profile_id))
                .then_with(|| left.profile_id.as_str().cmp(right.profile_id.as_str()))
        });
        drop(state);
        if changed {
            self.notify_change();
        }
        merged
    }
}

fn merge_window_warmup(
    local: Option<crate::WindowWarmupObservation>,
    incoming: Option<crate::WindowWarmupObservation>,
    old: Option<crate::WindowWarmupObservation>,
) -> Option<crate::WindowWarmupObservation> {
    if local != old && incoming != old {
        match (&local, &incoming) {
            (Some(local_observation), Some(incoming_observation))
                if incoming_observation
                    .compare_progress(local_observation)
                    .is_gt() =>
            {
                incoming
            }
            _ => local,
        }
    } else if local != old {
        local
    } else if incoming != old {
        incoming
    } else {
        local
    }
}
