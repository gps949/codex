use super::*;
use crate::AccountRuntimeProfileState;
use crate::AccountRuntimeState;

impl AccountPool {
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
            let local = AccountRuntimeProfileState {
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
                rate_limits: account.rate_limits.clone(),
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
                    if incoming.rate_limits.observed_at > local.rate_limits.observed_at {
                        result.rate_limits = incoming.rate_limits.clone();
                    }
                    // Cooldown clears (force / reset-credit) must win over a stale disk exhaustion.
                    // Taking max() previously resurrected exhausted_until after a successful redeem.
                    let old_exhausted = old.and_then(|profile| profile.exhausted_until);
                    if local.exhausted_until != old_exhausted {
                        result.exhausted_until = local.exhausted_until;
                    } else if incoming.exhausted_until != old_exhausted {
                        result.exhausted_until = incoming.exhausted_until;
                    }
                }
                if external_selection
                    && remote.active_profile_id.as_ref() == Some(&local.profile_id)
                {
                    // An explicit selection from another process may clear or set cooldown.
                    result.exhausted_until = incoming.exhausted_until;
                }
            }
            if account.rate_limits != result.rate_limits {
                account.rate_limits = result.rate_limits.clone();
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
        drop(state);
        if changed {
            self.notify_change();
        }
        merged
    }
}
