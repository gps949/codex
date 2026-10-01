use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn pool() -> AccountPoolReadResponse {
    serde_json::from_value(json!({
        "enabled": true, "activeProfileId": "secret-work-id", "activeGeneration": 1,
        "accounts": [{"profileId":"secret-work-id", "label":"Work", "priority":10,
        "isActive":true, "availability":{"type":"available"}, "planType":null, "email":null,
        "rateLimits":{"primary":{"usedPercent":37.0,"resetsAt":1900000000},"secondary":null,"observedAt":null},
        "windowWarmup":null}]
    })).unwrap()
}

#[test]
fn mobile_account_compact_views_show_selectors_and_unknown_quota() {
    let pool = pool();
    insta::assert_snapshot!(list(&pool, 1).unwrap(), @"
    Accounts · 1/1

    Work · Current
    Select: /account use @secret-w
    Used: primary 37% · secondary unknown · cached

    Details: /account show <label|@selector>
    Controls: /account help
    ");
    assert_eq!(pool_caption(&pool).as_deref(), Some("Work · 1/1 ready"));
    assert_eq!(resolve(&pool, "\"Work\"").unwrap(), &pool.accounts[0]);
}

#[test]
fn mobile_account_label_prefers_custom_label_over_email() {
    let mut pool = pool();
    pool.accounts[0].email = Some("work@example.com".into());
    pool.accounts[0].label = Some("Work".into());
    assert_eq!(label(&pool.accounts[0]), "Work");
    assert_eq!(pool_caption(&pool).as_deref(), Some("Work · 1/1 ready"));
    assert_eq!(
        resolve(&pool, "work@example.com").unwrap(),
        &pool.accounts[0]
    );
}

#[test]
fn mobile_account_same_email_profiles_have_stable_selectors() {
    let mut pool = pool();
    pool.accounts[0].email = Some("member@example.com".into());
    let mut personal = pool.accounts[0].clone();
    personal.profile_id = "personal-profile".into();
    personal.label = Some("Personal".into());
    personal.is_active = false;
    pool.accounts.push(personal);
    assert_eq!(resolve(&pool, "@personal").unwrap(), &pool.accounts[1]);
    assert_eq!(resolve(&pool, "Personal").unwrap(), &pool.accounts[1]);
    assert!(resolve(&pool, "member@example.com").is_err());
    let text = list(&pool, 1).unwrap();
    assert!(text.contains("Personal"));
    assert!(text.contains("/account use @personal"));
    let text = detail(&pool, "@personal").unwrap();
    assert!(text.contains("Email: member@example.com"));
    assert!(text.contains("Select: /account use @personal"));
    pool.accounts[0].profile_id = "personal-work-profile".into();
    assert!(
        resolve(&pool, "@personal")
            .unwrap_err()
            .contains("Ambiguous")
    );
}

#[test]
fn mobile_account_pages_are_bounded_and_names_cannot_inject_markup() {
    let mut pool = pool();
    let mut other = pool.accounts[0].clone();
    other.is_active = false;
    other.label = Some("[bad](https://example.com)\n**name**".repeat(100));
    pool.accounts.extend(std::iter::repeat_n(other, 8));
    let text = list(&pool, 1).unwrap();
    assert_eq!(text.matches("Used:").count(), 4);
    assert!(text.contains("Next: /account list 2"));
    assert_eq!(text.matches("Select: /account use @").count(), 4);
    assert!(!text.contains("[bad]"));
    assert!(text.len() < 1200);
    assert!(list(&pool, usize::MAX).is_err());
    assert!(resolve(&pool, "\"Work").is_err());
    pool.accounts[1].label = Some("Work".into());
    assert!(resolve(&pool, "Work").unwrap_err().contains("Ambiguous"));
}

#[test]
fn mobile_account_detail_reports_relative_reset_and_observation_time() {
    let text = detail(&pool(), "Work").unwrap();
    assert!(text.contains("Work · Current"));
    assert!(text.contains("Primary: 37% used"));
    assert!(text.contains("Reset: in "));
    assert!(text.contains("Secondary: unknown used"));
    assert!(text.contains("Reset: unknown"));
    assert!(text.contains("Checked: unknown"));
    assert!(text.contains("Cached values remain if refresh fails."));
}

#[test]
fn mobile_account_detail_marks_idle_primary_window_not_started() {
    let mut pool = pool();
    pool.accounts[0].rate_limits.primary = Some(AccountPoolRateLimitWindow {
        used_percent: 0.0,
        resets_at: Some(1_900_000_000),
    });
    let text = detail(&pool, "Work").unwrap();
    assert!(text.contains("Primary: 0% used"));
    assert!(text.contains("Reset: not started"));
}

#[test]
fn mobile_account_detail_hides_warmup_failure() {
    let mut pool = pool();
    pool.accounts[0].rate_limits.primary = Some(AccountPoolRateLimitWindow {
        used_percent: 0.0,
        resets_at: Some(1_900_000_000),
    });
    pool.accounts[0].window_warmup = Some(codex_app_server_protocol::AccountPoolWindowWarmup {
        outcome: codex_app_server_protocol::AccountPoolWindowWarmupOutcome::Failed,
        attempted_at: 1_900_000_000,
        retry_after: None,
    });
    let text = detail(&pool, "Work").unwrap();
    assert!(!text.contains("Warmup:"));
    assert!(!text.contains("warmup failed"));
    assert!(!text.contains("5h warmed"));
}

#[test]
fn mobile_account_displayed_names_are_valid_selectors() {
    let mut pool = pool();
    pool.accounts[0].label = Some("Work_Pro [personal]".repeat(10));
    assert_eq!(
        resolve(&pool, &label(&pool.accounts[0])).unwrap(),
        &pool.accounts[0]
    );
}

#[test]
fn mobile_account_settings_explain_effective_controls() {
    let mut config = codex_config::AccountPoolConfigToml {
        window_warmup: Some(false),
        max_reset_wait_minutes: Some(120),
        auto_reset_credits: Some(codex_config::AutoResetCredits::WhenPoolExhausted),
        ..Default::default()
    };
    config.preemptive_switch_percent = Some(0.0);
    insta::assert_snapshot!(settings(&config), @"
    Pool settings
    Strategy: fill-first — use preferred accounts first
    Early rotation: off
    Return to preferred: true
    Warmup: off; every 5 min, uses a tiny request
    Wait and resume: on; up to 120 min, cancellable
    Auto reset credits: only when all accounts exhaust and natural reset is more than 60 min away

    Controls
    /account strategy fill-first|earliest-reset
    /account warmup on|off
    /account resume on|off
    /account wait <minutes: 0..1440>
    /account reset-credits never|when-pool-exhausted

    Quota observations are not additive balances. Partial output or unresolved tools may require manual reconciliation.
    ");
}
