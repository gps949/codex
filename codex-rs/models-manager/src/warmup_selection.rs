//! Catalog-driven selection of the session/default model for standby window warmup.
//!
//! Codex has no versionless GPT alias (the catalog does not ship a DeepSeek-style
//! `flashthink` name). The Responses API still requires a `model` field, so "do not
//! specify a special warmup model" means: use a slug that already exists in the
//! catalog we are sending against. Never synthesize a missing name and never post a
//! reserved or ChatGPT-rejected slug.
//!
//! Candidates, in order, all from one catalog:
//! 1. The live session slug, only when that exact catalog entry is warmup-capable.
//! 2. The lowest-`priority` list-visible capable model (the picker default).
//! 3. The next lowest-priority capable list model, used only if the API rejects #1/#2.
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
    select_warmup_models(catalog, preferred_slug)
        .into_iter()
        .next()
}

/// Catalog-backed warmup candidates from a single catalog.
///
/// Unknown or unusable preferred slugs are ignored rather than posted. The spare
/// entry is the next picker-default capable model so one same-pass retry can
/// recover from an API "model not supported" / "model not found" rejection.
pub fn select_warmup_models(
    catalog: &ModelsResponse,
    preferred_slug: Option<&str>,
) -> Vec<ModelInfo> {
    let mut selected = Vec::new();
    if let Some(slug) = preferred_slug.filter(|slug| !slug.is_empty())
        && let Some(model) = catalog
            .models
            .iter()
            .find(|model| model.slug == slug && is_warmup_capable(model))
    {
        selected.push(model.clone());
    }
    if let Some(default) = select_default_warmup_model(catalog)
        && selected
            .iter()
            .all(|existing| existing.slug != default.slug)
    {
        selected.push(default);
    }
    if selected.len() < 2
        && let Some(spare) = catalog_capable_list_models(catalog)
            .into_iter()
            .find(|model| selected.iter().all(|existing| existing.slug != model.slug))
    {
        selected.push(spare);
    }
    selected
}

/// True when a Responses/API error means the posted slug cannot be used.
pub fn is_unusable_warmup_model_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    if !error.contains("model") {
        return false;
    }
    error.contains("not supported when using codex with a chatgpt account")
        || error.contains("model not found")
        || error.contains("unknown model")
        || error.contains("does not exist")
        || error.contains("is not supported")
}

/// Select the catalog picker-default warmup-capable model.
fn select_default_warmup_model(catalog: &ModelsResponse) -> Option<ModelInfo> {
    catalog_capable_list_models(catalog).into_iter().next()
}

fn catalog_capable_list_models(catalog: &ModelsResponse) -> Vec<ModelInfo> {
    let mut models: Vec<ModelInfo> = catalog
        .models
        .iter()
        .filter(|model| is_warmup_eligible(model))
        .cloned()
        .collect();
    models.sort_by_key(|model| model.priority);
    models
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
