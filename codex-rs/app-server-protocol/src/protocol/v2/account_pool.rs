use crate::JsonSchema;
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

/// Pushed whenever the account pool's scheduling state changes (active account, availability,
/// or observed rate limits). Unlike `accountPool/read`, the per-account `planType`/`email`
/// identity fields are omitted (`null`) to keep the notification cheap; fetch them on demand.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolUpdatedNotification {
    pub active_profile_id: Option<String>,
    #[ts(type = "number | null")]
    pub active_generation: Option<u64>,
    pub accounts: Vec<AccountPoolAccount>,
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
    /// Latest identity-preserving 5h-window warmup observation for standby accounts.
    #[serde(default)]
    pub window_warmup: Option<AccountPoolWindowWarmup>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolWindowWarmup {
    pub outcome: AccountPoolWindowWarmupOutcome,
    #[ts(type = "number")]
    pub attempted_at: i64,
    #[ts(type = "number | null")]
    pub retry_after: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum AccountPoolWindowWarmupOutcome {
    Succeeded,
    Failed,
    SkippedNoAuth,
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
    AuthenticationUnavailable {
        reason: String,
    },
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

/// Client->server request for the `/warmup` transcript dump.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolWarmupDebugParams {
    /// Request one warmup pass immediately. The response still returns now;
    /// run `/warmup` again to read the new events.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub run_now: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolWarmupDebugResponse {
    pub enabled: bool,
    pub task_running: bool,
    pub pass_requested: bool,
    #[ts(type = "number")]
    pub interval_seconds: u64,
    #[ts(type = "number")]
    pub settle_seconds: u64,
    pub rotation_strategy: String,
    pub session_model: Option<String>,
    pub accounts: Vec<AccountPoolWarmupDebugAccount>,
    pub events: Vec<AccountPoolWarmupDebugEvent>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolWarmupDebugAccount {
    pub profile_id: String,
    pub email: Option<String>,
    pub label: Option<String>,
    #[ts(type = "number")]
    pub priority: u32,
    pub is_active: bool,
    pub is_candidate: bool,
    pub availability: String,
    pub primary_used_percent: Option<f64>,
    pub persisted_warmup_outcome: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountPoolWarmupDebugEvent {
    #[ts(type = "number")]
    pub at: i64,
    pub message: String,
}
