//! Atomic eligibility and durable duplicate protection at the maintenance send boundary.

use super::*;

impl AccountRuntimeStateStore {
    /// Returns true only after this attempt is durably claimed for a ready standby.
    /// Reset epochs and a competing protected attempt reject the send instead of silently
    /// accepting a stale observation that was not written.
    pub fn claim_window_warmup(
        &self,
        profile_id: &AccountProfileId,
        attempted_at: DateTime<Utc>,
    ) -> Result<bool, AccountRuntimeStateError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        if crate::AccountPoolRuntime::is_home_suspended(&self.codex_home) {
            return Ok(false);
        }
        let profiles = crate::AccountProfileStore::new(self.codex_home.clone());
        if !profiles
            .load_profile_records_unlocked()?
            .iter()
            .any(|record| {
                &record.profile.id == profile_id
                    && !record.profile.disabled
                    && record.state == crate::AccountProfileState::Ready
            })
        {
            return Ok(false);
        }
        let mut state = self.load_unlocked()?;
        if state.active_profile_id.as_ref() == Some(profile_id) {
            return Ok(false);
        }
        let observation = WindowWarmupObservation::in_progress(attempted_at);
        if let Some(profile) = state
            .profiles
            .iter_mut()
            .find(|profile| &profile.profile_id == profile_id)
        {
            let now = Utc::now();
            if profile
                .quota_reset_at
                .is_some_and(|reset| reset >= attempted_at)
                || profile.window_warmup.as_ref().is_some_and(|current| {
                    if current.attempted_at == attempted_at {
                        // Only a definite rejection may retry the still-pending attempt.
                        current.phase != Some(crate::WindowWarmupPhase::InProgress)
                    } else {
                        current.attempted_at <= now
                            && (current.compare_progress(&observation).is_gt()
                                || current.request_is_protected(&profile.rate_limits, now))
                    }
                })
            {
                return Ok(false);
            }
            profile.window_warmup = Some(observation);
        } else {
            state.profiles.push(AccountRuntimeProfileState {
                reset_credit_excluded_until: None,
                profile_id: profile_id.clone(),
                exhausted_until: None,
                preemptive_rotation_until: None,
                quota_reset_at: None,
                rate_limits: AccountRateLimits::default(),
                window_warmup: Some(observation),

                backend_resets_at: None,
            });
        }
        self.save_unlocked(&state)?;
        Ok(true)
    }
}
