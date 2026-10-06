use super::*;
use pretty_assertions::assert_eq;

fn load(session: &mut NativeMenuSession, data: serde_json::Value) {
    let target = session.inventory.accounts[0].clone();
    session
        .identities
        .insert(target.id.clone(), Some("identity".into()));
    session
        .load_credits(
            data,
            Some(target),
            Some("identity".into()),
            NativeAccountLanguage::English,
        )
        .unwrap();
}

#[test]
fn direct_reset_chooses_the_earliest_expiry_and_keeps_the_browse_path() {
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
    let question = session
        .inventory
        .question(MenuPage::Detail(0), NativeAccountLanguage::English);
    assert!(
        question
            .choices
            .iter()
            .any(|choice| choice.action == MenuAction::Execute(MenuOperation::UseResetCredit(0)))
    );
    let actions = session
        .inventory
        .question(MenuPage::Actions(0), NativeAccountLanguage::English);
    assert!(
        actions
            .choices
            .iter()
            .any(|choice| choice.action == MenuAction::Execute(MenuOperation::Credits(0)))
    );
    load(
        &mut session,
        serde_json::json!({"resetOwnerKey":"a".repeat(64),"pendingResetCredit":null,"credits":[
            {"id":"no-expiry","resetType":"codex_rate_limits","status":"available"},
            {"id":"expired","resetType":"codex_rate_limits","status":"available","expiresAt":"2000-01-01T00:00:00Z"},
            {"id":"later","resetType":"codex_rate_limits","status":"available","expiresAt":"2099-03-01T00:00:00Z"},
            {"id":"different-scope","resetType":"other","status":"available","expiresAt":"2099-01-01T00:00:00Z"},
            {"id":"earliest","resetType":"codex_rate_limits","status":"available","expiresAt":"2099-02-01T00:00:00Z"}
        ]}),
    );
    session
        .prepare_reset_credit(0, NativeAccountLanguage::English)
        .unwrap();
    let pending = session.pending.as_ref().unwrap();
    let AccountManagerOperation::Redeem {
        profile_id,
        credit_id,
        expected_owner_key,
        ..
    } = &pending.operation
    else {
        unreachable!()
    };
    assert_eq!(
        (
            profile_id.as_str(),
            credit_id.as_str(),
            expected_owner_key.as_deref()
        ),
        ("slot-1", "earliest", Some("a".repeat(64).as_str()))
    );
    insta::assert_snapshot!(
        "mobile_direct_reset_confirmation",
        format!(
            "{}\n---\n{}",
            render(question),
            render(session.question(NativeAccountLanguage::English))
        )
    );
}

#[test]
fn direct_reset_restores_the_original_tuple_before_a_new_credit() {
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
    let owner = "a".repeat(64);
    let previous = NativePendingReset {
        owner_key: owner.clone(),
        idempotency_key: "original-key".into(),
        credit_id: Some("original-credit".into()),
    };
    session.pending_reset = Some(previous.clone());
    load(
        &mut session,
        serde_json::json!({"resetOwnerKey":owner,"pendingResetCredit":null,"credits":[
            {"id":"new-credit","resetType":"codex_rate_limits","status":"available","expiresAt":"2099-01-01T00:00:00Z"}
        ]}),
    );
    session
        .prepare_reset_credit(0, NativeAccountLanguage::English)
        .unwrap();
    assert_eq!(
        session.pending.as_ref().unwrap().operation,
        AccountManagerOperation::Redeem {
            profile_id: "slot-1".into(),
            credit_id: "original-credit".into(),
            idempotency_key: "original-key".into(),
            expected_owner_key: Some(owner),
        }
    );
    assert_eq!(
        session.pending.as_ref().unwrap().pending_reset,
        Some(previous)
    );
}

#[test]
fn visiting_another_owner_keeps_the_original_unconfirmed_reset() {
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    let pending = serde_json::json!({"ownerKey":first,"idempotencyKey":"original-key","creditId":"original-credit"});
    load(
        &mut session,
        serde_json::json!({"resetOwnerKey":first,"pendingResetCredit":pending,"credits":[]}),
    );
    load(
        &mut session,
        serde_json::json!({"resetOwnerKey":second,"pendingResetCredit":null,"credits":[]}),
    );
    assert_eq!(session.pending_reset, None);
    load(
        &mut session,
        serde_json::json!({"resetOwnerKey":first,"pendingResetCredit":null,"credits":[]}),
    );
    assert_eq!(
        session.pending_reset,
        Some(NativePendingReset {
            owner_key: first,
            idempotency_key: "original-key".into(),
            credit_id: Some("original-credit".into())
        })
    );
}

#[test]
fn direct_reset_keeps_an_unknown_credit_pending_without_creating_a_request() {
    let mut session = NativeMenuSession::new(fixture(), NativeMenuEntry::Manage);
    load(
        &mut session,
        serde_json::json!({"resetOwnerKey":"a".repeat(64),"pendingResetCredit":{
        "ownerKey":"a".repeat(64),"idempotencyKey":"original-key","creditId":null
    },"inventoryError":"unavailable","credits":[]}),
    );
    session
        .prepare_reset_credit(0, NativeAccountLanguage::English)
        .unwrap();
    assert_eq!(session.page, MenuPage::Credits(0, 0));
    assert!(session.pending.is_none());
    assert!(session.redemption_keys.is_empty());
}
