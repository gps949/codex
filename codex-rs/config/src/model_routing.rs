//! Independent task-level model selection; relative roles are preferences, not quota prices.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRoutingMode {
    #[default]
    Off,
    Preview,
    Automatic,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRoutingSource {
    #[default]
    Local,
    DecisionService,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRoutingRole {
    Economy,
    Balanced,
    Capability,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ModelRoutingConfigToml {
    pub mode: ModelRoutingMode,
    pub source: ModelRoutingSource,
    /// Select models for new root user tasks. Steering and recovery retain the admitted choice.
    pub main_tasks: bool,
    /// Select models for fresh or partial-history children without explicit or role pins.
    pub subagents: bool,
    /// 0 favors longer use; 100 favors capability. This is not a measured usage multiplier.
    #[schemars(range(min = 0, max = 100))]
    pub preference: u8,
    /// Independent consent to send a bounded task description to Jev/Clef.
    pub send_task_description: bool,
    /// Optional local decision when the selected decision service is unavailable.
    pub local_fallback: bool,
    /// Empty uses the supported catalog profiles; otherwise models must match exactly.
    pub allowed_models: Vec<String>,
    /// Highest automatically selected effort; Ultra is never enabled automatically.
    pub max_effort: String,
    /// Optional user-defined relative roles for exact model names.
    pub model_roles: BTreeMap<String, ModelRoutingRole>,
}

impl Default for ModelRoutingConfigToml {
    fn default() -> Self {
        Self {
            mode: ModelRoutingMode::Off,
            source: ModelRoutingSource::Local,
            main_tasks: true,
            subagents: true,
            preference: 50,
            send_task_description: false,
            local_fallback: false,
            allowed_models: Vec::new(),
            max_effort: "high".into(),
            model_roles: BTreeMap::new(),
        }
    }
}

impl ModelRoutingConfigToml {
    pub fn validate(&self) -> std::io::Result<()> {
        let valid_name = |name: &str| {
            !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
        };
        if self.preference > 100
            || self.allowed_models.len() > 32
            || self.model_roles.len() > 32
            || self.allowed_models.iter().any(|name| !valid_name(name))
            || self.model_roles.keys().any(|name| !valid_name(name))
            || !matches!(
                self.max_effort.as_str(),
                "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            )
        {
            return Err(std::io::Error::other(
                "Invalid model routing preference, model list or effort limit",
            ));
        }
        if self.mode != ModelRoutingMode::Off
            && self.source == ModelRoutingSource::DecisionService
            && !self.send_task_description
        {
            return Err(std::io::Error::other(
                "Decision-service model routing requires separate consent to send the task description",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "model_routing_tests.rs"]
mod tests;
