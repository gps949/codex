//! Warmup model/effort selection lives in `codex-models-manager::warmup_selection`.
//! Keep a thin smoke test here so core still exercises the catalog-driven path.

#[test]
fn catalog_driven_warmup_selection_is_available_to_core() {
    let catalog = codex_models_manager::warmup_models_catalog(/*preferred*/ None);
    let model = codex_models_manager::select_cheapest_warmup_model(&catalog)
        .expect("bundled catalog should expose a cheapest warmup model");
    let effort = codex_models_manager::cheapest_supported_effort(&model)
        .expect("selected warmup model should advertise at least one effort");

    assert!(
        !model.slug.is_empty(),
        "warmup model slug must come from the official catalog"
    );
    assert_ne!(
        effort,
        codex_protocol::openai_models::ReasoningEffort::Minimal,
        "bundled cheapest model currently rejects unsupported minimal effort"
    );
}
