//! Passive quota administration with no execution bridge, keepalive or generating warmup.

use crate::AccountPool;
use crate::AccountPoolRuntimeError;
use crate::AccountProfileState;
use crate::AccountProfileStore;
use crate::AccountRuntimeState;
use crate::AccountRuntimeStateStore;
use crate::AuthConfig;
use crate::AuthManager;
use std::sync::Arc;

/// Loads current profile credentials and cached scheduler state for a management operation.
/// This pool is request-owned and never registers a coordinator or starts background tasks.
/// Every authoritative probe rechecks manifest and stored credentials before committing.
pub async fn load_account_pool_for_management(
    auth_config: &AuthConfig,
) -> Result<Arc<AccountPool>, AccountPoolRuntimeError> {
    let store = AccountProfileStore::new(auth_config.codex_home.clone());
    let records = store.load_profile_records()?;
    let pool = Arc::new(AccountPool::new());
    for record in records
        .iter()
        .filter(|record| record.state == AccountProfileState::Ready)
    {
        let mut config = auth_config.clone();
        config.codex_home = record.profile.credential_home.clone();
        let manager = AuthManager::shared_managed_profile_from_auth_config(config.clone()).await;
        // Loading cached credentials never refreshes OAuth for unrelated or parked accounts.
        if manager.auth_cached().is_some_and(|auth| {
            auth.is_chatgpt_auth()
                && config.allows_auth(&auth)
                && manager.refresh_failure_for_auth(&auth).is_none()
        }) {
            pool.register(record.profile.clone(), manager)?;
        }
    }
    let state = AccountRuntimeStateStore::new(auth_config.codex_home.clone())
        .load()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    pool.merge_runtime_state(&state, &AccountRuntimeState::default(), Some(&records));
    Ok(pool)
}
