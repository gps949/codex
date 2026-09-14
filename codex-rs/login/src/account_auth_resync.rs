//! Reload per-profile auth from disk and recover sticky `AuthenticationUnavailable` state.
//!
//! CLI re-login (`codex account login <id>`) writes fresh tokens to a profile's credential home
//! while a long-lived Codex process may still hold a stale in-memory `AuthManager` cache for that
//! profile. Without an explicit reload, activate can appear to succeed and then silently rebound
//! when outer auth resolution marks the stale profile unavailable.

use std::sync::Arc;

use crate::AccountPool;
use crate::AccountPoolError;
use crate::AccountProfileId;
use crate::AuthManager;
use crate::CodexAuth;

/// Reloads every registered profile's AuthManager from disk and clears
/// `AuthenticationUnavailable` when on-disk credentials look like a successful CLI re-login.
///
/// Returns the profile ids that transitioned back to [`AccountAvailability::Available`].
///
/// Do **not** call this from the outer `ExternalAuth::resolve` hot path: disk may change as part of
/// a permanent auth failure (tests rewrite `auth.json` on 401), and re-entering resolve through the
/// shared AuthManager's external-auth bridge can recurse until the stack overflows.
pub async fn recover_pool_auth_from_disk(pool: &AccountPool) -> Vec<AccountProfileId> {
    let mut recovered = Vec::new();
    for (profile_id, manager) in pool.auth_managers() {
        if refresh_profile_auth_from_disk(pool, &profile_id, &manager)
            .await
            .unwrap_or(false)
        {
            recovered.push(profile_id);
        }
    }
    recovered
}

/// Reloads one profile's AuthManager from disk and clears sticky auth-unavailable state only when
/// the reload looks like an out-of-process re-login (refresh token changed) and the reloaded
/// credentials are still ChatGPT auth without a sticky refresh failure.
///
/// Returns `true` when availability was restored to [`AccountAvailability::Available`].
pub async fn refresh_profile_auth_from_disk(
    pool: &AccountPool,
    profile_id: &AccountProfileId,
    manager: &AuthManager,
) -> Result<bool, AccountPoolError> {
    let previous_refresh_token = refresh_token_fingerprint(manager);
    let _ = manager.reload().await;
    let current_refresh_token = refresh_token_fingerprint(manager);

    // Permanent 401 handlers (and similar) may rewrite `auth.json` without rotating the refresh
    // token. Treat only a refresh-token change as evidence of CLI re-login / credential repair.
    let looks_like_relogin = match (
        previous_refresh_token.as_deref(),
        current_refresh_token.as_deref(),
    ) {
        (None, Some(_)) => true,
        (Some(previous), Some(current)) => previous != current,
        _ => false,
    };
    if !looks_like_relogin || !profile_has_usable_chatgpt_auth(manager) {
        return Ok(false);
    }
    pool.clear_authentication_unavailable(profile_id)
}

/// Reloads the target profile (when registered) before an explicit activate/use so CLI re-login is
/// visible to the live process.
pub async fn prepare_profile_auth_for_activation(
    pool: &AccountPool,
    profile_id: &AccountProfileId,
) -> Result<(), AccountPoolError> {
    let Some(manager) = pool
        .auth_managers()
        .into_iter()
        .find_map(|(id, manager)| (id == *profile_id).then_some(manager))
    else {
        return Err(AccountPoolError::UnknownProfile(profile_id.clone()));
    };
    let _ = refresh_profile_auth_from_disk(pool, profile_id, manager.as_ref()).await?;
    Ok(())
}

fn refresh_token_fingerprint(manager: &AuthManager) -> Option<String> {
    manager
        .auth_cached()?
        .get_token_data()
        .ok()
        .map(|token_data| token_data.refresh_token)
}

fn profile_has_usable_chatgpt_auth(manager: &AuthManager) -> bool {
    let Some(auth) = manager.auth_cached() else {
        return false;
    };
    if !matches!(
        auth,
        CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_)
    ) {
        return false;
    }
    manager.refresh_failure_for_auth(&auth).is_none()
}

/// Shared helper for keep-alive: reload from disk first (picking up out-of-process re-login), then
/// run the normal proactive refresh path so standby tokens do not silently age out.
pub async fn keepalive_reload_profile_auth(
    pool: &AccountPool,
    profile_id: &AccountProfileId,
    manager: Arc<AuthManager>,
) {
    if let Err(error) = refresh_profile_auth_from_disk(pool, profile_id, manager.as_ref()).await {
        tracing::debug!(%profile_id, %error, "account keep-alive auth resync failed");
        return;
    }
    // `auth()` performs the managed ChatGPT proactive refresh after the cache matches disk.
    // Reload alone is not enough: standbys would otherwise never refresh while idle.
    if manager.auth().await.is_none() {
        tracing::debug!(%profile_id, "account keep-alive found no usable auth after disk reload");
    }
}

#[cfg(test)]
#[path = "account_auth_resync_tests.rs"]
mod tests;
