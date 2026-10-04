use super::*;
use crate::auth::KnownPlan;
use crate::auth::PlanType;
use crate::protocol::RateLimitSnapshot;
use pretty_assertions::assert_eq;

#[test]
fn quota_guidance_uses_ids_and_workspace_scope_consistently() {
    for (id, name, kind, expected, guidance) in [
        ("fast-model", None, None, true, "Switch to another model"),
        ("codex", Some("gpt-fast"), None, false, "Upgrade to Pro"),
        (
            "fast-model",
            Some("gpt-reserve"),
            None,
            false,
            "Upgrade to Pro",
        ),
        (
            "fast-model",
            Some("gpt-fast"),
            Some(RateLimitReachedType::WorkspaceMemberCreditsDepleted),
            false,
            "Ask your workspace owner",
        ),
    ] {
        let error = UsageLimitReachedError {
            plan_type: Some(PlanType::Known(KnownPlan::Plus)),
            resets_at: None,
            limit_window_minutes: None,
            rate_limits: Some(Box::new(RateLimitSnapshot {
                limit_id: Some(id.into()),
                limit_name: name.map(str::to_owned),
                rate_limit_reached_type: kind,
                normal_model_slug: None,
                plan_type: None,
                primary: None,
                secondary: None,
                credits: None,
                individual_limit: None,
                spend_control_reached: None,
            })),
            promo_message: None,
            rate_limit_reached_type: kind,
        };
        assert_eq!(error.is_model_specific(), expected);
        assert!(error.to_string().contains(guidance), "{}", error);
    }
}
