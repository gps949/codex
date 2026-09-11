use super::cheapest_warmup_effort;
use super::resolve_warmup_model_info_from_catalog;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use pretty_assertions::assert_eq;

#[test]
fn resolve_warmup_model_info_uses_bundled_luna_metadata() {
    let info = resolve_warmup_model_info_from_catalog(/*catalog*/ None, "gpt-5.6-luna");
    assert_eq!(info.slug, "gpt-5.6-luna");
    assert!(info.use_responses_lite);
    assert_ne!(info.default_reasoning_level, Some(ReasoningEffort::Minimal));
    assert!(
        !info
            .supported_reasoning_levels
            .iter()
            .any(|preset| preset.effort == ReasoningEffort::Minimal),
        "luna must not advertise unsupported minimal effort: {:?}",
        info.supported_reasoning_levels
    );
    assert!(
        info.supported_reasoning_levels
            .iter()
            .any(|preset| preset.effort == ReasoningEffort::Low),
        "luna must advertise low effort: {:?}",
        info.supported_reasoning_levels
    );
}

#[test]
fn cheapest_warmup_effort_prefers_low_over_medium() {
    let info = resolve_warmup_model_info_from_catalog(/*catalog*/ None, "gpt-5.6-luna");
    assert_eq!(cheapest_warmup_effort(&info), Some(ReasoningEffort::Low));
}

#[test]
fn cheapest_warmup_effort_falls_back_to_first_supported() {
    let mut info = resolve_warmup_model_info_from_catalog(/*catalog*/ None, "gpt-5.6-luna");
    info.supported_reasoning_levels = vec![ReasoningEffortPreset {
        effort: ReasoningEffort::High,
        description: "high".to_string(),
    }];
    assert_eq!(cheapest_warmup_effort(&info), Some(ReasoningEffort::High));
}
