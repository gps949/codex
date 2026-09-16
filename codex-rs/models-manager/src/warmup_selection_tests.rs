use super::select_default_warmup_model;
use super::select_warmup_model;
use super::warmup_models_catalog;
use super::warmup_supported_effort;
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
fn select_default_uses_lowest_priority_list_model() {
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
                "affordable",
                /*priority*/ 8,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::Low, ReasoningEffort::Medium],
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

    let selected = select_default_warmup_model(&catalog).expect("model");
    assert_eq!(selected.slug, "frontier");
    assert_eq!(
        warmup_supported_effort(&selected, /*preferred*/ None),
        Some(ReasoningEffort::High)
    );
}

#[test]
fn select_default_skips_specialty_and_chatgpt_unsupported_list_models() {
    let catalog = ModelsResponse {
        models: vec![
            model(
                "gpt-5.2",
                /*priority*/ 1,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::Low],
                None,
            ),
            model(
                "cyber",
                /*priority*/ 2,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::Low],
                Some("cyber"),
            ),
            model(
                "capable",
                /*priority*/ 7,
                ModelVisibility::List,
                None,
                &[ReasoningEffort::Low],
                None,
            ),
        ],
    };
    let selected = select_default_warmup_model(&catalog).expect("model");
    assert_eq!(selected.slug, "capable");
}

#[test]
fn warmup_supported_effort_prefers_session_then_model_default() {
    let mut info = model(
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
    info.default_reasoning_level = Some(ReasoningEffort::Medium);

    assert_eq!(
        warmup_supported_effort(&info, Some(&ReasoningEffort::Low)),
        Some(ReasoningEffort::Low)
    );
    assert_eq!(
        warmup_supported_effort(&info, /*preferred*/ None),
        Some(ReasoningEffort::Medium)
    );
    assert_eq!(
        warmup_supported_effort(&info, Some(&ReasoningEffort::Ultra)),
        Some(ReasoningEffort::Medium)
    );
}

#[test]
fn select_default_returns_none_when_only_ineligible_models_exist() {
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
    assert_eq!(select_default_warmup_model(&catalog), None);
}

#[test]
fn bundled_catalog_selects_picker_default_chatgpt_capable_model() {
    let catalog = warmup_models_catalog(/*preferred*/ None);
    let selected = select_warmup_model(&catalog, /*preferred_slug*/ None).expect("bundled model");
    // Picker default is the lowest-priority list-visible capable slug (gpt-6-astra),
    // not the cheapest hidden-upgrade successor (terra) and not gpt-5.2.
    assert_eq!(selected.slug, "gpt-6-astra");
    assert_eq!(
        warmup_supported_effort(&selected, /*preferred*/ None),
        Some(ReasoningEffort::Low)
    );
}

#[test]
fn bundled_catalog_skips_reserve_and_chatgpt_unsupported_warmup_models() {
    let catalog = warmup_models_catalog(/*preferred*/ None);
    let selected = select_warmup_model(&catalog, /*preferred_slug*/ None).expect("bundled model");
    assert_ne!(selected.slug, "gpt-5.6-luna");
    assert_ne!(selected.slug, "gpt-5.2");
    assert_ne!(selected.slug, "gpt-5.5");
    assert!(
        !selected.slug.contains("luna"),
        "warmup must not pick a Luna/reserve model: {}",
        selected.slug
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
    assert_eq!(selected.slug, "gpt-6-astra");
}

#[test]
fn select_warmup_model_ignores_chatgpt_unsupported_session_slug() {
    let catalog = warmup_models_catalog(/*preferred*/ None);
    let selected = select_warmup_model(&catalog, Some("gpt-5.2")).expect("fallback");
    assert_ne!(selected.slug, "gpt-5.2");
    assert_ne!(selected.slug, "gpt-5.5");
    assert_eq!(selected.slug, "gpt-6-astra");
}
