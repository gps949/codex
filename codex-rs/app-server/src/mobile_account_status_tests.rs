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
fn mobile_account_compact_views_hide_ids_and_show_unknown_quota() {
    let pool = pool();
    insta::assert_snapshot!(list(&pool, 1).unwrap(), @"
    Accounts · 1/1

    Work · Current
    Used: primary 37% · secondary unknown · cached

    Details: /account show <name>
    Controls: /account help
    ");
    assert_eq!(pool_caption(&pool).as_deref(), Some("Work · 1/1 ready"));
    assert_eq!(resolve(&pool, "\"Work\"").unwrap(), &pool.accounts[0]);
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
    assert!(!text.contains("secret-work-id"));
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
fn mobile_account_detail_includes_warmup_status() {
    let mut pool = pool();
    pool.accounts[0].window_warmup = Some(codex_app_server_protocol::AccountPoolWindowWarmup {
        outcome: codex_app_server_protocol::AccountPoolWindowWarmupOutcome::Succeeded,
        attempted_at: 1_900_000_000,
        retry_after: None,
    });
    let text = detail(&pool, "Work").unwrap();
    assert!(text.contains("Warmup: 5h warmed"));
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
