//! Reads only routing settings at new task boundaries, preserving higher-priority layers.

use super::Config;
use codex_config::ConfigLayerSource;
use codex_config::ModelRoutingConfigToml;
use std::io;

pub(super) fn resolve(
    value: Option<&ModelRoutingConfigToml>,
) -> io::Result<ModelRoutingConfigToml> {
    let value = value.cloned().unwrap_or_default();
    value.validate()?;
    Ok(value)
}

impl Config {
    pub async fn model_routing_snapshot(&self) -> io::Result<ModelRoutingConfigToml> {
        let mut layers = self.config_layer_stack.clone();
        let original: ModelRoutingConfigToml = layers
            .effective_config()
            .get("model_routing")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()
            .map_err(io::Error::other)?
            .unwrap_or_default();
        // Programmatically supplied test/session policy remains authoritative.
        if self.model_routing != original {
            self.model_routing.validate()?;
            return Ok(self.model_routing.clone());
        }
        for layer in self.config_layer_stack.layers_low_to_high() {
            let ConfigLayerSource::User { file, .. } = &layer.name else {
                continue;
            };
            let text = match tokio::fs::read_to_string(file).await {
                Ok(text) => text,
                Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(error),
            };
            if text.len() > 2 * 1024 * 1024 {
                return Err(io::Error::other("Model routing settings file is oversized"));
            }
            let current: toml::Value = toml::from_str(&text).map_err(io::Error::other)?;
            let mut updated = layer.config.clone();
            let table = updated
                .as_table_mut()
                .ok_or_else(|| io::Error::other("Invalid routing settings table"))?;
            table.remove("model_routing");
            if let Some(value) = current.get("model_routing") {
                table.insert("model_routing".into(), value.clone());
            }
            layers = layers.with_user_config(file, updated)?;
        }
        let value: ModelRoutingConfigToml = layers
            .effective_config()
            .get("model_routing")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()
            .map_err(io::Error::other)?
            .unwrap_or_default();
        value.validate()?;
        Ok(value)
    }
}

#[cfg(test)]
#[path = "model_routing_tests.rs"]
mod tests;
