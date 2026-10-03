//! Applies request-bound recovery while holding the live pool's conflict barrier.

use super::*;
use crate::AccountRuntimeStateError;
use crate::AuthChangeState;
use crate::CodexAuth;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuotaResetOrigin {
    Confirmed,
    Shared,
}

pub(crate) struct QuotaProbeGuard {
    pub(crate) profile: AccountProfile,
    auth_manager: Arc<AuthManager>,
    auth_revision: AuthChangeState,
    availability: AccountAvailability,
    pub(crate) quota_reset_at: Option<DateTime<Utc>>,
    quota_reset_observed_at: Option<DateTime<Utc>>,
    pub(crate) quota_failure_at: Option<DateTime<Utc>>,
    pub(crate) rate_limits: AccountRateLimits,
}

impl AccountPool {
    pub(crate) fn validate_stored_profile_auth(
        &self,
        profile: &AccountProfile,
        auth: &CodexAuth,
    ) -> std::io::Result<bool> {
        let state = self.lock_state();
        let Some(account) = state.accounts.get(&profile.id) else {
            return Ok(false);
        };
        if account.profile != *profile
            || matches!(
                account.availability,
                AccountAvailability::Disabled
                    | AccountAvailability::AuthenticationUnavailable { .. }
            )
        {
            return Ok(false);
        }
        let revision = account.auth_manager.auth_change_state_receiver();
        let before = *revision.borrow();
        Ok(account
            .auth_manager
            .stored_quota_probe_auth_matches(&profile.credential_home, auth)?
            && *revision.borrow() == before)
    }

    pub(crate) fn capture_quota_probe_guard(
        &self,
        profile_id: &AccountProfileId,
        auth: &CodexAuth,
    ) -> std::io::Result<Option<QuotaProbeGuard>> {
        let state = self.lock_state();
        let Some(account) = state.accounts.get(profile_id) else {
            return Ok(None);
        };
        let Some(cached) = account.auth_manager.auth_cached() else {
            return Ok(None);
        };
        let changes = account.auth_manager.auth_change_state_receiver();
        let auth_revision = *changes.borrow();
        if account.profile.disabled
            || !matches!(account.availability, AccountAvailability::Exhausted { .. })
            || !cached.is_chatgpt_auth()
            || cached.get_account_id() != auth.get_account_id()
            || cached.get_chatgpt_user_id() != auth.get_chatgpt_user_id()
            || account
                .auth_manager
                .refresh_failure_for_auth(&cached)
                .is_some()
            || !account
                .auth_manager
                .stored_quota_probe_auth_matches(&account.profile.credential_home, auth)?
            || *changes.borrow() != auth_revision
        {
            return Ok(None);
        }
        Ok(Some(QuotaProbeGuard {
            profile: account.profile.clone(),
            auth_manager: Arc::clone(&account.auth_manager),
            auth_revision,
            availability: account.availability.clone(),
            quota_reset_at: account.quota_reset_at,
            quota_reset_observed_at: account.quota_reset_observed_at,
            quota_failure_at: account.quota_failure_at,
            rate_limits: account.rate_limits.clone(),
        }))
    }

    pub(crate) fn reconcile_quota_probe_guard(
        &self,
        guard: &QuotaProbeGuard,
        limits: AccountRateLimits,
        reset_at: DateTime<Utc>,
        reset_observed_at: DateTime<Utc>,
        persist: impl FnOnce() -> Result<(), AccountRuntimeStateError>,
    ) -> Result<bool, AccountRuntimeStateError> {
        let mut state = self.lock_state();
        let Some(account) = state.accounts.get_mut(&guard.profile.id) else {
            return Ok(false);
        };
        if account.profile != guard.profile
            || !Arc::ptr_eq(&account.auth_manager, &guard.auth_manager)
            || *account.auth_manager.auth_change_state_receiver().borrow() != guard.auth_revision
            || account.availability != guard.availability
            || account.quota_failure_at != guard.quota_failure_at
            || account.quota_reset_at != guard.quota_reset_at
            || account.quota_reset_observed_at != guard.quota_reset_observed_at
            || account.rate_limits != guard.rate_limits
            || account.auth_manager.auth_cached().is_none_or(|auth| {
                !auth.is_chatgpt_auth()
                    || account
                        .auth_manager
                        .refresh_failure_for_auth(&auth)
                        .is_some()
            })
        {
            return Ok(false);
        }
        let Some(auth) = account.auth_manager.auth_cached() else {
            return Ok(false);
        };
        if !account
            .auth_manager
            .stored_quota_probe_auth_matches(&account.profile.credential_home, &auth)?
            || *account.auth_manager.auth_change_state_receiver().borrow() != guard.auth_revision
        {
            return Ok(false);
        }
        // Persistence happens before publication, inside both locks. A failed write leaves the
        // in-memory cooldown intact, and local refusals cannot race between these steps.
        persist()?;
        account.availability = AccountAvailability::Available;
        account.backend_resets_at = None;
        account.preemptive_rotation_until = None;
        account.quota_reset_at = Some(reset_at);
        account.quota_reset_observed_at = Some(reset_observed_at);
        account.quota_failure_pending = false;
        account.rate_limits = limits;
        account.window_warmup = None;
        account.last_active_generation = None;
        if state.active_profile.as_ref() == Some(&guard.profile.id) {
            state.generation = state.generation.wrapping_add(1);
            let generation = state.generation;
            if let Some(account) = state.accounts.get_mut(&guard.profile.id) {
                account.last_active_generation = Some(generation);
            }
        }
        drop(state);
        self.notify_change();
        Ok(true)
    }

    pub(crate) fn apply_quota_reset_with_cutoff(
        &self,
        profile_id: &AccountProfileId,
        reset_at: DateTime<Utc>,
        cutoff_at: DateTime<Utc>,
        origin: QuotaResetOrigin,
    ) -> Result<(), AccountPoolError> {
        let mut state = self.lock_state();
        let is_active = state.active_profile.as_ref() == Some(profile_id);
        let generation = if is_active {
            state.generation.wrapping_add(1)
        } else {
            state.generation
        };
        let account = state
            .accounts
            .get_mut(profile_id)
            .ok_or_else(|| AccountPoolError::UnknownProfile(profile_id.clone()))?;
        // A shared logical epoch may be ahead after a clock correction. Preserve an unsaved
        // local refusal until full synchronization restamps it against that shared clock.
        if (origin == QuotaResetOrigin::Shared && account.quota_failure_pending)
            || account
                .quota_reset_at
                .is_some_and(|current| current >= reset_at)
            || account
                .quota_failure_at
                .is_some_and(|failure| failure >= reset_at)
        {
            return Ok(());
        }
        account.quota_reset_at = Some(reset_at);
        account.quota_reset_observed_at = Some(cutoff_at);
        account.quota_failure_pending = false;
        account.backend_resets_at = None;
        account.rate_limits = AccountRateLimits {
            observed_at: Some(cutoff_at),
            ..AccountRateLimits::default()
        };
        account.window_warmup = None;
        account.preemptive_rotation_until = None;
        account.last_active_generation = is_active.then_some(generation);
        if matches!(account.availability, AccountAvailability::Exhausted { .. }) {
            account.availability = AccountAvailability::Available;
        }
        state.generation = generation;
        drop(state);
        self.notify_change();
        Ok(())
    }
}
