#!/usr/bin/env python3
from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    if new in text:
        print(f"already patched {path}")
        return
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected exactly one patch anchor, found {count}\n--- anchor ---\n{old}")
    file.write_text(text.replace(old, new, 1))
    print(f"patched {path}")


def write_exact(path: str, content: str) -> None:
    file = Path(path)
    if file.exists():
        current = file.read_text()
        if current == content:
            print(f"already generated {path}")
            return
        raise SystemExit(f"{path}: exists with unexpected content")
    file.write_text(content)
    print(f"generated {path}")


replace_once(
    "codex-rs/login/src/account_pool.rs",
    '''    /// Marks a lease exhausted and rotates only when that exact lease is still the active
    /// generation. Late failures from stale workers cannot rotate a newer account.
    pub fn mark_exhausted(''',
    '''    /// Explicitly re-enters fill-first scheduling, selecting the lowest-priority eligible
    /// profile even if another profile is currently active.
    pub fn activate_fill_first(&self) -> Result<AccountLease, AccountPoolError> {
        let mut state = self.lock_state();
        let refreshed = refresh_expired_exhaustion(&mut state);
        let now = Utc::now();
        let selected_id =
            select_fill_first(&state, &now).ok_or(AccountPoolError::NoEligibleAccount)?;
        let active_changed = set_active_profile(&mut state, &selected_id);
        let account = state
            .accounts
            .get(&selected_id)
            .expect("selected account must exist");
        let lease = make_lease(account, state.generation);
        drop(state);
        if refreshed || active_changed {
            self.notify_change();
        }
        Ok(lease)
    }

    /// Explicitly selects a profile while allowing a user to probe a profile that is only in
    /// quota cooldown. Disabled and authentication-unavailable profiles remain unavailable.
    pub fn force_activate(
        &self,
        profile_id: &AccountProfileId,
    ) -> Result<AccountLease, AccountPoolError> {
        let mut state = self.lock_state();
        let refreshed = refresh_expired_exhaustion(&mut state);
        let forced = {
            let account = state
                .accounts
                .get_mut(profile_id)
                .ok_or_else(|| AccountPoolError::UnknownProfile(profile_id.clone()))?;
            let forced = match &account.availability {
                AccountAvailability::Available => false,
                AccountAvailability::Exhausted { .. } => true,
                AccountAvailability::AuthenticationUnavailable { .. }
                | AccountAvailability::Disabled => {
                    return Err(AccountPoolError::ProfileUnavailable(profile_id.clone()));
                }
            };
            if forced {
                account.availability = AccountAvailability::Available;
            }
            forced
        };
        let active_changed = set_active_profile(&mut state, profile_id);
        let account = state
            .accounts
            .get(profile_id)
            .expect("activated account must exist");
        let lease = make_lease(account, state.generation);
        drop(state);
        if refreshed || forced || active_changed {
            self.notify_change();
        }
        Ok(lease)
    }

    /// Marks a lease exhausted and rotates only when that exact lease is still the active
    /// generation. Late failures from stale workers cannot rotate a newer account.
    pub fn mark_exhausted(''',
)

replace_once(
    "codex-rs/login/src/account_pool.rs",
    '''    #[tokio::test]
    async fn stale_failure_cannot_rotate_the_new_generation() {''',
    '''    #[tokio::test]
    async fn activate_fill_first_returns_to_preferred_available_profile() {
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
        pool.activate(&second.id).expect("activate second");
        let lease = pool.activate_fill_first().expect("activate fill-first");
        assert_eq!(lease.profile().id, first.id);
    }

    #[tokio::test]
    async fn force_activate_only_bypasses_quota_cooldown() {
        let pool = AccountPool::new();
        let first = profile("first", 10);
        pool.register(
            first.clone(),
            test_auth_manager(&first.credential_home).await,
        )
        .expect("register first");
        let lease = pool.lease().expect("initial lease");
        pool.mark_exhausted(&lease, None)
            .expect("mark exhausted");
        assert!(matches!(
            pool.activate(&first.id),
            Err(AccountPoolError::ProfileUnavailable(_))
        ));
        let forced = pool.force_activate(&first.id).expect("force activate");
        assert_eq!(forced.profile().id, first.id);
    }

    #[tokio::test]
    async fn stale_failure_cannot_rotate_the_new_generation() {''',
)

core_facade = '''use std::sync::Arc;

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
        identity_from_lease(lease.account_lease()?)
    }

    pub fn change_receiver(&self) -> watch::Receiver<u64> {
        self.inner.active_auth_change_receiver()
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
        let lease = if force {
            pool.force_activate(profile_id)?
        } else {
            pool.activate(profile_id)?
        };
        let identity = identity_from_lease(&lease).expect("pooled lease always has an identity");
        self.inner.compatibility_auth_manager().reload().await;
        Ok(identity)
    }

    pub async fn activate_fill_first(&self) -> Result<ExecutionAccountIdentity, AccountPoolError> {
        let pool = self
            .inner
            .account_pool()
            .ok_or(AccountPoolError::NoEligibleAccount)?;
        let lease = pool.activate_fill_first()?;
        let identity = identity_from_lease(&lease).expect("pooled lease always has an identity");
        self.inner.compatibility_auth_manager().reload().await;
        Ok(identity)
    }
}

fn identity_from_lease(lease: &AccountLease) -> Option<ExecutionAccountIdentity> {
    Some(ExecutionAccountIdentity {
        profile_id: lease.profile().id.clone(),
        generation: lease.generation(),
    })
}
'''
write_exact("codex-rs/core/src/execution_account_pool.rs", core_facade)

replace_once(
    "codex-rs/core/src/execution_auth.rs",
    '''    pub(crate) fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.auth_manager)
    }

    pub(crate) fn is_same_execution_identity(&self, other: &Self) -> bool {''',
    '''    pub(crate) fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.auth_manager)
    }

    pub(crate) fn account_lease(&self) -> Option<&AccountLease> {
        self.account.as_ref()
    }

    pub(crate) fn is_same_execution_identity(&self, other: &Self) -> bool {''',
)
replace_once(
    "codex-rs/core/src/lib.rs",
    "mod execution_auth;\n",
    "mod execution_account_pool;\nmod execution_auth;\n",
)
replace_once(
    "codex-rs/core/src/lib.rs",
    "pub use event_mapping::parse_turn_item;\n",
    "pub use event_mapping::parse_turn_item;\npub use execution_account_pool::ExecutionAccountIdentity;\npub use execution_account_pool::ExecutionAccountPoolHandle;\n",
)

protocol_file = '''use crate::JsonSchema;
use crate::TS;
use codex_protocol::account::PlanType;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolReadResponse {
    pub enabled: bool,
    pub active_profile_id: Option<String>,
    #[ts(type = "number | null")]
    pub active_generation: Option<u64>,
    pub accounts: Vec<AccountPoolAccount>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolUseParams {
    pub profile_id: Option<String>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolUseResponse {
    pub active_profile_id: String,
    #[ts(type = "number")]
    pub generation: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolAccount {
    pub profile_id: String,
    pub label: Option<String>,
    pub priority: u32,
    pub is_active: bool,
    pub availability: AccountPoolAvailability,
    pub plan_type: Option<PlanType>,
    pub email: Option<String>,
    pub rate_limits: AccountPoolRateLimits,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum AccountPoolAvailability {
    Available,
    Exhausted {
        #[ts(type = "number | null")]
        resets_at: Option<i64>,
    },
    AuthenticationUnavailable { reason: String },
    Disabled,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolRateLimits {
    pub primary: Option<AccountPoolRateLimitWindow>,
    pub secondary: Option<AccountPoolRateLimitWindow>,
    #[ts(type = "number | null")]
    pub observed_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolRateLimitWindow {
    pub used_percent: f64,
    #[ts(type = "number | null")]
    pub resets_at: Option<i64>,
}
'''
write_exact("codex-rs/app-server-protocol/src/protocol/v2/account_pool.rs", protocol_file)
replace_once(
    "codex-rs/app-server-protocol/src/protocol/v2/mod.rs",
    "mod account;\nmod apps;\n",
    "mod account;\nmod account_pool;\nmod apps;\n",
)
replace_once(
    "codex-rs/app-server-protocol/src/protocol/v2/mod.rs",
    "pub use account::*;\npub use apps::*;\n",
    "pub use account::*;\npub use account_pool::*;\npub use apps::*;\n",
)
replace_once(
    "codex-rs/app-server-protocol/src/protocol/common.rs",
    '''    GetAccountRateLimits => "account/rateLimits/read" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: None,
        response: v2::GetAccountRateLimitsResponse,
    },''',
    '''    #[experimental("accountPool/read")]
    AccountPoolRead => "accountPool/read" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: global("account-auth"),
        response: v2::AccountPoolReadResponse,
    },

    #[experimental("accountPool/use")]
    AccountPoolUse => "accountPool/use" {
        params: v2::AccountPoolUseParams,
        serialization: global("account-auth"),
        response: v2::AccountPoolUseResponse,
    },

    GetAccountRateLimits => "account/rateLimits/read" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        serialization: None,
        response: v2::GetAccountRateLimitsResponse,
    },''',
)

replace_once(
    "codex-rs/app-server/src/request_processors/account_processor.rs",
    "use crate::external_auth::ExternalAuthBridge;\n",
    "use crate::external_auth::ExternalAuthBridge;\nuse codex_core::ExecutionAccountPoolHandle;\n",
)
replace_once(
    "codex-rs/app-server/src/request_processors/account_processor.rs",
    '''pub(crate) struct AccountRequestProcessor {
    auth_manager: Arc<AuthManager>,
    thread_manager: Arc<ThreadManager>,''',
    '''pub(crate) struct AccountRequestProcessor {
    auth_manager: Arc<AuthManager>,
    execution_account_pool: ExecutionAccountPoolHandle,
    thread_manager: Arc<ThreadManager>,''',
)
replace_once(
    "codex-rs/app-server/src/request_processors/account_processor.rs",
    '''    ) -> Self {
        Self {
            auth_manager,
            thread_manager,''',
    '''    ) -> Self {
        let execution_account_pool = ExecutionAccountPoolHandle::shared(Arc::clone(&auth_manager));
        Self {
            auth_manager,
            execution_account_pool,
            thread_manager,''',
)
replace_once(
    "codex-rs/app-server/src/request_processors/account_processor.rs",
    '''    pub(crate) async fn get_account_rate_limits(
        &self,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {''',
    '''    pub(crate) async fn get_account_pool(
        &self,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_account_pool_response()
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn use_account_pool(
        &self,
        params: codex_app_server_protocol::AccountPoolUseParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.use_account_pool_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_account_rate_limits(
        &self,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {''',
)
replace_once(
    "codex-rs/app-server/src/request_processors/account_processor.rs",
    '''    async fn get_account_rate_limits_response(
        &self,
    ) -> Result<GetAccountRateLimitsResponse, JSONRPCErrorError> {''',
    '''    async fn get_account_pool_response(
        &self,
    ) -> Result<codex_app_server_protocol::AccountPoolReadResponse, JSONRPCErrorError> {
        let enabled = self
            .execution_account_pool
            .ensure_from_config(self.config.as_ref())
            .await
            .map_err(|err| internal_error(format!("failed to initialize account pool: {err}")))?;
        if !enabled {
            return Ok(codex_app_server_protocol::AccountPoolReadResponse {
                enabled: false,
                active_profile_id: None,
                active_generation: None,
                accounts: Vec::new(),
            });
        }

        let active = self.execution_account_pool.active_identity();
        let mut accounts = Vec::new();
        for snapshot in self.execution_account_pool.snapshots() {
            let (plan_type, email) = self.load_pool_profile_identity(&snapshot).await;
            accounts.push(codex_app_server_protocol::AccountPoolAccount {
                profile_id: snapshot.profile.id.to_string(),
                label: snapshot.profile.label.clone(),
                priority: snapshot.profile.priority,
                is_active: snapshot.is_active,
                availability: account_pool_availability(snapshot.availability),
                plan_type,
                email,
                rate_limits: account_pool_rate_limits(snapshot.rate_limits),
            });
        }
        Ok(codex_app_server_protocol::AccountPoolReadResponse {
            enabled: true,
            active_profile_id: active.as_ref().map(|identity| identity.profile_id.to_string()),
            active_generation: active.map(|identity| identity.generation),
            accounts,
        })
    }

    async fn use_account_pool_response(
        &self,
        params: codex_app_server_protocol::AccountPoolUseParams,
    ) -> Result<codex_app_server_protocol::AccountPoolUseResponse, JSONRPCErrorError> {
        let enabled = self
            .execution_account_pool
            .ensure_from_config(self.config.as_ref())
            .await
            .map_err(|err| internal_error(format!("failed to initialize account pool: {err}")))?;
        if !enabled {
            return Err(invalid_request("native account pool is not configured"));
        }

        let identity = if let Some(profile_id) = params.profile_id {
            let profile_id = codex_login::AccountProfileId::new(profile_id)
                .map_err(|err| invalid_request(err.to_string()))?;
            self.execution_account_pool
                .activate(&profile_id, params.force)
                .await
        } else {
            if params.force {
                return Err(invalid_request(
                    "force is only valid when selecting a specific account profile",
                ));
            }
            self.execution_account_pool.activate_fill_first().await
        }
        .map_err(|err| invalid_request(err.to_string()))?;

        Ok(codex_app_server_protocol::AccountPoolUseResponse {
            active_profile_id: identity.profile_id.to_string(),
            generation: identity.generation,
        })
    }

    async fn load_pool_profile_identity(
        &self,
        snapshot: &codex_login::AccountPoolSnapshot,
    ) -> (Option<codex_protocol::account::PlanType>, Option<String>) {
        let mut auth_config = self.config.auth_config();
        auth_config.codex_home = snapshot.profile.credential_home.clone();
        match AuthManager::shared_from_auth_config(
            auth_config,
            /*enable_codex_api_key_env*/ false,
        )
        .await
        {
            Ok(manager) => match manager.auth().await {
                Some(auth) => (auth.account_plan_type(), auth.get_account_email()),
                None => (None, None),
            },
            Err(err) => {
                tracing::warn!(
                    profile_id = %snapshot.profile.id,
                    %err,
                    "failed to read account-pool profile identity"
                );
                (None, None)
            }
        }
    }

    async fn get_account_rate_limits_response(
        &self,
    ) -> Result<GetAccountRateLimitsResponse, JSONRPCErrorError> {''',
)
replace_once(
    "codex-rs/app-server/src/request_processors/account_processor.rs",
    '''fn workspace_message_from_backend(
    message: BackendWorkspaceMessage,
) -> Result<WorkspaceMessage, JSONRPCErrorError> {''',
    '''fn account_pool_availability(
    availability: codex_login::AccountAvailability,
) -> codex_app_server_protocol::AccountPoolAvailability {
    match availability {
        codex_login::AccountAvailability::Available => {
            codex_app_server_protocol::AccountPoolAvailability::Available
        }
        codex_login::AccountAvailability::Exhausted { resets_at } => {
            codex_app_server_protocol::AccountPoolAvailability::Exhausted {
                resets_at: resets_at.map(|value| value.timestamp()),
            }
        }
        codex_login::AccountAvailability::AuthenticationUnavailable { reason } => {
            codex_app_server_protocol::AccountPoolAvailability::AuthenticationUnavailable { reason }
        }
        codex_login::AccountAvailability::Disabled => {
            codex_app_server_protocol::AccountPoolAvailability::Disabled
        }
    }
}

fn account_pool_rate_limits(
    limits: codex_login::AccountRateLimits,
) -> codex_app_server_protocol::AccountPoolRateLimits {
    codex_app_server_protocol::AccountPoolRateLimits {
        primary: limits.primary.map(account_pool_rate_limit_window),
        secondary: limits.secondary.map(account_pool_rate_limit_window),
        observed_at: limits.observed_at.map(|value| value.timestamp()),
    }
}

fn account_pool_rate_limit_window(
    window: codex_login::AccountRateLimitWindow,
) -> codex_app_server_protocol::AccountPoolRateLimitWindow {
    codex_app_server_protocol::AccountPoolRateLimitWindow {
        used_percent: window.used_percent,
        resets_at: window.resets_at.map(|value| value.timestamp()),
    }
}

fn workspace_message_from_backend(
    message: BackendWorkspaceMessage,
) -> Result<WorkspaceMessage, JSONRPCErrorError> {''',
)
replace_once(
    "codex-rs/app-server/src/message_processor.rs",
    '''            ClientRequest::GetAccountRateLimits { .. } => {
                self.account_processor.get_account_rate_limits().await
            }''',
    '''            ClientRequest::AccountPoolRead { .. } => {
                self.account_processor.get_account_pool().await
            }
            ClientRequest::AccountPoolUse { params, .. } => {
                self.account_processor.use_account_pool(params).await
            }
            ClientRequest::GetAccountRateLimits { .. } => {
                self.account_processor.get_account_rate_limits().await
            }''',
)
