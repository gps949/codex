//! Catalog-driven selection of the cheapest model/effort for standby window warmup.
//!
//! The backend `/models` catalog does not expose a numeric price. Instead we follow official
//! signals already present on model metadata:
//!
//! 1. Prefer the caller's current session model when it can start the 5h Codex window. That is
//!    the same request shape interactive turns already use, including current ChatGPT Codex
//!    models that advertise Responses Lite / `code_mode_only`.
//! 2. Else the upgrade target of the least-featured (highest `priority`) non-list model that
//!    declares an upgrade, when that target is warmup-capable. Ineligible targets (Luna, retired
//!    ChatGPT slugs) are skipped so we do not fall through to an unusable classic model.
//! 3. Else the highest-`priority` list-visible warmup-capable model (least featured remaining).
//!
//! Effort is always chosen from the selected model's advertised `supported_reasoning_levels`,
//! preferring Low (then Medium, then Minimal, …). Minimal is after Medium because some catalogs
//! advertise Minimal but still reject it for Responses warmup.

use chrono::Utc;
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

/// Select a warmup-capable model, preferring the session slug when it can start the 5h window.
pub fn select_warmup_model(
    catalog: &ModelsResponse,
    preferred_slug: Option<&str>,
) -> Option<ModelInfo> {
    if let Some(slug) = preferred_slug.filter(|slug| !slug.is_empty())
        && let Some(model) = catalog
            .models
            .iter()
            .find(|model| model.slug == slug && is_warmup_capable(model))
    {
        return Some(model.clone());
    }
    select_cheapest_warmup_model(catalog)
}

/// Select the cheapest warmup-capable model from an official catalog.
pub fn select_cheapest_warmup_model(catalog: &ModelsResponse) -> Option<ModelInfo> {
    if catalog.models.is_empty() {
        return None;
    }

    if let Some(model) = upgrade_target_of_least_featured_non_list(catalog) {
        return Some(model);
    }
    // Never fall back to an ineligible/first catalog entry (that can be a frontier slug).
    highest_priority_list_visible(catalog)
}

/// Cheapest advertised reasoning effort for `model`, if any.
pub fn cheapest_supported_effort(model: &ModelInfo) -> Option<ReasoningEffort> {
    // Prefer Low for warmup reliability. Minimal is sometimes advertised but still rejected, so
    // keep it behind Medium to avoid reintroducing the endless retry loop that #22 closed.
    const WARMUP_EFFORT_PREFERENCE: [ReasoningEffort; 7] = [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::Minimal,
        ReasoningEffort::High,
        ReasoningEffort::XHigh,
        ReasoningEffort::Max,
        ReasoningEffort::Ultra,
    ];
    for effort in WARMUP_EFFORT_PREFERENCE {
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
    let mut sources: Vec<&ModelInfo> = catalog
        .models
        .iter()
        .filter(|source| source.visibility != ModelVisibility::List && source.upgrade.is_some())
        .collect();
    sources.sort_by_key(|source| std::cmp::Reverse(source.priority));
    sources.iter().find_map(|source| {
        source
            .upgrade
            .as_ref()
            .and_then(|upgrade| find_eligible_model(catalog, &upgrade.model))
    })
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
    model.visibility == ModelVisibility::List && is_warmup_capable(model)
}

/// True when a catalog entry can start the primary 5h Codex window on a ChatGPT account.
///
/// Hidden session models are allowed when they are otherwise capable. List-only filtering
/// happens in [`is_warmup_eligible`].
///
/// Current ChatGPT Codex models (for example `gpt-5.6-sol`) advertise Responses Lite and
/// `code_mode_only`. Interactive turns already use that shape and start the 5h window.
/// Requiring classic HTTP here forced warmup onto retired API-only slugs such as `gpt-5.2`,
/// which ChatGPT Codex rejects with 400.
fn is_warmup_capable(model: &ModelInfo) -> bool {
    model.model_specialty.is_none()
        && !model.supported_reasoning_levels.is_empty()
        && !is_review_or_reserve_slug(&model.slug)
        && !is_chatgpt_unsupported_warmup_slug(&model.slug)
        && !is_retired_warmup_model(model)
}

fn is_review_or_reserve_slug(slug: &str) -> bool {
    let slug = slug.to_ascii_lowercase();
    slug.contains("luna") || slug.contains("auto-review") || slug.contains("guardian")
}

/// Slugs ChatGPT Codex rejects with `not supported when using Codex with a ChatGPT account`.
///
/// `supported_in_api` is API-key picker visibility, not ChatGPT usability. Bundled `gpt-5.2`
/// has `supported_in_api: true` and still 400s on ChatGPT accounts.
fn is_chatgpt_unsupported_warmup_slug(slug: &str) -> bool {
    let slug = slug.to_ascii_lowercase();
    slug == "gpt-5.5" || slug == "gpt-5.6" || slug == "gpt-5.2" || slug.starts_with("gpt-5.2-")
}

fn is_retired_warmup_model(model: &ModelInfo) -> bool {
    model
        .upgrade
        .as_ref()
        .and_then(|upgrade| upgrade.retirement_at)
        .is_some_and(|retirement_at| retirement_at < Utc::now())
}

#[cfg(test)]
#[path = "warmup_selection_tests.rs"]
mod tests;
