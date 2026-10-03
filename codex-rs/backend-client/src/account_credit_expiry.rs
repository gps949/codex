//! Bounded, owner-bound reset-credit metadata reads. This service never redeems credits.

use crate::Client;
use crate::RateLimitResetCreditsDetails;
use chrono::Utc;
use codex_login::AccountAvailability;
use codex_login::AccountPool;
use codex_login::AccountProfile;
use codex_login::AccountProfileId;
use codex_login::AccountProfileState;
use codex_login::AccountProfileStore;
use codex_login::AccountRuntimeStateStore;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[path = "account_credit_expiry_filter.rs"]
mod filter;
use filter::bounded_text;
use filter::filter_credits;

const MAX_CACHE_PROFILES: usize = 64;
const MAX_CREDITS: usize = 16;
const RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// A valid, available Codex voucher, including later expiry observations for notice retention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpiringResetCredit {
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub expires_at: i64,
}

/// In-memory display metadata for one exact seat. Owner identifiers must not be persisted.
/// Freshness is for reminders only; spending must independently reload and confirm the credit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditExpirySnapshot {
    pub profile_id: AccountProfileId,
    pub label: String,
    pub account_id: String,
    pub chatgpt_user_id: String,
    pub observed_at: i64,
    pub fresh: bool,
    pub credits: Vec<ExpiringResetCredit>,
}

#[derive(Clone, PartialEq, Eq)]
struct Owner {
    credential_home: PathBuf,
    account_id: String,
    user_id: String,
    owner_generation: u64,
    manager_address: usize,
}

struct Candidate {
    profile: AccountProfile,
    manager: Arc<AuthManager>,
    owner: Owner,
}

#[derive(Clone)]
struct CacheEntry {
    owner: Owner,
    credits: Vec<ExpiringResetCredit>,
    observed_at: i64,
    fresh_until: Instant,
    next_check: Instant,
}

#[derive(Default)]
struct ReaderState {
    active: bool,
    cursor: usize,
    entries: HashMap<AccountProfileId, CacheEntry>,
}

struct PassPermit(Arc<Mutex<ReaderState>>);

impl Drop for PassPermit {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active = false;
    }
}

/// Share one reader across sessions for the same home. Passes are singleflight;
/// at most four profiles are contacted per pass, with a 64-profile metadata cache.
pub struct AccountCreditExpiryReader {
    home: PathBuf,
    state: Arc<Mutex<ReaderState>>,
}

impl AccountCreditExpiryReader {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            state: Arc::new(Mutex::new(ReaderState::default())),
        }
    }

    /// Returns current owner-verified cached notices and refreshes a rotating due batch.
    /// A four-second pass deadline includes credential refresh, routing and credit GETs.
    /// Concurrent callers return cache immediately; cancellation never authorizes an action.
    pub async fn collect_due(
        &self,
        pool: &Arc<AccountPool>,
        chatgpt_base_url: &str,
        cancellation: &CancellationToken,
    ) -> Vec<CreditExpirySnapshot> {
        if cancellation.is_cancelled() {
            return Vec::new();
        }
        let candidates = candidates(pool);
        let jobs = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.active {
                drop(state);
                return self.cached(pool);
            }
            state.entries.retain(|id, entry| {
                candidates
                    .iter()
                    .any(|candidate| &candidate.profile.id == id && candidate.owner == entry.owner)
            });
            let mut jobs = Vec::new();
            let count = candidates.len();
            for offset in 0..count {
                let index = (state.cursor + offset) % count;
                let candidate = &candidates[index];
                if state
                    .entries
                    .get(&candidate.profile.id)
                    .is_some_and(|entry| entry.next_check > Instant::now())
                {
                    continue;
                }
                jobs.push(index);
                if jobs.len() == 4 {
                    break;
                }
            }
            let Some(last) = jobs.last().copied() else {
                drop(state);
                return self.cached(pool);
            };
            state.cursor = (last + 1) % count;
            state.active = true;
            let now = Instant::now();
            for index in &jobs {
                let candidate = &candidates[*index];
                let entry = state
                    .entries
                    .entry(candidate.profile.id.clone())
                    .or_insert_with(|| CacheEntry {
                        owner: candidate.owner.clone(),
                        credits: Vec::new(),
                        observed_at: 0,
                        fresh_until: now,
                        next_check: now,
                    });
                // Reserve before awaits. Cancellation and failed reads retain a bounded retry delay.
                entry.next_check = now + RETRY_INTERVAL;
            }
            while state.entries.len() > MAX_CACHE_PROFILES {
                let oldest = state
                    .entries
                    .iter()
                    .filter(|(id, _)| {
                        !jobs
                            .iter()
                            .any(|index| &candidates[*index].profile.id == *id)
                    })
                    .min_by_key(|(_, entry)| entry.observed_at)
                    .map(|(id, _)| id.clone());
                if let Some(id) = oldest {
                    state.entries.remove(&id);
                } else {
                    break;
                }
            }
            jobs
        };
        let permit = Arc::new(PassPermit(Arc::clone(&self.state)));
        let deadline = Instant::now() + Duration::from_secs(4);
        let profiles = AccountProfileStore::new(self.home.clone());
        let blocking_permit = Arc::clone(&permit);
        let records = tokio::task::spawn_blocking(move || {
            // If a metadata lock is busy past cancellation, retain singleflight until this read ends.
            let _permit = blocking_permit;
            profiles.load_profile_records()
        });
        let records = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Vec::new(),
            _ = tokio::time::sleep_until(deadline) => return self.cached(pool),
            result = records => match result {
                Ok(Ok(records)) => records,
                _ => return self.cached(pool),
            },
        };
        let mut pending = JoinSet::new();
        for (index, candidate) in candidates.into_iter().enumerate() {
            if !jobs.contains(&index)
                || !records.iter().any(|record| {
                    record.profile.id == candidate.profile.id
                        && !record.profile.disabled
                        && record.state == AccountProfileState::Ready
                        && record.profile.credential_home == candidate.profile.credential_home
                })
            {
                continue;
            }
            let pool = Arc::clone(pool);
            let store = AccountRuntimeStateStore::new(self.home.clone());
            let base_url = chatgpt_base_url.to_string();
            let cancellation = cancellation.clone();
            pending.spawn(async move {
                let observed = tokio::time::timeout(
                    Duration::from_secs(3),
                    read_profile(&pool, &store, &candidate, base_url, &cancellation),
                )
                .await
                .ok()
                .flatten();
                (candidate, observed)
            });
        }
        loop {
            let result = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Vec::new(),
                _ = tokio::time::sleep_until(deadline) => break,
                result = pending.join_next() => result,
            };
            let Some(result) = result else {
                break;
            };
            let Ok((candidate, observed)) = result else {
                continue;
            };
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(entry) = state.entries.get_mut(&candidate.profile.id) else {
                continue;
            };
            if entry.owner != candidate.owner {
                continue;
            }
            let now = Instant::now();
            entry.fresh_until = now;
            if let Some(credits) = observed {
                let observed_at = Utc::now().timestamp();
                entry.credits = filter_credits(credits, observed_at);
                entry.observed_at = observed_at;
                let interval = match entry.credits.first() {
                    Some(credit) if credit.expires_at - observed_at <= 6 * 3_600 => RETRY_INTERVAL,
                    Some(credit) if credit.expires_at - observed_at <= 24 * 3_600 => {
                        Duration::from_secs(15 * 60)
                    }
                    Some(_) => Duration::from_secs(60 * 60),
                    None => Duration::from_secs(60 * 60),
                };
                entry.next_check = now + interval;
                entry.fresh_until = entry.next_check;
            }
        }
        drop(pending);
        drop(permit);
        self.cached(pool)
    }

    fn cached(&self, pool: &AccountPool) -> Vec<CreditExpirySnapshot> {
        let now = Utc::now().timestamp();
        let store = AccountRuntimeStateStore::new(self.home.clone());
        let candidates = candidates(pool);
        let entries = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .clone();
        candidates
            .into_iter()
            .filter_map(|candidate| {
                let entry = entries.get(&candidate.profile.id)?;
                if entry.owner != candidate.owner || entry.credits.is_empty() {
                    return None;
                }
                let auth = candidate.manager.auth_cached()?;
                if auth.get_account_id().as_ref() != Some(&entry.owner.account_id)
                    || auth.get_chatgpt_user_id().as_ref() != Some(&entry.owner.user_id)
                    || candidate
                        .manager
                        .auth_change_state_receiver()
                        .borrow()
                        .owner_generation
                        != entry.owner.owner_generation
                    || !store
                        .validate_profile_auth(pool, &candidate.profile.id, &auth)
                        .unwrap_or(false)
                    || candidate
                        .manager
                        .auth_change_state_receiver()
                        .borrow()
                        .owner_generation
                        != entry.owner.owner_generation
                {
                    return None;
                }
                let credits: Vec<_> = entry
                    .credits
                    .iter()
                    .filter(|credit| credit.expires_at > now)
                    .cloned()
                    .collect();
                if credits.is_empty() {
                    return None;
                }
                Some(CreditExpirySnapshot {
                    profile_id: candidate.profile.id.clone(),
                    label: bounded_text(
                        candidate
                            .profile
                            .label
                            .as_deref()
                            .unwrap_or(candidate.profile.id.as_str()),
                        /*max_chars*/ 64,
                    ),
                    account_id: entry.owner.account_id.clone(),
                    chatgpt_user_id: entry.owner.user_id.clone(),
                    observed_at: entry.observed_at,
                    fresh: entry.observed_at <= now && entry.fresh_until > Instant::now(),
                    credits,
                })
            })
            .collect()
    }
}

fn candidates(pool: &AccountPool) -> Vec<Candidate> {
    let managers = pool.auth_managers();
    pool.snapshots()
        .into_iter()
        .filter_map(|snapshot| {
            if snapshot.profile.disabled
                || matches!(
                    snapshot.availability,
                    AccountAvailability::Disabled
                        | AccountAvailability::AuthenticationUnavailable { .. }
                )
            {
                return None;
            }
            let manager = &managers
                .iter()
                .find(|(id, _)| id == &snapshot.profile.id)?
                .1;
            let revision = manager.auth_change_state_receiver();
            let before = *revision.borrow();
            let auth = manager.auth_cached()?;
            let account_id = auth.get_account_id()?;
            let user_id = auth.get_chatgpt_user_id()?;
            if !auth.is_chatgpt_auth()
                || *revision.borrow() != before
                || [&account_id, &user_id]
                    .iter()
                    .any(|id| id.trim().is_empty() || id.len() > 512)
            {
                return None;
            }
            Some(Candidate {
                owner: Owner {
                    credential_home: snapshot.profile.credential_home.clone(),
                    account_id,
                    user_id,
                    owner_generation: before.owner_generation,
                    manager_address: Arc::as_ptr(manager) as usize,
                },
                profile: snapshot.profile,
                manager: Arc::clone(manager),
            })
        })
        .collect()
}

async fn read_profile(
    pool: &AccountPool,
    store: &AccountRuntimeStateStore,
    candidate: &Candidate,
    mut base_url: String,
    cancellation: &CancellationToken,
) -> Option<RateLimitResetCreditsDetails> {
    let (auth, mut factory) = candidate.manager.auth_with_http_client_factory().await?;
    let revision = *candidate.manager.auth_change_state_receiver().borrow();
    if let Some(clients) = candidate.manager.maintenance_clients(&auth).await.ok()? {
        factory = clients.http_client_factory;
        base_url = clients.chatgpt_base_url;
    }
    verify_profile(pool, store, candidate, &auth, revision, cancellation).await?;
    if cancellation.is_cancelled() {
        return None;
    }
    let credits = Client::from_auth(base_url, &auth, factory)
        .list_rate_limit_reset_credits()
        .await
        .ok()?;
    // Recheck stored tokens as well as the in-memory revision: another process can relogin first.
    verify_profile(pool, store, candidate, &auth, revision, cancellation).await?;
    Some(credits)
}

async fn verify_profile(
    pool: &AccountPool,
    store: &AccountRuntimeStateStore,
    candidate: &Candidate,
    auth: &CodexAuth,
    revision: codex_login::AuthChangeState,
    cancellation: &CancellationToken,
) -> Option<()> {
    loop {
        if cancellation.is_cancelled()
            || *candidate.manager.auth_change_state_receiver().borrow() != revision
            || revision.owner_generation != candidate.owner.owner_generation
            || auth.get_account_id().as_ref() != Some(&candidate.owner.account_id)
            || auth.get_chatgpt_user_id().as_ref() != Some(&candidate.owner.user_id)
            || !candidates(pool).iter().any(|current| {
                current.profile.id == candidate.profile.id && current.owner == candidate.owner
            })
        {
            return None;
        }
        if store
            .validate_profile_auth(pool, &candidate.profile.id, auth)
            .ok()?
        {
            return (*candidate.manager.auth_change_state_receiver().borrow() == revision)
                .then_some(());
        }
        tokio::select! { biased;
            _ = cancellation.cancelled() => return None,
            _ = tokio::time::sleep(Duration::from_millis(20)) => {},
        }
    }
}

#[cfg(test)]
#[path = "account_credit_expiry_tests.rs"]
mod tests;
