//! Explicit, independent settings for optional semantic tool discovery.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAdvisorModeToml {
    #[default]
    Off,
    /// Make bounded requests while retaining the original BM25 results.
    Shadow,
    /// Use validated semantic rankings for tool-search results.
    Rank,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAdvisorProviderToml {
    #[default]
    Typesafe,
    Cloudflare,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAdvisorCredentialSourceToml {
    #[default]
    Environment,
    Stored,
}

/// This service never uses subscription-pool credentials or authorizes tool execution.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct DecisionAdvisorConfigToml {
    #[serde(default)]
    pub mode: DecisionAdvisorModeToml,
    #[serde(default)]
    pub provider: DecisionAdvisorProviderToml,
    /// Independent credentials; a missing stored key never falls back to environment auth.
    #[serde(default)]
    pub credential_source: DecisionAdvisorCredentialSourceToml,
    /// Independently opt into bounded skill suggestions at root user-turn boundaries.
    /// Suggestions never load skills or authorize operations. Defaults to false.
    #[serde(default)]
    pub suggest_skills: bool,
    /// Exact HTTP endpoint. Cloud services require HTTPS and reject URL credentials.
    /// TypeSafe defaults to https://api.typesafe.ai/v1/systemone.
    pub endpoint: Option<String>,
    /// TypeSafe model name, or clef/clef-flash for Workers AI.
    pub model: Option<String>,
    /// Separate environment variable containing the API credential; never a literal key.
    /// An empty name is accepted only for explicitly allowed loopback HTTP services.
    pub api_key_env: Option<String>,
    /// Allows an explicitly configured HTTP endpoint on localhost or a loopback IP.
    #[serde(default)]
    pub allow_local_http: bool,
    /// Total call budget including queueing and response consumption. Defaults to 650 ms.
    #[schemars(range(min = 100, max = 1500))]
    pub timeout_ms: Option<u64>,
    /// Answers below this confidence retain the original search. Defaults to 0.35.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub min_confidence: Option<f64>,
}

#[cfg(test)]
#[path = "decision_advisor_tests.rs"]
mod tests;
