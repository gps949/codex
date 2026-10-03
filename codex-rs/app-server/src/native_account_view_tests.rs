use super::*;
use pretty_assertions::assert_eq;

fn fixture() -> FrozenAccountInventory {
    FrozenAccountInventory {
        accounts: vec![FrozenAccount {
            id: "slot-1".into(),
            label: "Alpha 中文".into(),
            current: true,
            state: "coolingDown".into(),
            detail: AccountDetail::Subscription {
                plan: "Business".into(),
                email: "alpha@example.test".into(),
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
            },
        }],
        total: 1,
        paused: false,
        observed_at: 1_700_000_000,
    }
}

#[test]
fn native_chinese_overview_retains_account_data_and_explicit_cache_guidance() {
    let question = fixture().question(MenuPage::Overview(0), NativeAccountLanguage::Chinese);
    insta::assert_snapshot!(question.text, @r"
    账号概览 · 1/1
    缓存快照；实际额度可能已变化。

    1. Alpha 中文 · 当前 · 等待重置
       主额度 (5 小时): 10% 已用 · 2 天前记录
       次额度: 已过期 (100% 缓存) · 刚刚记录

    本地快照创建时间: 11-14 22:13 UTC
    ");
    assert_eq!(
        question.choices,
        vec![
            ("账号详情".into(), MenuAction::Detail(0)),
            ("关闭".into(), MenuAction::Close)
        ]
    );
}

#[test]
fn native_api_detail_keeps_model_identity_and_separates_subscription_quota() {
    let mut inventory = fixture();
    inventory.accounts[0].detail = AccountDetail::Api {
        model: "provider/model-中文".into(),
        has_key: true,
    };
    inventory.accounts[0].state = "manual".into();
    insta::assert_snapshot!(inventory.question(MenuPage::Detail(0), NativeAccountLanguage::English).text, @r"
    Account details · 1/1
    Alpha 中文
    ID: slot-1
    State: Manual API
    API · provider/model-中文
    Key configured
    Subscription quota does not apply. API use may incur charges.

    Read-only snapshot. Reopen the menu to load current local data.
    ");
}

#[test]
fn page_actions_capture_first_detail_row_and_wrap_without_ambiguous_labels() {
    let mut inventory = fixture();
    for _ in 0..6 {
        inventory.accounts.push(fixture().accounts.remove(0));
    }
    inventory.total = inventory.accounts.len();
    let page = inventory.question(MenuPage::Overview(1), NativeAccountLanguage::English);
    assert_eq!(
        page.choices,
        vec![
            ("Account details".into(), MenuAction::Detail(5)),
            ("Next page".into(), MenuAction::Overview(0)),
            ("Close".into(), MenuAction::Close)
        ]
    );
    let detail = inventory.question(MenuPage::Detail(6), NativeAccountLanguage::English);
    assert_eq!(
        detail.choices,
        vec![
            ("Overview".into(), MenuAction::Overview(1)),
            ("Next account".into(), MenuAction::Detail(0)),
            ("Close".into(), MenuAction::Close)
        ]
    );
}

#[test]
fn unknown_and_non_finite_quota_are_not_misrepresented_as_available() {
    let mut inventory = fixture();
    let AccountDetail::Subscription {
        primary, secondary, ..
    } = &mut inventory.accounts[0].detail
    else {
        panic!("subscription fixture");
    };
    *primary = Some(ManagedRateLimitWindow {
        used_percent: f64::NAN,
        resets_at: None,
        window_minutes: None,
    });
    *secondary = None;
    let question = inventory.question(MenuPage::Overview(0), NativeAccountLanguage::English);
    assert!(question.text.contains("Primary: Unknown · 2 d old"));
    assert!(
        question
            .text
            .contains("Secondary: Unknown · recorded just now")
    );
}

#[test]
fn small_positive_and_nearly_exhausted_quota_do_not_round_to_zero_or_full() {
    let mut inventory = fixture();
    let AccountDetail::Subscription {
        primary, secondary, ..
    } = &mut inventory.accounts[0].detail
    else {
        panic!("subscription fixture");
    };
    primary.as_mut().unwrap().used_percent = 0.25;
    secondary.as_mut().unwrap().used_percent = 99.9;
    secondary.as_mut().unwrap().resets_at = Some(inventory.observed_at + 60);
    let question = inventory.question(MenuPage::Overview(0), NativeAccountLanguage::English);
    assert!(question.text.contains("Primary (5h): <1% used"));
    assert!(question.text.contains("Secondary: >99% used"));
}

#[test]
fn last_captured_account_is_reachable_within_the_session_step_cap() {
    let mut inventory = fixture();
    inventory.accounts = (0..128).map(|_| fixture().accounts.remove(0)).collect();
    inventory.total = inventory.accounts.len();
    let mut page = MenuPage::Home;
    let mut steps = 0;
    loop {
        steps += 1;
        assert!(
            steps <= 12,
            "last account must be accessible before the session closes"
        );
        let question = inventory.question(page, NativeAccountLanguage::English);
        assert!((2..=3).contains(&question.choices.len()));
        let unique = question
            .choices
            .iter()
            .map(|(label, _)| label)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), question.choices.len());
        let action = match page {
            MenuPage::Home => MenuAction::Overview(0),
            MenuPage::Overview(25) => MenuAction::Detail(125),
            MenuPage::Overview(_) => MenuAction::ChoosePage { first: 0, end: 26 },
            MenuPage::ChoosePage { .. } => question
                .choices
                .iter()
                .map(|(_, action)| *action)
                .rfind(|action| {
                    matches!(
                        action,
                        MenuAction::ChoosePage { .. } | MenuAction::Overview(25)
                    )
                })
                .unwrap(),
            MenuPage::Detail(127) => break,
            MenuPage::Detail(index) => MenuAction::Detail(index + 1),
        };
        assert!(
            question
                .choices
                .iter()
                .any(|(_, captured)| *captured == action)
        );
        page = match action {
            MenuAction::Overview(index) => MenuPage::Overview(index),
            MenuAction::Detail(index) => MenuPage::Detail(index),
            MenuAction::ChoosePage { first, end } => MenuPage::ChoosePage { first, end },
            MenuAction::Home | MenuAction::Language | MenuAction::Close => {
                panic!("expected a navigation action")
            }
        };
    }
    assert_eq!(steps, 11);
}

#[test]
fn detail_shows_each_window_own_sample_time_and_known_duration() {
    insta::assert_snapshot!(fixture().question(MenuPage::Detail(0), NativeAccountLanguage::English).text, @r"
    Account details · 1/1
    Alpha 中文
    ID: slot-1
    State: Cooling down
    Subscription: Business
    Email: alpha@example.test
    Primary (5h): 10% used
      Observed: 11-12 22:13 UTC · 2 d old
    Secondary: Stale (100% cached) · Reset 11-14 22:13 UTC
      Observed: 11-14 22:13 UTC · recorded just now
    Reset credits (cached): 2

    Read-only snapshot. Reopen the menu to load current local data.
    ");
}

#[test]
fn unknown_window_sample_time_does_not_inherit_recent_aggregate_metadata() {
    let mut inventory = fixture();
    let AccountDetail::Subscription {
        primary_observed_at,
        ..
    } = &mut inventory.accounts[0].detail
    else {
        panic!("subscription fixture");
    };
    *primary_observed_at = None;
    let question = inventory.question(MenuPage::Overview(0), NativeAccountLanguage::English);
    assert!(
        question
            .text
            .contains("Primary (5h): 10% used · age unknown")
    );
    assert!(
        question
            .text
            .contains("Secondary: Stale (100% cached) · recorded just now")
    );
}

#[test]
fn native_page_range_selector_has_short_unique_options_and_accessible_last_page() {
    let mut inventory = fixture();
    inventory.accounts = (0..128).map(|_| fixture().accounts.remove(0)).collect();
    inventory.total = inventory.accounts.len();
    let range = inventory.question(
        MenuPage::ChoosePage { first: 0, end: 26 },
        NativeAccountLanguage::English,
    );
    let last = inventory.question(
        MenuPage::ChoosePage { first: 25, end: 26 },
        NativeAccountLanguage::English,
    );
    insta::assert_snapshot!(format!("{}\n{}\n\n{}\n{}", range.text,
        range.choices.iter().map(|(label, _)| label.as_str()).collect::<Vec<_>>().join(" | "),
        last.text, last.choices.iter().map(|(label, _)| label.as_str()).collect::<Vec<_>>().join(" | ")), @r"
    Choose a page · 1–26
    Select a page range, then a page. All captured accounts are reachable.
    Pages 1–13 | Pages 14–26 | Back

    Choose a page · 26–26
    Select a page range, then a page. All captured accounts are reachable.
    Page 26 · 126–128 | Back
    ");
}
