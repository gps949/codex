//! Preserves the reason reset credits cannot repair an entitlement cooldown.

use super::*;

impl AccountRuntimeStateStore {
    /// Records a bounded no-redemption deadline without changing selection or quota.
    pub fn exclude_reset_credit_until(
        &self,
        profile_id: &AccountProfileId,
        until: DateTime<Utc>,
    ) -> Result<(), AccountRuntimeStateError> {
        let _lock = crate::account_file::lock(&self.codex_home)?;
        let mut state = self.load_unlocked()?;
        if let Some(profile) = state
            .profiles
            .iter_mut()
            .find(|profile| &profile.profile_id == profile_id)
        {
            profile.reset_credit_excluded_until =
                Some(until.min(Utc::now() + chrono::Duration::minutes(10)));
        } else {
            state.profiles.push(AccountRuntimeProfileState {
                profile_id: profile_id.clone(),
                exhausted_until: None,
                reset_credit_excluded_until: Some(
                    until.min(Utc::now() + chrono::Duration::minutes(10)),
                ),
                preemptive_rotation_until: None,
                quota_reset_at: None,
                rate_limits: AccountRateLimits::default(),
                window_warmup: None,
            });
        }
        self.save_unlocked(&state)
    }
}
