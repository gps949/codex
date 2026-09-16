//! Catalog-driven selection of the session/default model for standby window warmup.
//!
//! Codex has no versionless GPT alias (the catalog does not ship a DeepSeek-style
//! `flashthink` name). The Responses API still requires a `model` field, so "do not
//! specify a special warmup model" means: use the live session slug when it can start
//! the 5h ChatGPT window, otherwise the catalog picker default. Do not invent a cheap
//! or reserved slug — a `1+1?` turn does not need a price-optimized model.
//!
//! 1. Prefer the caller's current session model when it can start the 5h Codex window.
//!    That is the same request shape interactive turns already use, including current
//!    ChatGPT Codex models that advertise Responses Lite / `code_mode_only`.
//! 2. Else the lowest-`priority` list-visible warmup-capable model (the picker default).
//!
//! Effort follows the session reasoning setting when the selected model advertises it,
//! otherwise the model's own `default_reasoning_level`.

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
    select_default_warmup_model(catalog)
}

/// Select the catalog picker-default warmup-capable model.
fn select_default_warmup_model(catalog: &ModelsResponse) -> Option<ModelInfo> {
    catalog
        .models
        .iter()
        .filter(|model| is_warmup_eligible(model))
        .min_by_key(|model| model.priority)
        .cloned()
}

/// Reasoning effort for a warmup turn: session preference when advertised, else the
/// model's catalog default.
pub fn warmup_supported_effort(
    model: &ModelInfo,
    preferred: Option<&ReasoningEffort>,
) -> Option<ReasoningEffort> {
    if let Some(effort) = preferred
        && model
            .supported_reasoning_levels
            .iter()
            .any(|preset| &preset.effort == effort)
    {
        return Some(effort.clone());
    }
    if let Some(default) = model.default_reasoning_level.as_ref()
        && model
            .supported_reasoning_levels
            .iter()
            .any(|preset| &preset.effort == default)
    {
        return Some(default.clone());
    }
    model
        .supported_reasoning_levels
        .first()
        .map(|preset| preset.effort.clone())
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
