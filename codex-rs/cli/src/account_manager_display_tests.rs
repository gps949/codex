use super::*;
use codex_app_server::account_management::ManagedAccountView;
use codex_app_server::account_management::ManagedRateLimitWindow;
use codex_app_server::account_management::ManagedRateLimits;
use pretty_assertions::assert_eq;

fn inventory() -> AccountManagerInventory {
    let accounts = [
        ("Personal", 1.0, Some(2)),
        ("工作席位", 100.0, None),
        ("Standby", 9.0, Some(10)),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (label, used, credits))| ManagedAccountView {
        profile_id: format!("fixture-{index}"),
        label: label.into(),
        priority: index as u32,
        disabled: false,
        login_state: "signedIn".into(),
        availability: if used >= 100.0 {
            "coolingDown"
        } else {
            "ready"
        }
        .into(),
        cooldown_until: None,
        backend_resets_at: None,
        plan: Some("Pro".into()),
        email: None,
        rate_limits: ManagedRateLimits {
            primary: Some(ManagedRateLimitWindow {
                used_percent: used,
                resets_at: None,
                window_minutes: Some(300),
            }),
            secondary: None,
            observed_at: None,
            primary_observed_at: None,
            secondary_observed_at: None,
        },
        reset_credit_count: credits,
        refresh: None,
    })
    .collect();
    AccountManagerInventory {
        host_now: 1800000000,
        paused: false,
        active_profile_id: Some("fixture-0".into()),
        accounts,
        settings: serde_json::Value::Null,
        login_jobs: Vec::new(),
        api_accounts: Vec::new(),
        api_selection: codex_login::ApiAccountSelection::Subscription,
        api_fallback: codex_login::ApiAccountFallback::default(),
    }
}

#[test]
fn manager_inventory_aligns_numbers_and_unicode_labels() {
    let output = render(&inventory(), /*columns*/ 100);
    let rows: Vec<_> = output.lines().filter(|line| line.contains("Pro")).collect();
    assert_eq!(
        rows.iter()
            .map(|row| UnicodeWidthStr::width(*row))
            .collect::<Vec<_>>(),
        vec![83; 3]
    );
    insta::assert_snapshot!(output);
}

#[test]
fn manager_inventory_narrow_cards_wrap_without_losing_actions_or_unknown_quota() {
    let output = render(&inventory(), /*columns*/ 40);
    assert!(
        output
            .lines()
            .all(|line| UnicodeWidthStr::width(line) <= 40)
    );
    insta::assert_snapshot!(output);
}

#[test]
fn manager_manual_api_has_no_active_subscription_marker() {
    let mut inventory = inventory();
    inventory.api_selection = codex_login::ApiAccountSelection::Manual {
        profile_id: "synthetic-api".into(),
    };
    let output = render(&inventory, /*columns*/ 100);
    assert!(output.lines().all(|line| !line.starts_with('*')));
    insta::assert_snapshot!(output);
}
