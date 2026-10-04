use super::*;
use pretty_assertions::assert_eq;

fn fixture() -> FrozenAccountInventory {
    FrozenAccountInventory {
        accounts: vec![FrozenAccount {
            id: "slot-1".into(),
            label: "Work account".into(),
            current: true,
            state: "ready".into(),
            login_state: "signedIn".into(),
            disabled: false,
            detail: AccountDetail::Subscription {
                plan: "Business".into(),
                email: Some("work@example.test".into()),
                primary: None,
                secondary: None,
                primary_observed_at: None,
                secondary_observed_at: None,
                credits: None,
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
fn unset_enabled_settings_prepare_turning_them_off() {
    for (index, key) in [
        (1, "window_warmup"),
        (2, "resume_after_reset"),
        (5, "return_to_preferred"),
    ] {
        for (settings, next) in [
            (serde_json::json!({}), false),
            (serde_json::json!({key: false}), true),
        ] {
            let mut inventory = fixture();
            inventory.settings = settings;
            let mut session = NativeMenuSession::new(inventory);
            session
                .prepare(
                    MenuOperation::Setting(index),
                    NativeAccountLanguage::English,
                )
                .unwrap();
            assert_eq!(
                session.pending.unwrap().operation,
                AccountManagerOperation::Settings {
                    values: serde_json::json!({key: next})
                }
            );
        }
    }
}

#[test]
fn reset_wait_toggle_uses_the_effective_default_and_preserves_other_settings() {
    for (settings, expected) in [
        (serde_json::json!({}), 0),
        (serde_json::json!({"max_reset_wait_minutes": 0}), 360),
        (
            serde_json::json!({"resume_after_reset": false, "window_warmup_interval_minutes": 17}),
            0,
        ),
    ] {
        let mut inventory = fixture();
        inventory.settings = settings.clone();
        let mut session = NativeMenuSession::new(inventory);
        session
            .prepare(MenuOperation::Setting(3), NativeAccountLanguage::English)
            .unwrap();
        assert_eq!(
            session.pending.unwrap().operation,
            AccountManagerOperation::Settings {
                values: serde_json::json!({"max_reset_wait_minutes": expected})
            }
        );
        assert_eq!(session.inventory.settings, settings);
    }
}

#[test]
fn paid_fallback_confirmation_keeps_risk_and_exact_target_visible_when_metadata_is_long() {
    let mut inventory = fixture();
    let id = "api-captured-target".to_string();
    inventory.accounts[0].id = id.clone();
    inventory.accounts[0].label = "Paid provider".repeat(6);
    inventory.accounts[0].detail = AccountDetail::Api {
        account: codex_login::ApiAccount {
            id: id.clone(),
            label: "Paid provider".repeat(6),
            base_url: format!("https://api.example.test/v1/{}", "a".repeat(160)),
            model: "paid/model".repeat(14),
            disabled: false,
            context_window: 32_768,
            images: false,
        },
        has_key: true,
    };
    let mut session = NativeMenuSession::new(inventory);
    session
        .prepare(
            MenuOperation::ApiFallback(0),
            NativeAccountLanguage::English,
        )
        .unwrap();
    assert_eq!(
        session.pending.as_ref().unwrap().operation,
        AccountManagerOperation::ApiFallback {
            config: codex_login::ApiAccountFallback {
                enabled: true,
                profile_id: Some(id.clone()),
                wait_minutes: 5
            },
        }
    );
    let question = session.question(NativeAccountLanguage::English);
    let description = &question.choices[0].description;
    for expected in [
        "charges",
        "no hard spending cap",
        "https://api.example.test/v1",
        "paid/model",
        "Paid provider",
        &id,
    ] {
        assert!(
            description.contains(expected),
            "Missing {expected}: {description}"
        );
    }
    assert!(description.lines().count() <= 6);
    assert!(description.chars().count() <= 640);
    insta::assert_snapshot!("mobile_paid_fallback_confirmation", render(question));
}

#[test]
fn unavailable_api_target_cannot_prepare_paid_selection_or_fallback() {
    for (disabled, has_key) in [(true, true), (false, false)] {
        let mut inventory = fixture();
        inventory.accounts[0].disabled = disabled;
        inventory.accounts[0].detail = AccountDetail::Api {
            account: codex_login::ApiAccount {
                id: "slot-1".into(),
                label: "Paid provider".into(),
                base_url: "https://api.example.test/v1".into(),
                model: "paid/model".into(),
                disabled,
                context_window: 32_768,
                images: false,
            },
            has_key,
        };
        let mut session = NativeMenuSession::new(inventory);
        for operation in [MenuOperation::ApiUse(0), MenuOperation::ApiFallback(0)] {
            assert!(
                session
                    .prepare(operation, NativeAccountLanguage::English)
                    .is_err()
            );
            assert!(session.pending.is_none());
        }
    }
}

#[test]
fn chinese_receipts_translate_known_success_and_retain_unknown_cleanup_diagnostics() {
    let mut session = NativeMenuSession::new(fixture());
    session.show_result(
        AccountManagerResult {
            message: "Account selected for subsequent requests.".into(),
            data: serde_json::Value::Null,
        },
        MenuPage::Home,
    );
    assert_eq!(
        session.question(NativeAccountLanguage::Chinese).choices[0].description,
        "已选择此账号，后续请求将使用它。"
    );
    let known = render(session.question(NativeAccountLanguage::Chinese));
    let diagnostic = "Account removed from the pool; some cleanup remains: Scheduler cleanup failed: test diagnostic";
    session.show_result(
        AccountManagerResult {
            message: diagnostic.into(),
            data: serde_json::Value::Null,
        },
        MenuPage::Home,
    );
    assert_eq!(
        session.question(NativeAccountLanguage::Chinese).choices[0].description,
        diagnostic
    );
    insta::assert_snapshot!(
        "mobile_chinese_receipts",
        format!(
            "{known}\n\n{}",
            render(session.question(NativeAccountLanguage::Chinese))
        )
    );
}

#[test]
fn cancel_login_describes_cleanup_and_finish_routes_to_the_owned_pending_login() {
    let mut session = NativeMenuSession::new(fixture());
    session.login = Some(LoginProgress {
        operation_id: "owned-login".into(),
        profile_id: Some("slot-1".into()),
        verification_url: Some("https://example.test/verify".into()),
        user_code: Some("ABCD".into()),
        status: "waiting".into(),
        message: "Waiting for browser verification".into(),
    });
    session
        .prepare(MenuOperation::Relogin(0), NativeAccountLanguage::English)
        .unwrap();
    assert_eq!(session.page, MenuPage::Login);
    assert!(session.pending.is_none());
    insta::assert_snapshot!(
        "mobile_login_cleanup",
        render(session.question(NativeAccountLanguage::English))
    );
}

#[test]
fn legacy_root_removal_keeps_root_credentials_even_when_delete_is_requested() {
    let mut inventory = fixture();
    inventory.accounts[0].id = "legacy-root".into();
    let mut session = NativeMenuSession::new(inventory);
    session
        .prepare(
            MenuOperation::RemoveDelete(0),
            NativeAccountLanguage::English,
        )
        .unwrap();
    assert_eq!(
        session.pending.unwrap().operation,
        AccountManagerOperation::Remove {
            profile_id: "legacy-root".into(),
            keep_credentials: true
        }
    );
}
