use super::cheapest_supported_effort;
use super::select_cheapest_warmup_model;
use super::select_warmup_model;
use super::warmup_models_catalog;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelInfoUpgrade;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use pretty_assertions::assert_eq;

fn model(
    slug: &str,
    priority: i32,
    visibility: ModelVisibility,
    upgrade_to: Option<&str>,
    efforts: &[ReasoningEffort],
    specialty: Option<&str>,
) -> ModelInfo {
    let mut info = crate::model_info::model_info_from_slug(slug);
    info.priority = priority;
    info.visibility = visibility;
    info.model_specialty = specialty.map(str::to_string);
    info.upgrade = upgrade_to.map(|model| ModelInfoUpgrade {
        model: model.to_string(),
        migration_markdown: String::new(),
        retirement_at: None,
    });
    info.supported_reasoning_levels = efforts
        .iter()
        .map(|effort| ReasoningEffortPreset {
            effort: effort.clone(),
            description: effort.as_str().to_string(),
        })
        .collect();
    info.default_reasoning_level = efforts.first().cloned();
    info
}

#[test]
fn select_cheapest_follows_upgrade_of_least_featured_hidden_model() {
    let catalog = ModelsResponse {
        models: vec![
            model(
                "frontier",
                /*priority*/ 1,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::Low, ReasoningEffort::High],
                None,
            ),
            model(
                "affordable",
                /*priority*/ 8,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::Low, ReasoningEffort::Medium],
                None,
            ),
            model(
                "old-balanced",
                /*priority*/ 16,
                ModelVisibility::Hide,
                Some("frontier"),
                &[ReasoningEffort::Medium],
                None,
            ),
            model(
                "old-mini",
                /*priority*/ 23,
                ModelVisibility::Hide,
                Some("affordable"),
                &[ReasoningEffort::Low],
                None,
            ),
        ],
    };

    let selected = select_cheapest_warmup_model(&catalog).expect("model");
    assert_eq!(selected.slug, "affordable");
    assert_eq!(
        cheapest_supported_effort(&selected),
        Some(ReasoningEffort::Low)
    );
}

#[test]
fn select_cheapest_falls_back_to_highest_priority_list_model() {
    let catalog = ModelsResponse {
        models: vec![
            model(
                "frontier",
                /*priority*/ 1,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::High],
                None,
            ),
            model(
                "legacy",
                /*priority*/ 40,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::Low],
                None,
            ),
            model(
                "cyber",
                /*priority*/ 99,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::Low],
                Some("cyber"),
            ),
        ],
    };

    let selected = select_cheapest_warmup_model(&catalog).expect("model");
    assert_eq!(selected.slug, "legacy");
}

#[test]
fn cheapest_supported_effort_prefers_low_over_minimal_and_medium() {
    let with_low = model(
        "m",
        /*priority*/ 1,
        ModelVisibility::List,
        None,
        &[
            ReasoningEffort::Medium,
            ReasoningEffort::Minimal,
            ReasoningEffort::Low,
        ],
        None,
    );
    assert_eq!(
        cheapest_supported_effort(&with_low),
        Some(ReasoningEffort::Low)
    );

    let without_low = model(
        "m",
        /*priority*/ 1,
        ModelVisibility::List,
        None,
        &[ReasoningEffort::Medium, ReasoningEffort::Minimal],
        None,
    );
    assert_eq!(
        cheapest_supported_effort(&without_low),
        Some(ReasoningEffort::Medium)
    );
}

#[test]
fn select_cheapest_returns_none_when_only_ineligible_models_exist() {
    let catalog = ModelsResponse {
        models: vec![model(
            "frontier-hidden",
            /*priority*/ 1,
            ModelVisibility::Hide,
            None,
            &[ReasoningEffort::Low],
            None,
        )],
    };
    assert_eq!(select_cheapest_warmup_model(&catalog), None);
}

#[test]
fn bundled_catalog_selects_upgrade_successor_of_hidden_mini_tier() {
    let catalog = warmup_models_catalog(/*preferred*/ None);
    let selected = select_cheapest_warmup_model(&catalog).expect("bundled model");
    // gpt-5.4-mini upgrades to gpt-5.6-luna (reserve, not warmup-capable). The next
    // hidden upgrade is gpt-5.4 → gpt-5.6-terra, which ChatGPT Codex accepts.
    assert_eq!(selected.slug, "gpt-5.6-terra");
    assert_eq!(
        cheapest_supported_effort(&selected),
        Some(ReasoningEffort::Low)
    );
}

#[test]
fn bundled_catalog_skips_reserve_and_chatgpt_unsupported_warmup_models() {
    let catalog = warmup_models_catalog(/*preferred*/ None);
    let selected = select_cheapest_warmup_model(&catalog).expect("bundled model");
    // Luna is a reserve slug. gpt-5.2 / gpt-5.5 400 on ChatGPT Codex even though
    // they are list-visible and supported_in_api.
    assert_ne!(selected.slug, "gpt-5.6-luna");
    assert_ne!(selected.slug, "gpt-5.2");
    assert_ne!(selected.slug, "gpt-5.5");
    assert!(
        !selected.slug.contains("luna"),
        "warmup must not pick a Luna/reserve model: {}",
        selected.slug
    );
    assert_eq!(
        cheapest_supported_effort(&selected),
        Some(ReasoningEffort::Low)
    );
}

#[test]
fn select_warmup_model_prefers_capable_session_slug() {
    let catalog = warmup_models_catalog(/*preferred*/ None);
    let selected =
        select_warmup_model(&catalog, Some("gpt-5.6-sol")).expect("session model should win");
    assert_eq!(selected.slug, "gpt-5.6-sol");
}

#[test]
fn select_warmup_model_ignores_reserve_session_slug() {
    let catalog = warmup_models_catalog(/*preferred*/ None);
    let selected = select_warmup_model(&catalog, Some("gpt-5.6-luna")).expect("fallback");
    assert_ne!(selected.slug, "gpt-5.6-luna");
}

#[test]
fn select_warmup_model_ignores_chatgpt_unsupported_session_slug() {
    let catalog = warmup_models_catalog(/*preferred*/ None);
    let selected = select_warmup_model(&catalog, Some("gpt-5.2")).expect("fallback");
    assert_ne!(selected.slug, "gpt-5.2");
    assert_ne!(selected.slug, "gpt-5.5");
}

#[test]
fn select_cheapest_skips_chatgpt_unsupported_highest_priority_list_model() {
    let catalog = ModelsResponse {
        models: vec![
            model(
                "capable",
                /*priority*/ 7,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::Low],
                None,
            ),
            model(
                "gpt-5.2",
                /*priority*/ 29,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::Low],
                None,
            ),
        ],
    };
    let selected = select_cheapest_warmup_model(&catalog).expect("model");
    assert_eq!(selected.slug, "capable");
}
