//! Provider-scoped API accounts kept separate from subscription quota scheduling.

mod store;
pub use store::ApiAccountStore;

use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiAccount {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default = "default_context_window")]
    pub context_window: i64,
    #[serde(default)]
    pub images: bool,
}

fn default_context_window() -> i64 {
    32_768
}

/// Host-wide selection is explicit; existing subscription manifests are left compatible.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ApiAccountSelection {
    #[default]
    Subscription,
    Manual {
        #[serde(rename = "profileId")]
        profile_id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiAccountFallback {
    #[serde(default)]
    pub enabled: bool,
    pub profile_id: Option<String>,
    /// Only enter the configured paid target after this much subscription waiting.
    #[serde(default = "default_wait_minutes")]
    pub wait_minutes: u64,
}

fn default_wait_minutes() -> u64 {
    5
}

impl Default for ApiAccountFallback {
    fn default() -> Self {
        Self {
            enabled: false,
            profile_id: None,
            wait_minutes: default_wait_minutes(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApiAccountState {
    #[serde(default)]
    pub accounts: Vec<ApiAccount>,
    #[serde(default)]
    pub selection: ApiAccountSelection,
    #[serde(default)]
    pub fallback: ApiAccountFallback,
    #[serde(default)]
    pub revision: u64,
}

/// Non-secret management data captured together under the account transaction lock.
#[derive(Debug)]
pub struct ApiAccountInventory {
    pub state: ApiAccountState,
    /// Exact target-and-key fingerprints; accounts without a saved key have no entry.
    pub credential_revisions: BTreeMap<String, String>,
}

impl ApiAccount {
    pub fn validate(&self) -> std::io::Result<()> {
        let valid = !self.label.trim().is_empty()
            && self.label.chars().count() <= 80
            && !self.label.chars().any(char::is_control)
            && !self.model.trim().is_empty()
            && self.model.len() <= 256
            && !self.model.chars().any(char::is_control)
            && (8_192..=2_000_000).contains(&self.context_window);
        if !valid {
            return Err(std::io::Error::other(
                "Invalid API account label, model or context limit",
            ));
        }
        let url = url::Url::parse(&self.base_url)
            .map_err(|_| std::io::Error::other("Invalid provider endpoint"))?;
        let local = url
            .host_str()
            .is_some_and(|host| host == "127.0.0.1" || host == "localhost" || host == "[::1]");
        if !(url.scheme() == "https" || url.scheme() == "http" && local)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(std::io::Error::other(
                "Use an HTTPS endpoint without embedded credentials, query or fragment",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "api_account_tests.rs"]
mod tests;
