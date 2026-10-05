use super::*;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome;
use pretty_assertions::assert_eq;

fn operation(owner: char, key: &str) -> ResetCreditOperation {
    ResetCreditOperation {
        owner_key: owner.to_string().repeat(64),
        idempotency_key: key.to_string(),
        credit_id: Some("synthetic-credit".to_string()),
    }
}

#[test]
fn pending_reset_survives_restart_without_rebinding() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let original = operation('a', "original-request");
    ResetCreditJournal::remember(home.path(), &original)?;
    assert_eq!(
        ResetCreditJournal::read(home.path())?,
        vec![original.clone()]
    );
    assert!(ResetCreditJournal::remember(home.path(), &operation('a', "new-request")).is_err());
    assert_eq!(ResetCreditJournal::read(home.path())?, vec![original]);
    Ok(())
}

#[test]
fn terminal_receipt_cannot_remove_another_owners_pending_reset() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let first = operation('a', "request-a");
    let second = operation('b', "request-b");
    ResetCreditJournal::remember(home.path(), &first)?;
    ResetCreditJournal::remember(home.path(), &second)?;
    assert!(ResetCreditJournal::settle(home.path(), &operation('a', "wrong-key")).is_err());
    ResetCreditJournal::settle(home.path(), &first)?;
    assert_eq!(ResetCreditJournal::read(home.path())?, vec![second]);
    Ok(())
}

#[test]
fn damaged_journal_blocks_new_reset_instead_of_replacing_old_state() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join(RESET_CREDIT_JOURNAL_FILE);
    std::fs::write(&path, b"incomplete JSON")?;
    assert!(ResetCreditJournal::remember(home.path(), &operation('a', "new-key")).is_err());
    assert_eq!(std::fs::read(path)?, b"incomplete JSON");
    Ok(())
}

#[test]
fn concurrent_local_clients_cannot_overwrite_an_owners_pending_operation() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let original = operation('a', "request-a");
    assert!(ResetCreditJournal::read(home.path())?.is_empty());
    assert!(ResetCreditJournal::read(home.path())?.is_empty());
    ResetCreditJournal::remember(home.path(), &original)?;
    assert!(ResetCreditJournal::remember(home.path(), &operation('a', "request-b")).is_err());
    assert_eq!(ResetCreditJournal::read(home.path())?, vec![original]);
    Ok(())
}

#[test]
fn oversized_or_unknown_version_journal_stays_fail_closed() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join(RESET_CREDIT_JOURNAL_FILE);
    for bytes in [
        br#"{"version":99,"pending":[]}"#.to_vec(),
        vec![b' '; MAX_RESET_CREDIT_JOURNAL_BYTES + 1],
    ] {
        std::fs::write(&path, &bytes)?;
        assert!(ResetCreditJournal::read(home.path()).is_err());
        assert!(ResetCreditJournal::remember(home.path(), &operation('a', "new-key")).is_err());
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    Ok(())
}

async fn prepared_app() -> (Box<App>, u64, ResetCreditOperation) {
    let (mut app, _events, _ops) = crate::app::tests::make_test_app_with_channels().await;
    let operation = operation('a', "request-a");
    ResetCreditJournal::remember(app.local_settings.codex_home.as_path(), &operation).unwrap();
    app.restore_reset_credit_operations();
    app.chat_widget
        .set_reset_credit_owner(Some(operation.owner_key.clone()));
    let request_id = app
        .prepare_reset_credit_operation(&operation)
        .expect("saved reset");
    (app, request_id, operation)
}

#[tokio::test]
async fn widget_rebuild_retains_pending_reset_and_rejects_old_numeric_reply() {
    let (mut app, request_id, original) = prepared_app().await;
    let (replacement, _tx, _rx, _ops) = make_chatwidget_manual_with_sender().await;
    app.replace_chat_widget(replacement);
    let new_request_id = app.chat_widget.show_rate_limit_reset_loading_popup();
    assert_ne!(request_id, new_request_id);
    let response = serde_json::from_value(serde_json::json!({
        "resetOwnerKey": "b".repeat(64), "rateLimits": {},
        "rateLimitResetCredits": {"availableCount": 0, "credits": null}
    }))
    .unwrap();
    app.observe_reset_credit_read(&response);
    assert!(!app.finish_reset_credit_operation(
        request_id,
        original.clone(),
        Err("lost response".to_string()),
    ));
    assert_eq!(
        ResetCreditJournal::read(app.local_settings.codex_home.as_path()).unwrap(),
        vec![original.clone()],
    );
    assert!(app.chat_widget.finish_rate_limit_reset_credits_refresh(
        new_request_id,
        Vec::new(),
        Some("b".repeat(64)),
        Ok(codex_app_server_protocol::RateLimitResetCreditsSummary {
            available_count: 0,
            credits: None
        }),
    ));
    assert!(
        crate::chatwidget::tests::helpers::render_bottom_popup(&app.chat_widget, 90)
            .contains("Return to the original account")
    );
}

#[tokio::test]
async fn late_success_settles_original_owner_without_changing_new_owner_ui() {
    let (mut app, request_id, original) = prepared_app().await;
    let second = operation('b', "request-b");
    ResetCreditJournal::remember(app.local_settings.codex_home.as_path(), &second).unwrap();
    let (replacement, _tx, _rx, _ops) = make_chatwidget_manual_with_sender().await;
    app.replace_chat_widget(replacement);
    app.chat_widget
        .set_reset_credit_owner(Some(second.owner_key.clone()));
    let response = serde_json::from_value(serde_json::json!({
        "resetOwnerKey": "b".repeat(64), "rateLimits": {}, "rateLimitUpsell": {
            "banner_type": "selected_model_limit", "model_slug": "test-model-a", "presentation": "inline",
            "title": "Selected model usage exhausted", "description": "Contact your owner.",
            "ctas": [{"action": "notify_owner", "label": "Notify owner"}]
        }
    })).unwrap();
    app.chat_widget.set_model("test-model-a");
    app.chat_widget.update_backend_banner(&response);
    let before = crate::chatwidget::tests::helpers::render_bottom_popup(&app.chat_widget, 90);
    assert!(before.contains("Selected model usage exhausted"));
    let generation = app.rate_limit_hard_stop_generation;
    assert!(!app.finish_reset_credit_operation(
        request_id,
        original,
        Ok(ConsumeAccountRateLimitResetCreditResponse {
            outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset
        }),
    ));
    assert_eq!(
        ResetCreditJournal::read(app.local_settings.codex_home.as_path()).unwrap(),
        vec![second.clone()]
    );
    assert_eq!(app.reset_credit_operations.pending, vec![second]);
    assert_eq!(app.rate_limit_hard_stop_generation, generation);
    assert_eq!(
        crate::chatwidget::tests::helpers::render_bottom_popup(&app.chat_widget, 90),
        before
    );
}

#[tokio::test]
async fn mismatched_full_tuple_cannot_settle_the_original_reset() {
    let (mut app, request_id, original) = prepared_app().await;
    for changed in [
        ResetCreditOperation {
            owner_key: "b".repeat(64),
            ..original.clone()
        },
        ResetCreditOperation {
            idempotency_key: "different-key".to_string(),
            ..original.clone()
        },
        ResetCreditOperation {
            credit_id: Some("other-credit".to_string()),
            ..original.clone()
        },
    ] {
        assert!(!app.finish_reset_credit_operation(
            request_id,
            changed,
            Ok(ConsumeAccountRateLimitResetCreditResponse {
                outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset,
            })
        ));
    }
    assert_eq!(
        ResetCreditJournal::read(app.local_settings.codex_home.as_path()).unwrap(),
        vec![original]
    );
    assert!(app.reset_credit_operations.in_flight.is_some());
}

#[tokio::test]
async fn every_definitive_outcome_settles_before_any_quota_refresh() {
    for outcome in [
        ConsumeAccountRateLimitResetCreditOutcome::Reset,
        ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed,
        ConsumeAccountRateLimitResetCreditOutcome::NothingToReset,
        ConsumeAccountRateLimitResetCreditOutcome::NoCredit,
    ] {
        let (mut app, request_id, original) = prepared_app().await;
        app.finish_reset_credit_operation(
            request_id,
            original,
            Ok(ConsumeAccountRateLimitResetCreditResponse { outcome }),
        );
        assert!(
            ResetCreditJournal::read(app.local_settings.codex_home.as_path())
                .unwrap()
                .is_empty()
        );
        assert!(app.reset_credit_operations.pending.is_empty());
    }
}

#[tokio::test]
async fn journal_failure_prevents_dispatch_and_preserves_existing_bytes() {
    let (mut app, _events, _ops) = crate::app::tests::make_test_app_with_channels().await;
    let operation = operation('a', "request-a");
    app.chat_widget
        .set_reset_credit_owner(Some(operation.owner_key.clone()));
    app.chat_widget
        .set_pending_reset_credit_operation(Some(operation.clone()), None);
    let path = app
        .local_settings
        .codex_home
        .join(RESET_CREDIT_JOURNAL_FILE);
    std::fs::write(&path, b"broken journal").unwrap();
    assert_eq!(app.prepare_reset_credit_operation(&operation), None);
    assert!(app.reset_credit_operations.in_flight.is_none());
    assert_eq!(std::fs::read(path).unwrap(), b"broken journal");
}

#[tokio::test]
async fn terminal_journal_write_failure_keeps_the_original_request_for_review() {
    let (mut app, request_id, original) = prepared_app().await;
    let home = app.local_settings.codex_home.as_path();
    let _lock = AccountRuntimeStateStore::new(home.to_path_buf())
        .try_lock_reset_credit()
        .unwrap()
        .expect("lock");
    assert!(!app.finish_reset_credit_operation(
        request_id,
        original.clone(),
        Ok(ConsumeAccountRateLimitResetCreditResponse {
            outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset,
        })
    ));
    assert_eq!(app.reset_credit_operations.pending, vec![original]);
    assert!(app.reset_credit_operations.post_consume.is_none());
}

#[tokio::test]
async fn server_pending_reset_is_adopted_even_when_its_credit_is_no_longer_listed() {
    let (mut app, _events, _ops) = crate::app::tests::make_test_app_with_channels().await;
    let original = operation('a', "server-original-request");
    let response = serde_json::from_value(serde_json::json!({
        "resetOwnerKey": original.owner_key,
        "pendingResetCredit": {
            "ownerKey": original.owner_key,
            "idempotencyKey": original.idempotency_key,
            "creditId": original.credit_id,
        },
        "rateLimits": {},
        "rateLimitResetCredits": {"availableCount": 0, "credits": []}
    }))
    .unwrap();
    app.observe_reset_credit_read(&response);
    assert_eq!(
        ResetCreditJournal::read(app.local_settings.codex_home.as_path()).unwrap(),
        vec![original.clone()]
    );
    assert!(app.prepare_reset_credit_operation(&original).is_some());
}

#[tokio::test]
async fn local_unconfirmed_tuple_takes_precedence_over_new_server_selection() {
    for pending in [
        operation('a', "server-different-request"),
        operation('a', "client-original-request"),
    ] {
        let (mut app, _events, _ops) = crate::app::tests::make_test_app_with_channels().await;
        let original = ResetCreditOperation {
            credit_id: None,
            ..operation('a', "client-original-request")
        };
        ResetCreditJournal::remember(app.local_settings.codex_home.as_path(), &original).unwrap();
        let response = serde_json::from_value(serde_json::json!({
            "resetOwnerKey": original.owner_key,
            "pendingResetCredit": {
                "ownerKey": pending.owner_key,
                "idempotencyKey": pending.idempotency_key,
                "creditId": pending.credit_id,
            },
            "rateLimits": {},
            "rateLimitResetCredits": {"availableCount": 3, "credits": null}
        }))
        .unwrap();
        app.observe_reset_credit_read(&response);
        assert_eq!(
            ResetCreditJournal::read(app.local_settings.codex_home.as_path()).unwrap(),
            vec![original.clone()]
        );
        assert!(app.prepare_reset_credit_operation(&original).is_some());
    }
}

#[tokio::test]
async fn a_new_app_restores_the_original_operation_without_inferred_quota_recovery() {
    let (mut app, request_id, original) = prepared_app().await;
    app.finish_reset_credit_operation(
        request_id,
        original.clone(),
        Err("connection lost".to_string()),
    );
    let (mut restarted, _events, _ops) = crate::app::tests::make_test_app_with_channels().await;
    restarted.local_settings.codex_home = app.local_settings.codex_home.clone();
    restarted.restore_reset_credit_operations();
    let response = serde_json::from_value(serde_json::json!({
        "resetOwnerKey": original.owner_key, "pendingResetCredit": null,
        "rateLimits": {"primary": {"usedPercent": 0}},
        "rateLimitResetCredits": {"availableCount": 3, "credits": null}
    }))
    .unwrap();
    restarted.observe_reset_credit_read(&response);
    assert_eq!(
        restarted.reset_credit_operations.pending,
        vec![original.clone()]
    );
    assert!(
        restarted
            .prepare_reset_credit_operation(&original)
            .is_some()
    );
}

#[tokio::test]
async fn switching_to_another_owner_keeps_its_own_pending_operation_accessible() {
    let (mut app, _, first) = prepared_app().await;
    let second = operation('b', "request-b");
    ResetCreditJournal::remember(app.local_settings.codex_home.as_path(), &second).unwrap();
    let response = serde_json::from_value(serde_json::json!({
        "resetOwnerKey": second.owner_key, "rateLimits": {},
        "rateLimitResetCredits": {"availableCount": 0, "credits": null}
    }))
    .unwrap();
    app.observe_reset_credit_read(&response);
    let request_id = app.chat_widget.show_rate_limit_reset_loading_popup();
    assert!(app.chat_widget.finish_rate_limit_reset_credits_refresh(
        request_id,
        Vec::new(),
        Some(second.owner_key.clone()),
        Ok(codex_app_server_protocol::RateLimitResetCreditsSummary {
            available_count: 0,
            credits: None
        }),
    ));
    let rendered = crate::chatwidget::tests::helpers::render_bottom_popup(&app.chat_widget, 90);
    assert!(rendered.contains("Review original reset"), "{rendered}");
    assert!(
        !rendered.contains("Return to the original account"),
        "{rendered}"
    );
    assert_eq!(
        ResetCreditJournal::read(app.local_settings.codex_home.as_path()).unwrap(),
        vec![first, second]
    );
}
