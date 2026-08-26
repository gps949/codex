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
