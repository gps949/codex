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
        custom_label: Some(label.into()),
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
        warmup: None,
    })
    .collect();
    AccountManagerInventory {
        decision_advisor: None,
        reset_journals: vec![],
        primary_login: None,
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
    let output = render(&inventory(), /*columns*/ 100, Locale::English);
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
fn manager_interrupted_reset_guidance_is_visible_in_both_languages() {
    let mut inventory = inventory();
    inventory
        .reset_journals
        .push(codex_app_server::account_management::ResetJournalView {
            file_name: "synthetic-file".into(),
            digest: "synthetic-digest".into(),
            profile_id: Some("fixture-0".into()),
            attempted_at: None,
            legacy: true,
            archive_available: true,
            message: "Legacy record needs review".into(),
        });
    insta::assert_snapshot!(
        "manager_interrupted_reset_english",
        render(&inventory, /*columns*/ 80, Locale::English)
    );
    insta::assert_snapshot!(
        "manager_interrupted_reset_chinese",
        render(&inventory, /*columns*/ 80, Locale::SimplifiedChinese)
    );
}

#[test]
fn manager_inventory_narrow_cards_wrap_without_losing_actions_or_unknown_quota() {
    let output = render(&inventory(), /*columns*/ 40, Locale::English);
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
    let output = render(&inventory, /*columns*/ 100, Locale::English);
    assert!(output.lines().all(|line| !line.starts_with('*')));
    insta::assert_snapshot!(output);
}

#[test]
fn manager_chinese_wide_inventory_aligns_terminal_cells() {
    let output = render(
        &inventory(),
        /*columns*/ 100,
        Locale::SimplifiedChinese,
    );
    let rows: Vec<_> = output.lines().filter(|line| line.contains("Pro")).collect();
    assert_eq!(
        rows.iter()
            .map(|row| UnicodeWidthStr::width(*row))
            .collect::<Vec<_>>(),
        vec![83; 3]
    );
    assert!(output.contains("Personal"));
    assert!(output.contains("工作席位"));
    assert!(output.contains("Standby"));
    insta::assert_snapshot!(output);
}

#[test]
fn manager_chinese_narrow_manual_api_retains_account_data_and_has_no_subscription_marker() {
    let mut inventory = inventory();
    inventory.api_accounts = vec![codex_app_server::account_management::ApiAccountView {
        credential_revision: None,

        account: codex_login::ApiAccount {
            id: "api-{untouched}".into(),
            label: "Needs login".into(),
            base_url: "https://example.invalid/responses".into(),
            model: "available".into(),
            disabled: false,
            context_window: 32768,
            images: false,
        },
        has_key: true,
    }];
    inventory.api_selection = codex_login::ApiAccountSelection::Manual {
        profile_id: "api-{untouched}".into(),
    };
    let output = render(&inventory, /*columns*/ 40, Locale::SimplifiedChinese);
    assert!(output.lines().all(|line| !line.starts_with('*')));
    assert!(
        output
            .lines()
            .all(|line| UnicodeWidthStr::width(line) <= 40)
    );
    assert!(output.contains("Needs login"));
    assert!(output.contains("available"));
    assert!(output.contains("工作席位"));
    insta::assert_snapshot!(output);
}

#[test]
fn manager_language_switch_updates_menu_and_guidance() {
    let inventory = inventory();
    let english = Locale::English;
    let chinese = english.toggle();
    assert_eq!(
        render(&inventory, /*columns*/ 100, chinese.toggle()),
        render(&inventory, /*columns*/ 100, english)
    );
    let output = format!(
        "{}\n\n{}\n\n{}\n\n{}",
        render(&inventory, /*columns*/ 100, english),
        english.main_menu(),
        render(&inventory, /*columns*/ 100, chinese),
        chinese.main_menu()
    );
    insta::assert_snapshot!(output);
}

#[test]
fn manager_all_disabled_guidance_points_to_enable_not_quota_refresh() {
    let mut inventory = inventory();
    for account in &mut inventory.accounts {
        account.disabled = true;
        account.availability = "disabled".into();
    }
    insta::assert_snapshot!(format!(
        "English:\n{}\nChinese:\n{}",
        render(&inventory, /*columns*/ 100, Locale::English),
        render(&inventory, /*columns*/ 100, Locale::SimplifiedChinese),
    ));
}

#[test]
fn manager_email_names_expand_tables_and_remain_complete_in_cards_after_clear() {
    let mut inventory = inventory();
    inventory.accounts[0].custom_label = None;
    inventory.accounts[0].email = Some("alice+personal-subscription@example.com".into());
    inventory.accounts[0].label = codex_login::account_display::account_display_name(
        inventory.accounts[0].custom_label.as_deref(),
        inventory.accounts[0].email.as_deref(),
        &inventory.accounts[0].profile_id,
    )
    .to_string();
    let wide = render(&inventory, /*columns*/ 120, Locale::English);
    assert!(wide.contains("alice+personal-subscription@example.com"));
    let narrow = render(&inventory, /*columns*/ 60, Locale::SimplifiedChinese);
    assert!(narrow.contains("alice+personal-subscription@example.com"));
    assert!(
        narrow
            .lines()
            .all(|line| UnicodeWidthStr::width(line) <= 60)
    );
    insta::assert_snapshot!(format!("Wide:\n{wide}\nNarrow:\n{narrow}"));
}

#[test]
fn manager_identity_controls_cannot_move_rows_or_hide_numeric_selectors() {
    let mut inventory = inventory();
    inventory.accounts[0].custom_label = None;
    inventory.accounts[0].email = Some("alice\u{061c}@example.com\n".into());
    inventory.accounts[0].label = codex_login::account_display::account_display_name(
        inventory.accounts[0].custom_label.as_deref(),
        inventory.accounts[0].email.as_deref(),
        &inventory.accounts[0].profile_id,
    )
    .to_string();
    inventory.accounts[1].label = "Name\u{202e}\tWork".into();
    let output = render(&inventory, /*columns*/ 100, Locale::English);
    assert!(!output.contains(['\u{061c}', '\u{202e}', '\t']));
    assert!(output.contains("alice @example.com"));
    assert_eq!(
        output.lines().filter(|line| line.contains("Pro")).count(),
        3
    );
    insta::assert_snapshot!(output);
}
