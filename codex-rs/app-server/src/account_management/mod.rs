//! Account administration independent of whether the execution pool can currently run.

mod api_accounts;
mod decision_advisor;
pub use decision_advisor::DecisionAdvisorCredentialAction;
pub use decision_advisor::DecisionAdvisorView;
mod inventory;
mod login;
mod operation_context;
mod operations;
pub(crate) use operation_context::AccountOperationContext;
mod preferences;
mod primary;
mod profile_identity;
mod quota;
mod warmup;
pub use preferences::ManagerLanguage;
pub use preferences::ManagerPreferences;
pub use primary::PrimaryLoginView;
pub use primary::PrimaryRuntimeView;
mod web;

pub use web::AccountManagerWebOptions;
pub use web::serve;

use codex_core::config::Config;
use codex_login::AccountProfileRecord;
use codex_login::AccountProfileStore;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared administration operations used by terminal and browser clients.
pub struct AccountManager {
    config: Arc<Config>,
    refreshes: Arc<std::sync::Mutex<HashMap<String, RefreshStatus>>>,
    logins: Mutex<HashMap<String, login::LoginJob>>,
    login_shutdown: std::sync::atomic::AtomicBool,
}

impl AccountManager {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config: Arc::new(config),
            refreshes: Arc::new(std::sync::Mutex::new(HashMap::new())),
            logins: Mutex::new(HashMap::new()),
            login_shutdown: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn store(&self) -> AccountProfileStore {
        AccountProfileStore::new(self.config.codex_home.to_path_buf())
    }

    fn profile(&self, id: &str) -> anyhow::Result<AccountProfileRecord> {
        self.store()
            .load_profile_records()?
            .into_iter()
            .find(|record| record.profile.id.as_str() == id)
            .ok_or_else(|| anyhow::anyhow!("Account profile no longer exists"))
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountManagerInventory {
    pub host_now: i64,
    pub primary_login: Option<PrimaryLoginView>,
    pub decision_advisor: Option<DecisionAdvisorView>,
    pub paused: bool,
    pub active_profile_id: Option<String>,
    pub accounts: Vec<ManagedAccountView>,
    pub settings: serde_json::Value,
    pub login_jobs: Vec<LoginProgress>,
    pub api_accounts: Vec<ApiAccountView>,
    pub api_selection: codex_login::ApiAccountSelection,
    pub api_fallback: codex_login::ApiAccountFallback,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiAccountView {
    #[serde(flatten)]
    pub account: codex_login::ApiAccount,
    pub has_key: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedAccountView {
    pub profile_id: String,
    pub label: String,
    pub custom_label: Option<String>,
    pub priority: u32,
    pub disabled: bool,
    pub login_state: String,
    pub availability: String,
    pub cooldown_until: Option<i64>,
    pub backend_resets_at: Option<i64>,
    pub plan: Option<String>,
    pub email: Option<String>,
    pub rate_limits: ManagedRateLimits,
    pub reset_credit_count: Option<u64>,
    pub refresh: Option<RefreshStatus>,
    pub warmup: Option<ManagedWarmupView>,
}

/// Latest persisted attempt and current window evidence for administration screens.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWarmupView {
    pub status: String,
    pub attempted_at: i64,
    pub retry_after: Option<i64>,
    pub consecutive_failures: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedRateLimitWindow {
    pub used_percent: f64,
    pub resets_at: Option<i64>,
    pub window_minutes: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedRateLimits {
    pub primary: Option<ManagedRateLimitWindow>,
    pub secondary: Option<ManagedRateLimitWindow>,
    pub observed_at: Option<i64>,
    pub primary_observed_at: Option<i64>,
    pub secondary_observed_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshStatus {
    pub in_progress: bool,
    pub attempted_at: i64,
    pub succeeded: bool,
    pub message: String,
    pub reset_credit_count: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginProgress {
    pub operation_id: String,
    pub profile_id: Option<String>,
    pub verification_url: Option<String>,
    pub user_code: Option<String>,
    pub status: String,
    pub message: String,
}

/// Explicit operations; refresh, probe and redemption intentionally have separate names.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AccountManagerOperation {
    DecisionSave {
        config: codex_config::DecisionAdvisorConfigToml,
        credential: DecisionAdvisorCredentialAction,
        consent: bool,
        #[serde(default, rename = "expectedVersion")]
        expected_version: Option<String>,
    },
    DecisionProbe {
        config: codex_config::DecisionAdvisorConfigToml,
        credential: DecisionAdvisorCredentialAction,
        consent: bool,
        #[serde(default, rename = "expectedVersion")]
        expected_version: Option<String>,
    },
    PrimaryUse {
        #[serde(rename = "profileId")]
        profile_id: String,
    },
    PrimaryRoot,
    PrimaryLogout,
    ApiAdd {
        label: String,
        #[serde(rename = "baseUrl")]
        base_url: String,
        model: String,
        #[serde(rename = "apiKey")]
        api_key: String,
        #[serde(rename = "contextWindow")]
        context_window: Option<i64>,
        images: Option<bool>,
    },
    ApiUpdate {
        account: codex_login::ApiAccount,
    },
    ApiReplaceKey {
        #[serde(rename = "profileId")]
        profile_id: String,
        #[serde(rename = "apiKey")]
        api_key: String,
    },
    ApiUse {
        #[serde(rename = "profileId")]
        profile_id: String,
    },
    ApiRemove {
        #[serde(rename = "profileId")]
        profile_id: String,
    },
    ApiFallback {
        config: codex_login::ApiAccountFallback,
    },
    Refresh {
        #[serde(rename = "profileIds")]
        profile_ids: Option<Vec<String>>,
    },
    Use {
        #[serde(rename = "profileId")]
        profile_id: String,
    },
    Retry {
        #[serde(rename = "profileId")]
        profile_id: String,
    },
    Automatic,
    Update {
        #[serde(rename = "profileId")]
        profile_id: String,
        label: Option<String>,
        priority: Option<u32>,
        disabled: Option<bool>,
    },
    Remove {
        #[serde(rename = "profileId")]
        profile_id: String,
        #[serde(default, rename = "keepCredentials")]
        keep_credentials: bool,
    },
    Login {
        #[serde(rename = "profileId")]
        profile_id: Option<String>,
        label: Option<String>,
    },
    CancelLogin {
        #[serde(rename = "operationId")]
        operation_id: String,
    },
    Credits {
        #[serde(rename = "profileId")]
        profile_id: String,
    },
    Redeem {
        #[serde(rename = "profileId")]
        profile_id: String,
        #[serde(rename = "creditId")]
        credit_id: String,
        #[serde(rename = "idempotencyKey")]
        idempotency_key: String,
    },
    Settings {
        values: serde_json::Value,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountManagerResult {
    pub message: String,
    pub data: serde_json::Value,
}

#[cfg(test)]
#[path = "management_tests.rs"]
mod tests;
