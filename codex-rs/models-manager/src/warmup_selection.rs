//! Catalog-driven selection of the cheapest model/effort for standby window warmup.
//!
//! The backend `/models` catalog does not expose a numeric price. Instead we follow official
//! signals already present on model metadata:
//!
//! 1. Prefer the upgrade target of the least-featured (highest `priority`) non-list model that
//!    declares an upgrade. Today that is the cost-efficient mini tier succeeding into the current
//!    affordable list model — without hardcoding either slug.
//! 2. Else the highest-`priority` list-visible unspecialized model (least featured remaining).
//! 3. Else the first catalog model.
//!
//! Effort is always chosen from the selected model's advertised `supported_reasoning_levels`,
//! preferring the cheapest known wire values that the model actually supports.

use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;

/// Resolve the catalog to use for warmup: prefer a live/config catalog, else the bundled one.
pub fn warmup_models_catalog(preferred: Option<&ModelsResponse>) -> ModelsResponse {
    if let Some(catalog) = preferred.filter(|catalog| !catalog.models.is_empty()) {
        return catalog.clone();
    }
    crate::bundled_models_response().unwrap_or_else(|_| ModelsResponse::default())
}

/// Select the cheapest warmup-capable model from an official catalog.
pub fn select_cheapest_warmup_model(catalog: &ModelsResponse) -> Option<ModelInfo> {
    if catalog.models.is_empty() {
        return None;
    }

    if let Some(model) = upgrade_target_of_least_featured_non_list(catalog) {
        return Some(model);
    }
    if let Some(model) = highest_priority_list_visible(catalog) {
        return Some(model);
    }
    catalog.models.first().cloned()
}

/// Cheapest advertised reasoning effort for `model`, if any.
pub fn cheapest_supported_effort(model: &ModelInfo) -> Option<ReasoningEffort> {
    // Ordered cheapest → expensive. Skip Persistent/Custom: they are not cheap warmup targets.
    const CHEAPEST_FIRST: [ReasoningEffort; 8] = [
        ReasoningEffort::None,
        ReasoningEffort::Minimal,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::XHigh,
        ReasoningEffort::Max,
        ReasoningEffort::Ultra,
    ];
    for effort in CHEAPEST_FIRST {
        if model
            .supported_reasoning_levels
            .iter()
            .any(|preset| preset.effort == effort)
        {
            return Some(effort);
        }
    }
    model
        .supported_reasoning_levels
        .first()
        .map(|preset| preset.effort.clone())
        .or_else(|| model.default_reasoning_level.clone())
}

fn upgrade_target_of_least_featured_non_list(catalog: &ModelsResponse) -> Option<ModelInfo> {
    let mut best: Option<(i32, &str)> = None;
    for source in &catalog.models {
        if source.visibility == ModelVisibility::List {
            continue;
        }
        let Some(upgrade) = source.upgrade.as_ref() else {
            continue;
        };
        let replace = best.is_none_or(|(priority, _)| source.priority >= priority);
        if replace {
            best = Some((source.priority, upgrade.model.as_str()));
        }
    }
    let (_, target_slug) = best?;
    find_eligible_model(catalog, target_slug)
}

fn highest_priority_list_visible(catalog: &ModelsResponse) -> Option<ModelInfo> {
    catalog
        .models
        .iter()
        .filter(|model| is_warmup_eligible(model))
        .max_by_key(|model| model.priority)
        .cloned()
}

fn find_eligible_model(catalog: &ModelsResponse, slug: &str) -> Option<ModelInfo> {
    catalog
        .models
        .iter()
        .find(|model| model.slug == slug && is_warmup_eligible(model))
        .cloned()
}

fn is_warmup_eligible(model: &ModelInfo) -> bool {
    model.visibility == ModelVisibility::List
        && model.model_specialty.is_none()
        && !model.supported_reasoning_levels.is_empty()
}

#[cfg(test)]
#[path = "warmup_selection_tests.rs"]
mod tests;
