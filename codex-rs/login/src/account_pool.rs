use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::auth::AuthManager;

static NEXT_PROFILE_ID: AtomicU64 = AtomicU64::new(1);

/// Stable identifier for one independently authenticated Codex account profile.
///
/// A profile id is deliberately independent from the ChatGPT account id. It is allocated
/// before OAuth begins so file and keyring-backed credentials can use their final storage
/// location for the entire login lifecycle.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountProfileId(String);

impl AccountProfileId {
    pub fn new(value: impl Into<String>) -> Result<Self, AccountPoolError> {
        let value = value.into();
        if value.is_empty()
            || !value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return Err(AccountPoolError::InvalidProfileId(value));
        }
        Ok(Self(value))
    }

    /// Allocates a profile id before OAuth starts. The id is intentionally opaque and is not
    /// derived from account metadata that is unavailable until login completes.
    pub fn generate() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let sequence = NEXT_PROFILE_ID.fetch_add(1, Ordering::Relaxed);
        Self(format!("acct-{nanos:x}-{sequence:x}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AccountProfileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Persistent identity and storage information for an account in the pool.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountProfile {
    pub id: AccountProfileId,
    pub label: Option<String>,
    pub credential_home: PathBuf,
    /// Lower values are preferred by fill-first selection.
    pub priority: u32,
}

impl AccountProfile {
    pub fn new(
        id: AccountProfileId,
        credential_home: PathBuf,
        priority: u32,
        label: Option<String>,
    ) -> Self {
        Self {
            id,
            label,
            credential_home,
            priority,
        }
    }
}

/// One rate-limit window reported for an account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountRateLimitWindow {
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

/// Cached rate-limit information used for scheduling and UI. Real request failures remain
/// authoritative when the cached snapshot disagrees with the backend.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountRateLimits {
    pub primary: Option<AccountRateLimitWindow>,
    pub secondary: Option<AccountRateLimitWindow>,
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AccountAvailability {
    Available,
    Exhausted {
        resets_at: Option<DateTime<Utc>>,
    },
    AuthenticationUnavailable {
        reason: String,
    },
    Disabled,
}

impl AccountAvailability {
    fn is_eligible(&self, now: &DateTime<Utc>) -> bool {
        match self {
            Self::Available => true,
            Self::Exhausted {
                resets_at: Some(resets_at),
            } => resets_at <= now,
            Self::Exhausted { resets_at: None }
            | Self::AuthenticationUnavailable { .. }
            | Self::Disabled => false,
        }
    }

    fn refresh_for_time(&mut self, now: &DateTime<Utc>) {
        if matches!(
            self,
            Self::Exhausted {
                resets_at: Some(resets_at)
            } if resets_at <= now
        ) {
            *self = Self::Available;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountPoolSnapshot {
    pub profile: AccountProfile,
    pub availability: AccountAvailability,
    pub rate_limits: AccountRateLimits,
    pub is_active: bool,
}

/// Immutable execution binding handed to account-scoped clients.
///
/// The generation changes whenever the pool switches the active execution identity. A failure
/// reported by an older lease is stale and must never rotate the newly active account again.
#[derive(Clone)]
pub struct AccountLease {
    profile: AccountProfile,
    auth_manager: Arc<AuthManager>,
    generation: u64,
}

impl AccountLease {
    pub fn profile(&self) -> &AccountProfile {
        &self.profile
    }

    pub fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.auth_manager)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

struct ManagedAccount {
    profile: AccountProfile,
    auth_manager: Arc<AuthManager>,
    availability: AccountAvailability,
    rate_limits: AccountRateLimits,
}

struct AccountPoolState {
    accounts: HashMap<AccountProfileId, ManagedAccount>,
    active_profile: Option<AccountProfileId>,
    generation: u64,
}

impl Default for AccountPoolState {
    fn default() -> Self {
        Self {
            accounts: HashMap::new(),
            active_profile: None,
            generation: 1,
        }
    }
}

/// Process-wide execution account pool.
///
/// Threads, conversation state and app-server state remain outside this type and therefore stay
/// shared while only the execution identity changes.
pub struct AccountPool {
    state: Mutex<AccountPoolState>,
}

impl Default for AccountPool {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountPool {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(AccountPoolState::default()),
        }
    }

    pub fn register(
        &self,
        profile: AccountProfile,
        auth_manager: Arc<AuthManager>,
    ) -> Result<(), AccountPoolError> {
        let mut state = self.lock_state();
        if state.accounts.contains_key(&profile.id) {
            return Err(AccountPoolError::DuplicateProfile(profile.id));
        }

        state.accounts.insert(
            profile.id.clone(),
            ManagedAccount {
                profile,
                auth_manager,
                availability: AccountAvailability::Available,
                rate_limits: AccountRateLimits::default(),
            },
        );
        Ok(())
    }

    /// Returns a lease for the currently active eligible account. If there is no eligible active
    /// account, fill-first selection chooses the lowest-priority available profile.
    pub fn lease(&self) -> Result<AccountLease, AccountPoolError> {
        let mut state = self.lock_state();
        refresh_expired_exhaustion(&mut state);
        let now = Utc::now();

        if let Some(active_id) = state.active_profile.clone()
            && let Some(account) = state.accounts.get(&active_id)
            && account.availability.is_eligible(&now)
        {
            return Ok(make_lease(account, state.generation));
        }

        let selected_id = select_fill_first(&state, &now).ok_or(AccountPoolError::NoEligibleAccount)?;
        activate(&mut state, &selected_id);
        let account = state
            .accounts
            .get(&selected_id)
            .expect("selected account must exist");
        Ok(make_lease(account, state.generation))
    }

    /// Explicitly selects a profile. This is primarily for user-directed account selection and
    /// does not bypass disabled/auth-unavailable state.
    pub fn activate(&self, profile_id: &AccountProfileId) -> Result<AccountLease, AccountPoolError> {
        let mut state = self.lock_state();
        refresh_expired_exhaustion(&mut state);
        let now = Utc::now();
        let account = state
            .accounts
            .get(profile_id)
            .ok_or_else(|| AccountPoolError::UnknownProfile(profile_id.clone()))?;
        if !account.availability.is_eligible(&now) {
            return Err(AccountPoolError::ProfileUnavailable(profile_id.clone()));
        }

        activate(&mut state, profile_id);
        let account = state
            .accounts
            .get(profile_id)
            .expect("activated account must exist");
        Ok(make_lease(account, state.generation))
    }

    /// Marks a lease exhausted and rotates only when that exact lease is still the active
    /// generation. Late failures from stale workers cannot rotate a newer account.
    pub fn mark_exhausted(
        &self,
        lease: &AccountLease,
        resets_at: Option<DateTime<Utc>>,
    ) -> Result<Option<AccountLease>, AccountPoolError> {
        self.mark_unavailable_from_lease(
            lease,
            AccountAvailability::Exhausted { resets_at },
        )
    }

    /// Marks a permanently failed authentication profile unavailable after its own normal token
    /// refresh/recovery path has been exhausted.
    pub fn mark_authentication_unavailable(
        &self,
        lease: &AccountLease,
        reason: impl Into<String>,
    ) -> Result<Option<AccountLease>, AccountPoolError> {
        self.mark_unavailable_from_lease(
            lease,
            AccountAvailability::AuthenticationUnavailable {
                reason: reason.into(),
            },
        )
    }

    pub fn set_disabled(
        &self,
        profile_id: &AccountProfileId,
        disabled: bool,
    ) -> Result<(), AccountPoolError> {
        let mut state = self.lock_state();
        let account = state
            .accounts
            .get_mut(profile_id)
            .ok_or_else(|| AccountPoolError::UnknownProfile(profile_id.clone()))?;
        account.availability = if disabled {
            AccountAvailability::Disabled
        } else {
            AccountAvailability::Available
        };
        if disabled && state.active_profile.as_ref() == Some(profile_id) {
            state.active_profile = None;
            state.generation = state.generation.wrapping_add(1);
        }
        Ok(())
    }

    pub fn update_rate_limits(
        &self,
        profile_id: &AccountProfileId,
        rate_limits: AccountRateLimits,
    ) -> Result<(), AccountPoolError> {
        let mut state = self.lock_state();
        let account = state
            .accounts
            .get_mut(profile_id)
            .ok_or_else(|| AccountPoolError::UnknownProfile(profile_id.clone()))?;
        account.rate_limits = rate_limits;
        Ok(())
    }

    pub fn snapshots(&self) -> Vec<AccountPoolSnapshot> {
        let mut state = self.lock_state();
        refresh_expired_exhaustion(&mut state);
        let active_profile = state.active_profile.clone();
        let mut snapshots = state
            .accounts
            .values()
            .map(|account| AccountPoolSnapshot {
                profile: account.profile.clone(),
                availability: account.availability.clone(),
                rate_limits: account.rate_limits.clone(),
                is_active: active_profile.as_ref() == Some(&account.profile.id),
            })
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            left.profile
                .priority
                .cmp(&right.profile.priority)
                .then_with(|| left.profile.id.as_str().cmp(right.profile.id.as_str()))
        });
        snapshots
    }

    fn mark_unavailable_from_lease(
        &self,
        lease: &AccountLease,
        availability: AccountAvailability,
    ) -> Result<Option<AccountLease>, AccountPoolError> {
        let mut state = self.lock_state();
        refresh_expired_exhaustion(&mut state);

        if state.active_profile.as_ref() != Some(&lease.profile.id)
            || state.generation != lease.generation
        {
            return Ok(state
                .active_profile
                .as_ref()
                .and_then(|profile_id| state.accounts.get(profile_id))
                .map(|account| make_lease(account, state.generation)));
        }

        let account = state
            .accounts
            .get_mut(&lease.profile.id)
            .ok_or_else(|| AccountPoolError::UnknownProfile(lease.profile.id.clone()))?;
        account.availability = availability;
        state.active_profile = None;

        let now = Utc::now();
        let Some(selected_id) = select_fill_first(&state, &now) else {
            state.generation = state.generation.wrapping_add(1);
            return Ok(None);
        };
        activate(&mut state, &selected_id);
        let account = state
            .accounts
            .get(&selected_id)
            .expect("selected account must exist");
        Ok(Some(make_lease(account, state.generation)))
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, AccountPoolState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn make_lease(account: &ManagedAccount, generation: u64) -> AccountLease {
    AccountLease {
        profile: account.profile.clone(),
        auth_manager: Arc::clone(&account.auth_manager),
        generation,
    }
}

fn refresh_expired_exhaustion(state: &mut AccountPoolState) {
    let now = Utc::now();
    for account in state.accounts.values_mut() {
        account.availability.refresh_for_time(&now);
    }
}

fn select_fill_first(
    state: &AccountPoolState,
    now: &DateTime<Utc>,
) -> Option<AccountProfileId> {
    state
        .accounts
        .values()
        .filter(|account| account.availability.is_eligible(now))
        .min_by(|left, right| {
            left.profile
                .priority
                .cmp(&right.profile.priority)
                .then_with(|| left.profile.id.as_str().cmp(right.profile.id.as_str()))
        })
        .map(|account| account.profile.id.clone())
}

fn activate(state: &mut AccountPoolState, profile_id: &AccountProfileId) {
    if state.active_profile.as_ref() != Some(profile_id) {
        state.active_profile = Some(profile_id.clone());
        state.generation = state.generation.wrapping_add(1);
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AccountPoolError {
    #[error("invalid account profile id: {0}")]
    InvalidProfileId(String),
    #[error("account profile already exists: {0}")]
    DuplicateProfile(AccountProfileId),
    #[error("unknown account profile: {0}")]
    UnknownProfile(AccountProfileId),
    #[error("account profile is unavailable: {0}")]
    ProfileUnavailable(AccountProfileId),
    #[error("no eligible account is available")]
    NoEligibleAccount,
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::path::PathBuf;

    use codex_config::types::AuthCredentialsStoreMode;

    use super::*;
    use crate::AuthKeyringBackendKind;

    async fn test_auth_manager(home: &Path) -> Arc<AuthManager> {
        AuthManager::shared(
            home.to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
            None,
            None,
            AuthKeyringBackendKind::default(),
            crate::test_support::transport_default_auth_route_config(),
        )
        .await
    }

    fn profile(name: &str, priority: u32) -> AccountProfile {
        AccountProfile::new(
            AccountProfileId::new(name).expect("valid profile id"),
            PathBuf::from(format!("/tmp/{name}")),
            priority,
            Some(name.to_string()),
        )
    }

    #[test]
    fn generated_profile_ids_are_valid_and_unique() {
        let first = AccountProfileId::generate();
        let second = AccountProfileId::generate();
        assert_ne!(first, second);
        assert!(AccountProfileId::new(first.to_string()).is_ok());
    }

    #[test]
    fn rejects_unsafe_profile_ids() {
        assert!(AccountProfileId::new("").is_err());
        assert!(AccountProfileId::new("../other-home").is_err());
        assert!(AccountProfileId::new("with space").is_err());
    }

    #[tokio::test]
    async fn fill_first_sticks_until_active_account_is_exhausted() {
        let pool = AccountPool::new();
        let first = profile("first", 10);
        let second = profile("second", 20);
        pool.register(
            second.clone(),
            test_auth_manager(&second.credential_home).await,
        )
        .expect("register second");
        pool.register(
            first.clone(),
            test_auth_manager(&first.credential_home).await,
        )
        .expect("register first");

        let lease = pool.lease().expect("initial lease");
        assert_eq!(lease.profile().id, first.id);
        let same_lease = pool.lease().expect("sticky lease");
        assert_eq!(same_lease.profile().id, first.id);
        assert_eq!(same_lease.generation(), lease.generation());

        let next = pool
            .mark_exhausted(&lease, None)
            .expect("mark exhausted")
            .expect("fallback account");
        assert_eq!(next.profile().id, second.id);
        assert!(next.generation() > lease.generation());
    }

    #[tokio::test]
    async fn stale_failure_cannot_rotate_the_new_generation() {
        let pool = AccountPool::new();
        let first = profile("first", 10);
        let second = profile("second", 20);
        let third = profile("third", 30);
        for account in [&first, &second, &third] {
            pool.register(
                account.clone(),
                test_auth_manager(&account.credential_home).await,
            )
            .expect("register account");
        }

        let first_lease = pool.lease().expect("first lease");
        let second_lease = pool
            .mark_exhausted(&first_lease, None)
            .expect("first failover")
            .expect("second lease");
        assert_eq!(second_lease.profile().id, second.id);

        let still_second = pool
            .mark_exhausted(&first_lease, None)
            .expect("stale failure is ignored")
            .expect("active lease remains");
        assert_eq!(still_second.profile().id, second.id);
        assert_eq!(still_second.generation(), second_lease.generation());
    }

    #[tokio::test]
    async fn manually_disabled_active_account_is_reselected() {
        let pool = AccountPool::new();
        let first = profile("first", 10);
        let second = profile("second", 20);
        for account in [&first, &second] {
            pool.register(
                account.clone(),
                test_auth_manager(&account.credential_home).await,
            )
            .expect("register account");
        }

        let lease = pool.lease().expect("first lease");
        assert_eq!(lease.profile().id, first.id);
        pool.set_disabled(&first.id, true).expect("disable first");
        let next = pool.lease().expect("second lease");
        assert_eq!(next.profile().id, second.id);
    }
}
