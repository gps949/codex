//! Warmup model/effort selection lives in `codex-models-manager::warmup_selection`.
//! Keep a thin smoke test here so core still exercises the catalog-driven path.
//! Also cover escalating failure backoff so NOOP/API streaks stop short-retry spam.

use super::*;
use pretty_assertions::assert_eq;
use std::time::Duration;

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

#[test]
fn backoff_for_streak_escalates_hard_and_noop() {
    assert_eq!(
        backoff_for_streak(FailureKind::Hard, 1),
        Duration::from_secs(5 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Hard, 2),
        Duration::from_secs(15 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Hard, 3),
        Duration::from_secs(45 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Hard, 4),
        Duration::from_secs(3 * 60 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Hard, 5),
        MAX_FAILURE_BACKOFF
    );

    assert_eq!(
        backoff_for_streak(FailureKind::Noop, 1),
        Duration::from_secs(30 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Noop, 2),
        Duration::from_secs(2 * 60 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Noop, 3),
        MAX_FAILURE_BACKOFF
    );
}
