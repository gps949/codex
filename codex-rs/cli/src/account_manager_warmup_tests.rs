use super::*;
use codex_app_server::account_management::ManagedRateLimits;
use codex_app_server::account_management::ManagedWarmupView;

#[test]
fn manager_warmup_details_keep_zero_usage_completion_unconfirmed() {
    let account = ManagedAccountView {
        profile_id: "fixture-seat".into(),
        label: "Personal".into(),
        custom_label: None,
        priority: 1,
        disabled: false,
        login_state: "signedIn".into(),
        availability: "ready".into(),
        cooldown_until: None,
        backend_resets_at: None,
        plan: None,
        email: None,
        rate_limits: ManagedRateLimits {
            primary: None,
            secondary: None,
            observed_at: None,
            primary_observed_at: None,
            secondary_observed_at: None,
        },
        reset_credit_count: None,
        refresh: None,
        warmup: Some(ManagedWarmupView {
            status: "completedUnconfirmed".into(),
            attempted_at: 1800000000,
            retry_after: Some(1800018000),
            consecutive_failures: 0,
        }),
    };
    insta::assert_snapshot!(format!(
        "English:\n{}\nChinese:\n{}",
        render(&account, Locale::English),
        render(&account, Locale::SimplifiedChinese),
    ));
}
