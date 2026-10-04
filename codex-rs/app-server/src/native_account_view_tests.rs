use super::*;
use pretty_assertions::assert_eq;

const NOW: i64 = 1_700_000_000;

fn fixture() -> FrozenAccountInventory {
    FrozenAccountInventory {
        accounts: vec![FrozenAccount {
            id: "slot-1".into(),
            label: "Alpha 中文".into(),
            current: true,
            state: "coolingDown".into(),
            login_state: "signedIn".into(),
            disabled: false,
            detail: AccountDetail::Subscription {
                plan: "Business".into(),
                email: Some("alpha@example.test".into()),
                primary: Some(ManagedRateLimitWindow {
                    used_percent: 9.5,
                    resets_at: None,
                    window_minutes: Some(300),
                }),
                secondary: Some(ManagedRateLimitWindow {
                    used_percent: 100.0,
                    resets_at: Some(1_700_000_000),
                    window_minutes: None,
                }),
                primary_observed_at: Some(1_699_827_200),
                secondary_observed_at: Some(1_700_000_000),
                credits: Some(2),
                refresh: None,
                cooldown_until: None,
                backend_resets_at: None,
            },
        }],
        total: 1,
        paused: false,
        settings: serde_json::json!({}),
        primary: None,
        fallback: codex_login::ApiAccountFallback::default(),
    }
}

fn render(question: MenuQuestion) -> String {
    format!(
        "{}\n{}",
        question.text,
        question
            .choices
            .into_iter()
            .map(|choice| format!("{}\n{}", choice.label, choice.description))
            .collect::<Vec<_>>()
            .join("\n\n")
    )
}

#[test]
fn native_chinese_overview_moves_quota_and_cache_age_out_of_heading() {
    let question =
        fixture().question_at(MenuPage::Overview(0), NativeAccountLanguage::Chinese, NOW);
    assert_eq!(question.text, "选择账号 · 1/1");
    assert_eq!(
        question.choices[0].action,
        MenuAction::Page(MenuPage::Detail(0))
    );
    insta::assert_snapshot!("mobile_chinese_account_choices", render(question));
}

#[test]
fn quota_details_preserve_individual_samples_and_expiry_in_regular_descriptions() {
    let question = fixture().question_at(MenuPage::Usage(0), NativeAccountLanguage::English, NOW);
    assert!(!question.text.contains('\n'));
    assert!(question.choices[0].description.contains("2 d old"));
    assert!(
        question.choices[1]
            .description
            .contains("Stale (100% cached)")
    );
    assert!(
        question.choices[2]
            .description
            .contains("alpha@example.test")
    );
    insta::assert_snapshot!("mobile_subscription_details", render(question));
}

#[test]
fn native_api_detail_explains_paid_use_without_subscription_quota() {
    let mut inventory = fixture();
    inventory.accounts[0].detail = AccountDetail::Api {
        account: codex_login::ApiAccount {
            id: "api-1".into(),
            label: "API".into(),
            base_url: "https://api.example.test/v1".into(),
            model: "provider/model-中文".into(),
            disabled: false,
            context_window: 32_768,
            images: false,
        },
        has_key: true,
    };
    inventory.accounts[0].state = "manual".into();
    insta::assert_snapshot!(
        "mobile_api_details",
        render(inventory.question_at(MenuPage::Usage(0), NativeAccountLanguage::English, NOW))
    );
    let actions = inventory.question_at(MenuPage::Actions(0), NativeAccountLanguage::English, NOW);
    assert_eq!(
        actions.choices[0].action,
        MenuAction::Prepare(MenuOperation::ApiUse(0))
    );
    assert!(actions.choices[0].description.contains("billed"));
}

#[test]
fn phone_pages_keep_short_headings_and_bounded_unique_choices() {
    let mut inventory = fixture();
    for _ in 0..127 {
        inventory.accounts.push(fixture().accounts.remove(0));
    }
    inventory.total = inventory.accounts.len();
    for page in [
        MenuPage::Home,
        MenuPage::Overview(0),
        MenuPage::Overview(31),
        MenuPage::Detail(0),
        MenuPage::Usage(0),
        MenuPage::Actions(0),
        MenuPage::More(0),
        MenuPage::Membership(0),
        MenuPage::ChoosePage { first: 0, end: 32 },
        MenuPage::ChoosePage { first: 31, end: 32 },
        MenuPage::Primary,
        MenuPage::Settings(0),
        MenuPage::Settings(1),
        MenuPage::Settings(2),
    ] {
        let question = inventory.question_at(page, NativeAccountLanguage::English, NOW);
        assert!(!question.text.contains('\n'));
        assert!(question.text.chars().count() <= 56);
        assert!((2..=5).contains(&question.choices.len()));
        let unique = question
            .choices
            .iter()
            .map(|choice| &choice.label)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), question.choices.len());
        assert!(
            question
                .choices
                .iter()
                .all(|choice| choice.label.chars().count() <= 56
                    && choice.description.chars().count() <= 640)
        );
    }
}

#[test]
fn all_captured_accounts_are_reachable_with_short_page_choices() {
    let mut inventory = fixture();
    inventory.accounts = (0..128).map(|_| fixture().accounts.remove(0)).collect();
    inventory.total = 128;
    let mut page = MenuPage::ChoosePage {
        first: 0,
        end: inventory.pages(),
    };
    let mut steps = 0;
    loop {
        steps += 1;
        assert!(steps <= 8);
        let question = inventory.question_at(page, NativeAccountLanguage::English, NOW);
        let choice = question
            .choices
            .iter()
            .filter(|choice| {
                matches!(
                    choice.action,
                    MenuAction::Page(MenuPage::ChoosePage { .. } | MenuPage::Overview(_))
                )
            })
            .rfind(|choice| choice.label != "Back")
            .unwrap();
        let MenuAction::Page(next) = choice.action else {
            unreachable!();
        };
        if let MenuPage::Overview(last) = next {
            assert_eq!(last, 31);
            let overview = inventory.question_at(next, NativeAccountLanguage::English, NOW);
            assert!(
                overview
                    .choices
                    .iter()
                    .any(|choice| choice.action == MenuAction::Page(MenuPage::Detail(127)))
            );
            break;
        }
        page = next;
    }
}

#[test]
fn non_finite_and_unknown_quota_are_kept_unknown_in_descriptions() {
    let mut inventory = fixture();
    let AccountDetail::Subscription {
        primary,
        secondary,
        primary_observed_at,
        ..
    } = &mut inventory.accounts[0].detail
    else {
        panic!("subscription fixture");
    };
    primary.as_mut().unwrap().used_percent = f64::NAN;
    *secondary = None;
    *primary_observed_at = None;
    let question =
        inventory.question_at(MenuPage::Overview(0), NativeAccountLanguage::English, NOW);
    assert!(
        question.choices[0]
            .description
            .contains("Primary (5h): Unknown · age unknown")
    );
    assert!(
        question.choices[0]
            .description
            .contains("Secondary: Unknown · just now")
    );
}

#[test]
fn small_positive_and_nearly_exhausted_quota_are_not_rounded_to_empty_or_full() {
    let mut inventory = fixture();
    let AccountDetail::Subscription {
        primary, secondary, ..
    } = &mut inventory.accounts[0].detail
    else {
        panic!("subscription fixture");
    };
    primary.as_mut().unwrap().used_percent = 0.25;
    secondary.as_mut().unwrap().used_percent = 99.9;
    secondary.as_mut().unwrap().resets_at = Some(NOW + 60);
    let question =
        inventory.question_at(MenuPage::Overview(0), NativeAccountLanguage::English, NOW);
    assert!(
        question.choices[0]
            .description
            .contains("Primary (5h): <1% used")
    );
    assert!(
        question.choices[0]
            .description
            .contains("Secondary: >99% used")
    );
}

#[test]
fn display_shortening_never_changes_the_exact_captured_account_id() {
    let mut inventory = fixture();
    let exact = format!("slot-{}", "x".repeat(140));
    inventory.accounts[0].id = exact.clone();
    inventory.accounts[0].label = "name\n\u{202e}duplicate".into();
    let question =
        inventory.question_at(MenuPage::Overview(0), NativeAccountLanguage::English, NOW);
    assert!(!question.choices[0].label.contains('\n'));
    assert!(!question.choices[0].label.contains('\u{202e}'));
    assert_eq!(inventory.accounts[0].id, exact);
}

#[test]
fn explicit_paid_confirmation_names_the_account_and_does_not_execute() {
    let inventory = fixture();
    let mut session = actions::NativeMenuSession::new(inventory);
    session
        .prepare(MenuOperation::PrimaryUse(0), NativeAccountLanguage::Chinese)
        .unwrap();
    let question = session.question(NativeAccountLanguage::Chinese);
    assert_eq!(question.choices[0].action, MenuAction::Apply);
    assert_eq!(
        question.choices[1].action,
        MenuAction::Page(MenuPage::Detail(0))
    );
    insta::assert_snapshot!("mobile_host_confirmation", render(question));
}

#[test]
fn native_host_summary_distinguishes_stored_source_from_runtime_resolution() {
    let mut inventory = fixture();
    inventory.primary = Some(crate::account_management::PrimaryLoginView {
        source: "root".into(),
        profile_id: None,
        label: "Host-managed workload identity".into(),
        email: None,
        ready: false,
        message: None,
        status: "runtimeResolutionRequired".into(),
        revision: 3,
        runtime: None,
    });
    let question = inventory.question_at(MenuPage::Home, NativeAccountLanguage::Chinese, NOW);
    let host = question
        .choices
        .iter()
        .find(|choice| choice.action == MenuAction::Page(MenuPage::Primary))
        .unwrap();
    assert!(host.description.contains("由运行中的主机确认"));
    assert!(!host.description.contains("尚不可用"));
    assert!(!host.description.contains("已连接中继"));
    insta::assert_snapshot!("mobile_host_resolution", &host.description);
}

#[test]
fn unset_settings_show_effective_enabled_values_and_the_default_wait_budget() {
    let inventory = fixture();
    let first = inventory.question_at(MenuPage::Settings(0), NativeAccountLanguage::English, NOW);
    let second = inventory.question_at(MenuPage::Settings(1), NativeAccountLanguage::English, NOW);
    for question in [&first, &second] {
        for choice in question.choices.iter().filter(|choice| {
            matches!(
                choice.action,
                MenuAction::Prepare(MenuOperation::Setting(1 | 2 | 5))
            )
        }) {
            assert!(
                choice
                    .description
                    .contains("Currently on; confirm to turn off")
            );
        }
    }
    assert!(second.choices[0].description.contains("360"));
    insta::assert_snapshot!(
        "mobile_effective_pool_settings",
        format!("{}\n\n{}", render(first), render(second))
    );
}

#[test]
fn incomplete_subscription_login_offers_completion_before_backend_actions() {
    let mut inventory = fixture();
    inventory.accounts[0].login_state = "pending".into();
    inventory.accounts[0].state = "needsLogin".into();
    let question = inventory.question_at(MenuPage::Actions(0), NativeAccountLanguage::English, NOW);
    assert_eq!(
        question
            .choices
            .iter()
            .map(|choice| choice.action)
            .collect::<Vec<_>>(),
        vec![
            MenuAction::Prepare(MenuOperation::Relogin(0)),
            MenuAction::Page(MenuPage::More(0)),
            MenuAction::Page(MenuPage::Detail(0)),
        ]
    );
    insta::assert_snapshot!("mobile_pending_login_actions", render(question));
}

#[test]
fn unavailable_api_actions_offer_guidance_without_paid_choices() {
    let mut inventory = fixture();
    inventory.accounts[0].detail = AccountDetail::Api {
        account: codex_login::ApiAccount {
            id: "slot-1".into(),
            label: "API".into(),
            base_url: "https://api.example.test/v1".into(),
            model: "paid/model".into(),
            disabled: false,
            context_window: 32_768,
            images: false,
        },
        has_key: false,
    };
    let mut rendered = Vec::new();
    for disabled in [false, true] {
        inventory.accounts[0].disabled = disabled;
        let question =
            inventory.question_at(MenuPage::Actions(0), NativeAccountLanguage::English, NOW);
        assert_eq!(
            question
                .choices
                .iter()
                .map(|choice| choice.action)
                .collect::<Vec<_>>(),
            vec![
                MenuAction::Page(MenuPage::Membership(0)),
                MenuAction::Page(MenuPage::Detail(0)),
            ]
        );
        rendered.push(render(question));
    }
    insta::assert_snapshot!("mobile_missing_api_key_actions", rendered.join("\n\n"));
}

#[test]
fn legacy_root_membership_actions_explain_that_root_login_is_retained() {
    let mut inventory = fixture();
    inventory.accounts[0].id = "legacy-root".into();
    let question =
        inventory.question_at(MenuPage::Membership(0), NativeAccountLanguage::English, NOW);
    assert_eq!(
        question
            .choices
            .iter()
            .map(|choice| choice.action)
            .collect::<Vec<_>>(),
        vec![
            MenuAction::Prepare(MenuOperation::Disable(0)),
            MenuAction::Prepare(MenuOperation::ClearLabel(0)),
            MenuAction::Prepare(MenuOperation::RemoveKeep(0)),
            MenuAction::Page(MenuPage::Actions(0)),
        ]
    );
    insta::assert_snapshot!("mobile_legacy_root_actions", render(question));
}

#[test]
fn quota_display_ages_across_page_changes_without_rebinding_the_frozen_target() {
    let mut inventory = fixture();
    let AccountDetail::Subscription {
        primary,
        primary_observed_at,
        ..
    } = &mut inventory.accounts[0].detail
    else {
        panic!("subscription fixture");
    };
    primary.as_mut().unwrap().resets_at = Some(NOW + 30);
    *primary_observed_at = Some(NOW);
    let initial = inventory.question_at(MenuPage::Overview(0), NativeAccountLanguage::English, NOW);
    assert!(
        initial.choices[0]
            .description
            .contains("10% used · just now")
    );
    let later = inventory.question_at(MenuPage::Usage(0), NativeAccountLanguage::English, NOW + 61);
    assert!(later.choices[0].description.contains("Stale (10% cached)"));
    assert!(later.choices[0].description.contains("1 m old"));
    assert!(later.choices[2].description.contains("ID: slot-1"));
    assert_eq!(inventory.accounts[0].id, "slot-1");
}

#[test]
fn host_runtime_heartbeat_expires_while_the_same_inventory_is_open() {
    let mut inventory = fixture();
    inventory.primary = Some(crate::account_management::PrimaryLoginView {
        source: "root".into(),
        profile_id: None,
        label: "Root login".into(),
        email: None,
        ready: true,
        message: None,
        status: "storedReady".into(),
        revision: 3,
        runtime: Some(crate::account_management::PrimaryRuntimeView {
            source_revision: 3,
            email: Some("host@example.test".into()),
            profile_id: None,
            remote_status: "connected".into(),
            observed_at: NOW,
        }),
    });
    let initial = inventory.question_at(MenuPage::Home, NativeAccountLanguage::English, NOW);
    assert!(
        initial.choices[2]
            .description
            .contains("Connected to relay")
    );
    let later = inventory.question_at(MenuPage::Home, NativeAccountLanguage::English, NOW + 11);
    assert!(
        later.choices[2]
            .description
            .contains("No recent running-host confirmation")
    );
    assert!(!later.choices[2].description.contains("Connected to relay"));
}

#[test]
fn incomplete_quota_refresh_keeps_window_ages_separate_from_the_last_check_and_cooldown() {
    use crate::account_management::ManagedAccountView;
    use crate::account_management::ManagedRateLimits;
    use crate::account_management::RefreshStatus;

    let inventory = FrozenAccountInventory::from_inventory(AccountManagerInventory {
        host_now: NOW,
        primary_login: None,
        paused: false,
        active_profile_id: None,
        accounts: vec![ManagedAccountView {
            profile_id: "quota-fixture".into(),
            label: "CEO fixture".into(),
            custom_label: None,
            priority: 10,
            disabled: false,
            login_state: "signedIn".into(),
            availability: "coolingDown".into(),
            cooldown_until: Some(NOW + 7200),
            backend_resets_at: Some(NOW + 7200),
            plan: Some("Business".into()),
            email: Some("quota@example.test".into()),
            rate_limits: ManagedRateLimits {
                primary: Some(ManagedRateLimitWindow {
                    used_percent: 41.0,
                    resets_at: Some(NOW + 7200),
                    window_minutes: Some(300),
                }),
                secondary: Some(ManagedRateLimitWindow {
                    used_percent: 31.0,
                    resets_at: Some(NOW + 86_400),
                    window_minutes: Some(10_080),
                }),
                observed_at: Some(NOW - 60),
                primary_observed_at: Some(NOW - 1680),
                secondary_observed_at: Some(NOW - 60),
            },
            reset_credit_count: None,
            refresh: Some(RefreshStatus {
                in_progress: false,
                attempted_at: NOW - 60,
                succeeded: false,
                message: "Primary window not refreshed (omitted by backend); Secondary window updated. Refresh again; cached values kept.".into(),
                reset_credit_count: None,
            }),
            warmup: None,
        }],
        settings: serde_json::json!({}),
        login_jobs: vec![],
        api_accounts: vec![],
        api_selection: codex_login::ApiAccountSelection::Subscription,
        api_fallback: codex_login::ApiAccountFallback::default(),
    });
    let question = inventory.question_at(MenuPage::Usage(0), NativeAccountLanguage::English, NOW);
    assert!(question.choices[0].description.contains("41% used"));
    assert!(question.choices[0].description.contains("28 m old"));
    assert!(question.choices[1].description.contains("31% used"));
    assert!(question.choices[1].description.contains("1 m old"));
    let status = question
        .choices
        .iter()
        .find(|choice| choice.label == "Quota check and status")
        .unwrap();
    assert!(status.description.contains("Last quota check: 1 m old"));
    assert!(status.description.contains("Primary window not refreshed"));
    assert!(
        status
            .description
            .contains("Cached percentages do not confirm recovery")
    );
    insta::assert_snapshot!("mobile_incomplete_quota_refresh", render(question));
}
