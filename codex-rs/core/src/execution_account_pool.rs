use std::sync::Arc;

use codex_login::AccountLease;
use codex_login::AccountPoolError;
use codex_login::AccountPoolRuntimeError;
use codex_login::AccountPoolSnapshot;
use codex_login::AccountProfileId;
use codex_login::AuthManager;
use tokio::sync::watch;

use crate::config::Config;
use crate::execution_auth::ExecutionAuth;

/// Public app-facing facade over the process-local execution-account coordinator.
///
/// The internal coordinator remains private to codex-core; app-server and future TUI surfaces use
/// this handle so they always observe and mutate the exact same scheduler used by model requests.
#[derive(Clone)]
pub struct ExecutionAccountPoolHandle {
    inner: Arc<ExecutionAuth>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionAccountIdentity {
    pub profile_id: AccountProfileId,
    pub generation: u64,
}

impl ExecutionAccountPoolHandle {
    pub fn shared(auth_manager: Arc<AuthManager>) -> Self {
        Self {
            inner: ExecutionAuth::shared(auth_manager),
        }
    }

    pub async fn ensure_from_config(
        &self,
        config: &Config,
    ) -> Result<bool, AccountPoolRuntimeError> {
        self.inner.ensure_runtime_from_config(config).await
    }

    pub fn snapshots(&self) -> Vec<AccountPoolSnapshot> {
        self.inner
            .account_pool()
            .map(|pool| pool.snapshots())
            .unwrap_or_default()
    }

    pub fn active_identity(&self) -> Option<ExecutionAccountIdentity> {
        let lease = self.inner.active_lease()?;
        Some(identity_from_lease(lease.account_lease()?))
    }

    pub fn change_receiver(&self) -> watch::Receiver<u64> {
        self.inner.active_auth_change_receiver()
    }

    pub fn auth_managers(&self) -> Vec<(AccountProfileId, Arc<AuthManager>)> {
        self.inner
            .account_pool()
            .map(|pool| pool.auth_managers())
            .unwrap_or_default()
    }

    pub fn account_pool(&self) -> Option<Arc<codex_login::AccountPool>> {
        self.inner.account_pool()
    }

    pub async fn activate(
        &self,
        profile_id: &AccountProfileId,
        force: bool,
    ) -> Result<ExecutionAccountIdentity, AccountPoolError> {
        let pool = self
            .inner
            .account_pool()
            .ok_or(AccountPoolError::NoEligibleAccount)?;
        // Reload this profile's credentials from disk before scheduling so CLI re-login is picked
        // up without requiring a process restart.
        codex_login::prepare_profile_auth_for_activation(pool.as_ref(), profile_id).await?;
        let lease = if force {
            pool.force_activate(profile_id)?
        } else {
            pool.activate(profile_id)?
        };
        let expected = identity_from_lease(&lease);
        self.inner.compatibility_auth_manager().reload().await;
        // Outer auth resolve may mark the profile unavailable and rebound. Never report success
        // for a profile that is no longer active after that reload.
        match self.active_identity() {
            Some(actual) if actual.profile_id == expected.profile_id => Ok(actual),
            _ => Err(AccountPoolError::ProfileUnavailable(profile_id.clone())),
        }
    }

    pub async fn activate_fill_first(&self) -> Result<ExecutionAccountIdentity, AccountPoolError> {
        let pool = self
            .inner
            .account_pool()
            .ok_or(AccountPoolError::NoEligibleAccount)?;
        let _ = codex_login::recover_pool_auth_from_disk(pool.as_ref()).await;
        let lease = pool.activate_fill_first()?;
        let expected = identity_from_lease(&lease);
        self.inner.compatibility_auth_manager().reload().await;
        match self.active_identity() {
            Some(actual) if actual.profile_id == expected.profile_id => Ok(actual),
            _ => Err(AccountPoolError::NoEligibleAccount),
        }
    }

    pub fn window_warmup_settle_seconds() -> u64 {
        crate::account_window_warmup::INITIAL_WARMUP_SETTLE.as_secs()
    }

    pub fn window_warmup_task_running(&self) -> bool {
        self.inner.window_warmup_task_running()
    }

    pub fn request_window_warmup_pass_now(&self, config: &Config) {
        self.inner.request_window_warmup_pass_now(config.clone());
    }

    pub async fn force_activate_automatic(
        &self,
    ) -> Result<ExecutionAccountIdentity, AccountPoolError> {
        let pool = self
            .inner
            .account_pool()
            .ok_or(AccountPoolError::NoEligibleAccount)?;
        let _ = codex_login::recover_pool_auth_from_disk(pool.as_ref()).await;
        let lease = pool.force_activate_automatic()?;
        let expected = identity_from_lease(&lease);
        self.inner.compatibility_auth_manager().reload().await;
        match self.active_identity() {
            Some(actual) if actual.profile_id == expected.profile_id => Ok(actual),
            _ => Err(AccountPoolError::NoEligibleAccount),
        }
    }
}

fn identity_from_lease(lease: &AccountLease) -> ExecutionAccountIdentity {
    ExecutionAccountIdentity {
        profile_id: lease.profile().id.clone(),
        generation: lease.generation(),
    }
}
