//! Fresh backend permission can recover quota only for the exact request-bound profile state.

use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;

use super::*;
use crate::AccountRateLimitWindow;
use crate::CodexAuth;
use crate::account_pool::QuotaProbeGuard;

/// Backend metadata retained by the usage reader; an ordinary quota snapshot is insufficient.
pub struct AccountQuotaEvidence<'a> {
    pub rate_limits: &'a [RateLimitSnapshot],
    pub ordinary_usage_allowed: Option<bool>,
    pub account_id: Option<&'a str>,
    pub user_id: Option<&'a str>,
}

/// Opaque, single-use token captured before issuing a profile-specific usage GET.
pub struct AccountQuotaProbe {
    live: QuotaProbeGuard,
    persisted: Option<AccountRuntimeProfileState>,
    credential_version: Option<String>,
    manifest_modified_at: std::time::SystemTime,
    started_at: DateTime<Utc>,
    recovery_at: DateTime<Utc>,
    account_id: String,
    user_id: String,
}

impl AccountRuntimeStateStore {
    /// Validates the exact stored seat before an explicit maintenance mutation, including ready
    /// accounts. A recovery token additionally requires exhaustion and is checked separately.
    pub fn validate_profile_auth(
        &self,
        pool: &AccountPool,
        profile_id: &AccountProfileId,
        auth: &CodexAuth,
    ) -> Result<bool, AccountRuntimeStateError> {
        let Some(_home) = crate::account_file::try_lock(&self.codex_home)? else {
            return Ok(false);
        };
        if crate::AccountPoolRuntime::is_home_suspended(&self.codex_home) {
            return Ok(false);
        }
        let profiles = crate::AccountProfileStore::new(self.codex_home.clone());
        let records = profiles.load_profile_records_unlocked()?;
        let Some(record) = records.iter().find(|record| {
            &record.profile.id == profile_id
                && !record.profile.disabled
                && record.state == crate::AccountProfileState::Ready
        }) else {
            return Ok(false);
        };
        let Some(_credential) =
            crate::account_file::try_refresh_lock(&record.profile.credential_home)?
        else {
            return Ok(false);
        };
        pool.validate_stored_profile_auth(&record.profile, auth)
            .map_err(Into::into)
    }

    /// Captures both conflict barriers without refreshing credentials or changing selection.
    /// Callers must build the usage client from `auth` and retain all permission/owner metadata.
    pub fn capture_quota_probe(
        &self,
        pool: &AccountPool,
        profile_id: &AccountProfileId,
        auth: &CodexAuth,
    ) -> Result<Option<AccountQuotaProbe>, AccountRuntimeStateError> {
        let Some(account_id) = auth.get_account_id().filter(|id| !id.trim().is_empty()) else {
            return Ok(None);
        };
        let Some(user_id) = auth
            .get_chatgpt_user_id()
            .filter(|id| !id.trim().is_empty())
        else {
            return Ok(None);
        };
        if !auth.is_chatgpt_auth() || crate::AccountPoolRuntime::is_home_suspended(&self.codex_home)
        {
            return Ok(None);
        }
        let Some(_lock) = crate::account_file::try_lock(&self.codex_home)? else {
            return Ok(None);
        };
        let profiles = crate::AccountProfileStore::new(self.codex_home.clone());
        let records = profiles.load_profile_records_unlocked()?;
        let manifest_modified_at = std::fs::metadata(profiles.manifest_path())?.modified()?;
        let Some(record) = records.iter().find(|record| {
            &record.profile.id == profile_id
                && !record.profile.disabled
                && record.state == crate::AccountProfileState::Ready
        }) else {
            return Ok(None);
        };
        let Some(_credentials) =
            crate::account_file::try_refresh_lock(&record.profile.credential_home)?
        else {
            return Ok(None);
        };
        let saved = self.load_unlocked()?;
        let persisted = saved
            .profiles
            .into_iter()
            .find(|profile| &profile.profile_id == profile_id);
        if persisted.as_ref().is_some_and(|profile| {
            profile
                .reset_credit_excluded_until
                .is_some_and(|until| until > Utc::now())
        }) {
            return Ok(None);
        }
        let Some(live) = pool.capture_quota_probe_guard(profile_id, auth)? else {
            return Ok(None);
        };
        if live.profile != record.profile {
            return Ok(None);
        }
        let latest = live.quota_reset_at.max(live.quota_failure_at).max(
            persisted
                .as_ref()
                .and_then(|profile| profile.quota_reset_at.max(profile.quota_failure_at)),
        );
        let next = latest.map(|time| time.checked_add_signed(chrono::Duration::nanoseconds(1)));
        if next == Some(None) {
            return Ok(None);
        }
        let started_at = Utc::now();
        // The shared ordering clock can be ahead after a wall-clock correction. Never use it
        // as a quota observation time or compare it with backend window reset timestamps.
        let recovery_at = (started_at - chrono::Duration::nanoseconds(1))
            .max(next.flatten().unwrap_or(DateTime::<Utc>::MIN_UTC));
        Ok(Some(AccountQuotaProbe {
            credential_version: crate::account_credentials::credential_version(
                &live.profile.credential_home,
            ),
            manifest_modified_at,
            live,
            persisted,
            started_at,
            recovery_at,
            account_id,
            user_id,
        }))
    }

    /// Replaces the old exhausted windows only after a complete, owner-matched permission read.
    /// Local or persisted failures, resets, relogin, disablement and entitlement changes win.
    pub fn reconcile_quota_probe(
        &self,
        pool: &AccountPool,
        probe: AccountQuotaProbe,
        evidence: AccountQuotaEvidence<'_>,
    ) -> Result<bool, AccountRuntimeStateError> {
        if evidence.ordinary_usage_allowed != Some(true)
            || evidence.account_id != Some(probe.account_id.as_str())
            || evidence.user_id != Some(probe.user_id.as_str())
        {
            return Ok(false);
        }
        let mut codex = evidence
            .rate_limits
            .iter()
            .filter(|snapshot| snapshot.limit_id.as_deref() == Some("codex"));
        let Some(snapshot) = codex.next() else {
            return Ok(false);
        };
        if codex.next().is_some()
            || snapshot.spend_control_reached == Some(true)
            || snapshot
                .individual_limit
                .as_ref()
                .is_some_and(|limit| limit.remaining_percent <= 0)
            || snapshot.rate_limit_reached_type.is_some()
        {
            return Ok(false);
        }
        let window = |window: &RateLimitWindow| -> Option<AccountRateLimitWindow> {
            let resets_at = window
                .resets_at
                .and_then(|time| DateTime::from_timestamp(time, 0))?;
            if !window.used_percent.is_finite()
                || !(0.0..100.0).contains(&window.used_percent)
                || window.window_minutes.is_none_or(|minutes| minutes <= 0)
                || resets_at <= probe.started_at
            {
                return None;
            }
            Some(AccountRateLimitWindow {
                used_percent: window.used_percent,
                resets_at: Some(resets_at),
                window_minutes: window.window_minutes,
            })
        };
        let Some(primary) = snapshot.primary.as_ref().and_then(window) else {
            return Ok(false);
        };
        // The normalized backend type cannot distinguish an omitted secondary field from an
        // authoritative non-applicable window. Unknown quota cannot establish that distinction.
        let Some(secondary) = snapshot.secondary.as_ref().and_then(window) else {
            return Ok(false);
        };
        let limits = AccountRateLimits {
            primary: Some(primary),
            secondary: Some(secondary),
            observed_at: Some(probe.started_at),
            window_observed_at: None,
        };
        let reset_at = probe.recovery_at;
        self.commit_quota_probe(pool, probe, limits, reset_at)
    }

    /// Applies an authenticated reset-credit `Reset` response using the same captured barriers.
    /// Receipt time validates the request interval; its captured logical epoch orders recovery.
    pub fn confirm_quota_reset(
        &self,
        pool: &AccountPool,
        probe: AccountQuotaProbe,
        reset_at: DateTime<Utc>,
    ) -> Result<bool, AccountRuntimeStateError> {
        if reset_at < probe.started_at || reset_at > Utc::now() {
            return Ok(false);
        }
        let reset_at = probe.recovery_at;
        let limits = AccountRateLimits {
            observed_at: Some(probe.started_at),
            ..AccountRateLimits::default()
        };
        self.commit_quota_probe(pool, probe, limits, reset_at)
    }

    fn commit_quota_probe(
        &self,
        pool: &AccountPool,
        probe: AccountQuotaProbe,
        limits: AccountRateLimits,
        reset_at: DateTime<Utc>,
    ) -> Result<bool, AccountRuntimeStateError> {
        let Some(_lock) = crate::account_file::try_lock(&self.codex_home)? else {
            return Ok(false);
        };
        if crate::AccountPoolRuntime::is_home_suspended(&self.codex_home) {
            return Ok(false);
        }
        let profiles = crate::AccountProfileStore::new(self.codex_home.clone());
        let records = profiles.load_profile_records_unlocked()?;
        if std::fs::metadata(profiles.manifest_path())?.modified()? != probe.manifest_modified_at {
            return Ok(false);
        }
        let Some(_credentials) =
            crate::account_file::try_refresh_lock(&probe.live.profile.credential_home)?
        else {
            return Ok(false);
        };
        if !records.iter().any(|record| {
            record.profile == probe.live.profile
                && !record.profile.disabled
                && record.state == crate::AccountProfileState::Ready
        }) || crate::account_credentials::credential_version(&probe.live.profile.credential_home)
            != probe.credential_version
        {
            return Ok(false);
        }
        let mut state = self.load_unlocked()?;
        let current = state
            .profiles
            .iter()
            .find(|profile| profile.profile_id == probe.live.profile.id);
        // Full equality is deliberately conservative: a concurrent quota or warmup writer can
        // request another read, but can never make this older response clear a newer refusal.
        if current != probe.persisted.as_ref() {
            return Ok(false);
        }
        let mut recovered = current.cloned().unwrap_or(AccountRuntimeProfileState {
            profile_id: probe.live.profile.id.clone(),
            exhausted_until: None,
            backend_resets_at: None,
            reset_credit_excluded_until: None,
            preemptive_rotation_until: None,
            quota_reset_at: None,
            quota_reset_observed_at: None,
            quota_failure_at: probe.live.quota_failure_at,
            rate_limits: AccountRateLimits::default(),
            window_warmup: None,
        });
        recovered.exhausted_until = None;
        recovered.backend_resets_at = None;
        recovered.preemptive_rotation_until = None;
        recovered.quota_reset_at = Some(reset_at);
        let reset_observed_at = probe.started_at - chrono::Duration::nanoseconds(1);
        recovered.quota_reset_observed_at = Some(reset_observed_at);
        recovered.rate_limits = limits.clone();
        recovered.window_warmup = None;
        recovered.quota_failure_at = recovered.quota_failure_at.max(probe.live.quota_failure_at);
        if let Some(profile) = state
            .profiles
            .iter_mut()
            .find(|profile| profile.profile_id == recovered.profile_id)
        {
            *profile = recovered;
        } else {
            state.profiles.push(recovered);
        }
        pool.reconcile_quota_probe_guard(&probe.live, limits, reset_at, reset_observed_at, || {
            self.save_unlocked(&state)
        })
    }
}
