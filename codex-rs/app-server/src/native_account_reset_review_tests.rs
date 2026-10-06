use super::*;
use pretty_assertions::assert_eq;

fn fixture() -> FrozenAccountInventory {
    FrozenAccountInventory {
        accounts: vec![],
        total: 0,
        paused: false,
        settings: serde_json::json!({}),
        primary: None,
        fallback: codex_login::ApiAccountFallback::default(),
        routing: None,
        reset_journals: vec![crate::account_management::ResetJournalView {
            file_name: ".manual-rate-limit-reset-credits.json".into(),
            digest: "b".repeat(64),
            profile_id: None,
            attempted_at: None,
            legacy: false,
            archive_available: false,
            message:
                "Manual reset outcome remains unconfirmed; its original account may be unavailable."
                    .into(),
            manual: Some(crate::reset_credit_journal::ManualResetCreditReviewView {
                owner_key: "a".repeat(64),
                idempotency_key: "original-operation".into(),
                credit_id: "original-credit".into(),
                digest: "b".repeat(64),
                legacy: false,
            }),
        }],
        reset_total: 1,
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
fn deleted_account_manual_review_freezes_the_original_binding_and_requires_confirmation() {
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
    let page = session
        .inventory
        .reset_review_question(MenuPage::ResetReviews(0), NativeAccountLanguage::English);
    session
        .prepare_reset_review(0, NativeAccountLanguage::Chinese)
        .unwrap();
    assert_eq!(
        session.pending.as_ref().unwrap().operation,
        AccountManagerOperation::ManualResetReview {
            owner_key: "a".repeat(64),
            idempotency_key: "original-operation".into(),
            expected_digest: "b".repeat(64),
            acknowledge_unconfirmed: true,
        }
    );
    assert!(session.pending.as_ref().unwrap().target.is_none());
    assert!(session.inventory.accounts.is_empty());
    insta::assert_snapshot!(
        "mobile_interrupted_reset_review",
        format!(
            "{}\n---\n{}",
            render(page),
            render(session.question(NativeAccountLanguage::Chinese))
        )
    );
}

#[test]
fn review_rejects_changed_binding_credit_digest_and_a_removed_record() {
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
    session
        .prepare_reset_review(0, NativeAccountLanguage::English)
        .unwrap();
    let operation = &session.pending.as_ref().unwrap().operation;
    for change in 0..4 {
        let mut fresh = fixture();
        let manual = fresh.reset_journals[0].manual.as_mut().unwrap();
        match change {
            0 => manual.owner_key = "c".repeat(64),
            1 => manual.credit_id = "replacement-credit".into(),
            2 => manual.digest = "d".repeat(64),
            3 => fresh.reset_journals.clear(),
            _ => unreachable!(),
        }
        assert!(validate_reset_review(operation, &session.inventory, &fresh).is_err());
    }
    assert!(validate_reset_review(operation, &session.inventory, &fixture()).is_ok());
}

#[test]
fn pending_automatic_record_without_archive_permission_is_read_only() {
    let mut inventory = fixture();
    let record = &mut inventory.reset_journals[0];
    record.manual = None;
    record.profile_id = Some("removed-profile".into());
    record.message = "Owner-bound reset is awaiting confirmed recovery.".into();
    let question =
        inventory.reset_review_question(MenuPage::ResetReviews(0), NativeAccountLanguage::English);
    assert_eq!(
        question.choices[0].action,
        MenuAction::Page(MenuPage::ResetRecord(0))
    );
    let mut session = NativeMenuSession::new(inventory, NativeMenuEntry::Manage);
    assert!(
        session
            .prepare_reset_review(0, NativeAccountLanguage::English)
            .is_err()
    );
    assert!(session.pending.is_none());
}

#[test]
fn reset_review_pagination_keeps_each_exact_record_reachable() {
    let mut inventory = fixture();
    let record = inventory.reset_journals[0].clone();
    inventory.reset_journals = (0..10)
        .map(|index| {
            let mut record = record.clone();
            record.manual.as_mut().unwrap().idempotency_key = format!("operation-{index}");
            record
        })
        .collect();
    let mut reached = Vec::new();
    for page in 0..4 {
        let question = inventory
            .reset_review_question(MenuPage::ResetReviews(page), NativeAccountLanguage::English);
        assert!(!question.text.contains('\n'));
        assert!(question.choices.len() <= 6);
        reached.extend(
            question
                .choices
                .iter()
                .filter_map(|choice| match choice.action {
                    MenuAction::Prepare(MenuOperation::ResetReview(index)) => Some(index),
                    _ => None,
                }),
        );
    }
    assert_eq!(reached, (0..10).collect::<Vec<_>>());
}
