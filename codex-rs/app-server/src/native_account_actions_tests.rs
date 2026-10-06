use super::*;
use pretty_assertions::assert_eq;

#[path = "native_account_credits_tests.rs"]
mod credit_flow;
#[path = "native_account_actions_lifecycle_tests.rs"]
mod lifecycle;

#[test]
fn pending_reset_reuses_the_original_credit_and_key_even_without_a_listed_credit() {
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
    session.credit_target = Some(session.inventory.accounts[0].clone());
    session.credit_identity = Some("identity-binding".into());
    session
        .identities
        .insert("slot-1".into(), Some("identity-binding".into()));
    session.pending_reset = Some(NativePendingReset {
        owner_key: "opaque-reset-owner".into(),
        idempotency_key: "original-operation".into(),
        credit_id: Some("previous-credit".into()),
    });
    session.page = MenuPage::Credits(0, 0);
    insta::assert_snapshot!(
        "mobile_pending_reset_retry",
        render(session.question(NativeAccountLanguage::English))
    );
    session
        .prepare(
            MenuOperation::RetryPendingCredit(0),
            NativeAccountLanguage::English,
        )
        .unwrap();
    assert_eq!(
        session.pending.as_ref().unwrap().operation,
        AccountManagerOperation::Redeem {
            expected_owner_key: Some("opaque-reset-owner".into()),
            profile_id: "slot-1".into(),
            credit_id: "previous-credit".into(),
            idempotency_key: "original-operation".into(),
        }
    );
    assert!(session.redemption_keys.is_empty());
}

#[test]
fn unresolved_reset_prevents_a_new_credit_operation_or_an_unknown_credit_retry() {
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
    session.credit_target = Some(session.inventory.accounts[0].clone());
    session.pending_reset = Some(NativePendingReset {
        owner_key: "opaque-reset-owner".into(),
        idempotency_key: "original-operation".into(),
        credit_id: None,
    });
    session.credits.push(NativeCredit {
        id: "new-credit".into(),
        expires: "2099-01-01".into(),
        available: true,
    });
    assert!(
        session
            .prepare(MenuOperation::Redeem(0, 0), NativeAccountLanguage::English)
            .is_err()
    );
    assert!(
        session
            .prepare(
                MenuOperation::RetryPendingCredit(0),
                NativeAccountLanguage::English
            )
            .is_err()
    );
    assert!(session.pending.is_none());
    assert!(session.redemption_keys.is_empty());
}

#[test]
fn paid_confirmation_rejects_a_replaced_credential_with_unchanged_provider_metadata() {
    let mut expected = fixture().accounts.remove(0);
    expected.detail = AccountDetail::Api {
        account: codex_login::ApiAccount {
            id: "slot-1".into(),
            label: "API".into(),
            base_url: "https://api.example.test/v1".into(),
            model: "paid/model".into(),
            disabled: false,
            context_window: 32_768,
            images: false,
        },
        has_key: true,
        credential_revision: Some("credential-one".into()),
    };
    let mut actual = expected.clone();
    let AccountDetail::Api {
        credential_revision,
        ..
    } = &mut actual.detail
    else {
        unreachable!()
    };
    *credential_revision = Some("credential-two".into());
    assert!(!same_target(&expected, &actual));
}

#[test]
fn logical_location_tracks_the_displayed_account_after_reordering_or_removal() {
    let mut inventory = fixture();
    inventory.accounts = (0..6)
        .map(|index| {
            let mut account = fixture().accounts.remove(0);
            account.id = format!("profile-{index}");
            account.current = index == 0;
            account
        })
        .collect();
    let location = location::MenuLocation::capture(MenuPage::Quick(2), &inventory);
    inventory.accounts.rotate_left(2);
    assert_eq!(location.restore(&inventory), MenuPage::Quick(1));
    inventory
        .accounts
        .retain(|account| account.id != "profile-4");
    assert_eq!(location.restore(&inventory), MenuPage::Quick(2));
}

#[test]
fn credit_read_adopts_the_owner_bound_previous_key_without_a_listed_credit() {
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
    let owner = "a".repeat(64);
    let target = session.inventory.accounts[0].clone();
    session
        .load_credits(
            serde_json::json!({"resetOwnerKey":owner,"credits":[], "availableCount":null,"inventoryError":"synthetic inventory unavailable", "pendingResetCredit": {
                "ownerKey":owner,"idempotencyKey":"previous-key","creditId":"removed-credit"
            }}),
            Some(target),
            Some("saved-identity".into()),
            NativeAccountLanguage::English,
        )
        .unwrap();
    assert_eq!(
        session
            .redemption_keys
            .get(&(owner, "removed-credit".into()))
            .map(String::as_str),
        Some("previous-key")
    );
    assert!(session.credits.is_empty());
    let question = session.question(NativeAccountLanguage::English);
    insta::assert_snapshot!(
        "mobile_pending_reset_inventory_unavailable",
        question.choices[0].description
    );
    assert_eq!(
        question.choices[1].action,
        MenuAction::Prepare(MenuOperation::RetryPendingCredit(0))
    );
}

#[test]
fn same_credit_id_for_another_backend_owner_keeps_the_old_operation_binding() {
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
    let old_owner = "a".repeat(64);
    let new_owner = "b".repeat(64);
    session.redemption_keys.insert(
        (old_owner.clone(), "shared-credit-id".into()),
        "old-owner-operation".into(),
    );
    let target = session.inventory.accounts[0].clone();
    session.load_credits(serde_json::json!({"resetOwnerKey":new_owner, "pendingResetCredit":null,
        "credits":[{"id":"shared-credit-id","resetType":"codex_rate_limits","status":"available","expiresAt":"2099-01-01T00:00:00Z"}]}),
        Some(target), None, NativeAccountLanguage::English).unwrap();
    session
        .prepare(MenuOperation::Redeem(0, 0), NativeAccountLanguage::English)
        .unwrap();
    let AccountManagerOperation::Redeem {
        expected_owner_key,
        idempotency_key,
        ..
    } = &session.pending.as_ref().unwrap().operation
    else {
        unreachable!()
    };
    assert_eq!(expected_owner_key.as_deref(), Some(new_owner.as_str()));
    assert_ne!(idempotency_key, "old-owner-operation");
    assert_eq!(
        session
            .redemption_keys
            .get(&(old_owner, "shared-credit-id".into()))
            .map(String::as_str),
        Some("old-owner-operation")
    );
}

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
        routing: None,
        reset_journals: vec![],
        reset_total: 0,
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
            let mut session = NativeMenuSession::new(inventory, NativeMenuEntry::Manage);
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
        let mut session = NativeMenuSession::new(inventory, NativeMenuEntry::Manage);
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
        credential_revision: Some("test-credential-revision".into()),
    };
    let mut session = NativeMenuSession::new(inventory, NativeMenuEntry::Manage);
    session
        .prepare(
            MenuOperation::ApiFallback(0),
            NativeAccountLanguage::English,
        )
        .unwrap();
    assert_eq!(
        session.pending.as_ref().unwrap().operation,
        AccountManagerOperation::ApiFallback {
            expected_credential_revision: Some("test-credential-revision".into()),
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
            credential_revision: has_key.then(|| "test-credential-revision".into()),
        };
        let mut session = NativeMenuSession::new(inventory, NativeMenuEntry::Manage);
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
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
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
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
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
    let mut session = NativeMenuSession::new(inventory, NativeMenuEntry::Manage);
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
