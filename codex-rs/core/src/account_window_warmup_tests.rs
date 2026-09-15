//! Warmup model/effort selection lives in `codex-models-manager::warmup_selection`.
//! Keep a thin smoke test here so core still exercises the catalog-driven path.
//! Also cover escalating failure backoff and success-path helpers that prevent false NOOP retries.

use super::*;
use pretty_assertions::assert_eq;
use std::time::Duration;

fn rate_limit_window(used_percent: f64) -> RateLimitWindow {
    RateLimitWindow {
        used_percent,
        window_minutes: Some(300),
        resets_at: None,
    }
}

fn rate_limit_snapshot(limit_id: Option<&str>, used_percent: f64) -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_id: limit_id.map(str::to_string),
        limit_name: None,
        normal_model_slug: None,
        primary: Some(rate_limit_window(used_percent)),
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    }
}

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

#[test]
fn prefer_rate_limit_snapshot_prefers_codex_and_higher_usage() {
    let other = rate_limit_snapshot(Some("codex_other"), 12.0);
    let codex_idle = rate_limit_snapshot(Some("codex"), 0.0);
    let preferred = prefer_rate_limit_snapshot(Some(other), codex_idle.clone());
    assert_eq!(preferred.limit_id.as_deref(), Some("codex"));

    let codex_started = rate_limit_snapshot(Some("codex"), 3.0);
    let preferred = prefer_rate_limit_snapshot(Some(codex_idle), codex_started);
    assert_eq!(
        preferred.primary.as_ref().map(|window| window.used_percent),
        Some(3.0)
    );
}

#[test]
fn merge_account_rate_limits_monotonic_keeps_started_primary() {
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 2.5,
            resets_at: Some(Utc::now() + chrono::Duration::hours(4)),
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(Utc::now() - chrono::Duration::seconds(30)),
    };
    let incoming = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 0.0,
            resets_at: None,
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(Utc::now()),
    };
    let merged = merge_account_rate_limits_monotonic(Some(&existing), incoming);
    assert_eq!(
        merged.primary.as_ref().map(|window| window.used_percent),
        Some(2.5)
    );
}
