use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn persisted_keys_keep_their_exact_owner_and_credit_binding() {
    let home = tempfile::tempdir().unwrap();
    let owner = "a".repeat(64);
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    journal
        .remember(&owner, "original-request", "original-credit")
        .unwrap();
    drop(journal);
    let journal = ManualResetCreditJournal::load(home.path()).unwrap();
    assert_eq!(
        journal.known_credit(&owner, "original-request", None),
        Ok(Some("original-credit"))
    );
    assert_eq!(
        journal.known_credit(&owner, "original-request", Some("original-credit")),
        Ok(Some("original-credit"))
    );
    assert_eq!(
        journal.known_credit(&owner, "new-request", Some("original-credit")),
        Ok(None)
    );
    assert_eq!(
        journal.known_credit(&"b".repeat(64), "original-request", None),
        Err(BINDING_CHANGED)
    );
    assert_eq!(
        journal.known_credit(&owner, "original-request", Some("different-credit")),
        Err(BINDING_CHANGED)
    );
}

#[test]
fn a_full_journal_refuses_new_spending_without_forgetting_known_keys() {
    let home = tempfile::tempdir().unwrap();
    let owner = "a".repeat(64);
    std::fs::write(
        home.path().join(JOURNAL_FILE_NAME),
        serde_json::to_vec(&json!({
            "version": 2,
            "operations": (0..MAX_OPERATIONS).map(|index| json!({
                "ownerDigest":owner,"idempotencyKey":format!("request-{index}"),"creditId":"credit",
                "phase":{"state":"terminal","outcome":"nothingToReset"}
            })).collect::<Vec<_>>()
        }))
        .unwrap(),
    )
    .unwrap();
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    assert_eq!(
        journal.remember(&owner, "overflow-request", "credit"),
        Err(JOURNAL_FULL)
    );
    let mut reopened = ManualResetCreditJournal::load(home.path()).unwrap();
    assert_eq!(
        reopened.known_credit(&owner, "request-0", None),
        Ok(Some("credit"))
    );
    assert_eq!(
        reopened.known_credit(&owner, "overflow-request", None),
        Ok(None)
    );
    assert_eq!(reopened.remember(&owner, "request-0", "credit"), Ok(()));
}

#[test]
fn malformed_or_ambiguous_journals_block_new_spending() {
    let record = json!({"ownerDigest": "a".repeat(64),
        "idempotencyKey": "request", "creditId": "credit"});
    for malformed in [
        json!({"version": 3, "operations": []}),
        json!({"version": 1, "operations": [], "extra": "unexpected"}),
        json!({"version": 1, "operations": [record.clone(), record]}),
        json!({"version": 2, "operations": [
            {"ownerDigest":"a".repeat(64),"idempotencyKey":"first","creditId":"credit",
                "phase":{"state":"pending"}},
            {"ownerDigest":"a".repeat(64),"idempotencyKey":"second","creditId":"another-credit",
                "phase":{"state":"pending"}}
        ]}),
        json!({"version": 2, "operations": [{"ownerDigest":"a".repeat(64),
            "idempotencyKey":"first","creditId":"credit","phase":{"state":"unknown"}}]}),
        json!({"version": 1, "operations": [{"ownerDigest": "a".repeat(64),
            "idempotencyKey": "request", "creditId": "credit", "secret": "never-accepted"}]}),
        json!({"version": 1, "operations": [{"ownerDigest": "invalid",
            "idempotencyKey": "request", "creditId": "credit"}]}),
        json!({"version": 1, "operations": [{"ownerDigest": "a".repeat(64),
            "idempotencyKey": "request", "creditId": "x".repeat(257)}]}),
        json!({"version": 1, "operations": (0..MAX_OPERATIONS + 1).map(|index| json!({
            "ownerDigest": "a".repeat(64), "idempotencyKey": format!("request-{index}"),
            "creditId": "credit"
        })).collect::<Vec<_>>()}),
    ] {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(JOURNAL_FILE_NAME),
            serde_json::to_vec(&malformed).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            ManualResetCreditJournal::load(home.path()),
            Err(JOURNAL_INVALID)
        ));
    }
}

#[test]
fn journal_reads_stop_at_the_byte_limit() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(JOURNAL_FILE_NAME),
        vec![b' '; MAX_JOURNAL_BYTES as usize + 1],
    )
    .unwrap();
    assert!(matches!(
        ManualResetCreditJournal::load(home.path()),
        Err(JOURNAL_INVALID)
    ));
}

#[test]
fn a_failed_persist_does_not_create_a_replayable_binding() {
    let home = tempfile::tempdir().unwrap();
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    std::fs::create_dir(home.path().join(JOURNAL_FILE_NAME)).unwrap();
    assert_eq!(
        journal.remember(&"a".repeat(64), "request", "credit"),
        Err(JOURNAL_UNAVAILABLE)
    );
    assert_eq!(
        journal.known_credit(&"a".repeat(64), "request", None),
        Ok(None)
    );
    assert!(matches!(
        ManualResetCreditJournal::load(home.path()),
        Err(JOURNAL_UNAVAILABLE)
    ));
}

#[test]
fn unconfirmed_operations_survive_restart_and_block_rebinding_until_terminal() {
    let home = tempfile::tempdir().unwrap();
    let owner = "a".repeat(64);
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    journal.remember(&owner, "original", "credit").unwrap();
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    assert_eq!(
        journal.pending_for_owner(&owner),
        Some(PendingAccountRateLimitResetCredit {
            owner_key: owner.clone(),
            idempotency_key: "original".into(),
            credit_id: Some("credit".into()),
        })
    );
    assert_eq!(
        journal.remember(&owner, "replacement", "another-credit"),
        Err(PENDING_OPERATION)
    );
    journal
        .complete(
            &owner,
            "original",
            ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed,
        )
        .unwrap();
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    assert_eq!(journal.pending_for_owner(&owner), None);
    assert_eq!(
        journal.known_credit(&owner, "original", /*expected_credit_id*/ None),
        Ok(Some("credit"))
    );
    assert_eq!(
        journal.terminal_outcome(&owner, "original"),
        Ok(Some(
            ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed
        ))
    );
    assert_eq!(
        journal.remember(&owner, "replacement", "another-credit"),
        Ok(())
    );
}

#[test]
fn latest_legacy_binding_requires_original_review_before_a_new_operation() {
    let home = tempfile::tempdir().unwrap();
    let owner = "a".repeat(64);
    std::fs::write(
        home.path().join(JOURNAL_FILE_NAME),
        serde_json::to_vec(&json!({
            "version": 1, "operations": [
                {"ownerDigest":owner,"idempotencyKey":"old","creditId":"old-credit"},
                {"ownerDigest":owner,"idempotencyKey":"latest","creditId":"latest-credit"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    assert_eq!(journal.terminal_outcome(&owner, "latest"), Ok(None));
    assert_eq!(
        journal.pending_for_owner(&owner).unwrap().idempotency_key,
        "latest"
    );
    assert_eq!(
        journal.remember(&owner, "new", "new-credit"),
        Err(PENDING_OPERATION)
    );
    journal.remember(&owner, "latest", "latest-credit").unwrap();
    journal
        .complete(
            &owner,
            "latest",
            ConsumeAccountRateLimitResetCreditOutcome::NothingToReset,
        )
        .unwrap();
    journal.remember(&owner, "new", "new-credit").unwrap();
    journal
        .complete(
            &owner,
            "new",
            ConsumeAccountRateLimitResetCreditOutcome::NothingToReset,
        )
        .unwrap();
    let journal = ManualResetCreditJournal::load(home.path()).unwrap();
    assert_eq!(journal.pending_for_owner(&owner), None);
    assert_eq!(
        journal.terminal_outcome(&owner, "latest"),
        Ok(Some(
            ConsumeAccountRateLimitResetCreditOutcome::NothingToReset
        ))
    );
    assert_eq!(
        journal.known_credit(&owner, "old", /*expected_credit_id*/ None),
        Ok(Some("old-credit"))
    );
}

#[test]
fn explicit_review_releases_new_spending_without_inventing_a_terminal_outcome() {
    let home = tempfile::tempdir().unwrap();
    let owner = "a".repeat(64);
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    journal.remember(&owner, "original", "credit").unwrap();
    let original = std::fs::read(home.path().join(JOURNAL_FILE_NAME)).unwrap();
    let view = journal.review_views().unwrap().remove(0);
    review_manual_reset(
        home.path(),
        &owner,
        "original",
        &view.digest,
        /*acknowledge_unconfirmed*/ true,
    )
    .unwrap();
    let mut reopened = ManualResetCreditJournal::load(home.path()).unwrap();
    assert_eq!(reopened.pending_for_owner(&owner), None);
    assert_eq!(reopened.terminal_outcome(&owner, "original"), Ok(None));
    assert_eq!(
        reopened.known_credit(&owner, "original", Some("credit")),
        Ok(Some("credit"))
    );
    assert_eq!(
        reopened.known_credit(&owner, "original", Some("another")),
        Err(BINDING_CHANGED)
    );
    assert!(!codex_login::automatic_reset_spending_blocked(home.path()).unwrap());
    let archived = std::fs::read_dir(home.path().join(".reset-credit-journal-archive"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::read(archived.join(JOURNAL_FILE_NAME)).unwrap(),
        original
    );
    reopened.remember(&owner, "replacement", "another").unwrap();
}

#[test]
fn latest_legacy_review_keeps_older_bindings_and_rejects_stale_confirmation() {
    let home = tempfile::tempdir().unwrap();
    let owner = "a".repeat(64);
    let path = home.path().join(JOURNAL_FILE_NAME);
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"version":1,"operations":[
            {"ownerDigest":owner,"idempotencyKey":"old","creditId":"old-credit"},
            {"ownerDigest":owner,"idempotencyKey":"latest","creditId":"latest-credit"}
        ]}))
        .unwrap(),
    )
    .unwrap();
    let view = ManualResetCreditJournal::load(home.path())
        .unwrap()
        .review_views()
        .unwrap()
        .remove(0);
    let original = std::fs::read(&path).unwrap();
    assert!(
        review_manual_reset(
            home.path(),
            &owner,
            "latest",
            &view.digest,
            /*acknowledge_unconfirmed*/ false
        )
        .is_err()
    );
    assert!(
        review_manual_reset(
            home.path(),
            &"b".repeat(64),
            "latest",
            &view.digest,
            /*acknowledge_unconfirmed*/ true
        )
        .is_err()
    );
    assert!(
        review_manual_reset(
            home.path(),
            &owner,
            "old",
            &view.digest,
            /*acknowledge_unconfirmed*/ true
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    review_manual_reset(
        home.path(),
        &owner,
        "latest",
        &view.digest,
        /*acknowledge_unconfirmed*/ true,
    )
    .unwrap();
    assert!(
        review_manual_reset(
            home.path(),
            &owner,
            "latest",
            &view.digest,
            /*acknowledge_unconfirmed*/ true
        )
        .is_err()
    );
    let reopened = ManualResetCreditJournal::load(home.path()).unwrap();
    assert!(reopened.review_views().unwrap().is_empty());
    assert_eq!(
        reopened.known_credit(&owner, "old", /*expected_credit_id*/ None),
        Ok(Some("old-credit"))
    );
    assert!(!codex_login::automatic_reset_spending_blocked(home.path()).unwrap());
}

#[test]
fn replay_of_a_reviewed_original_reestablishes_the_pending_barrier() {
    let home = tempfile::tempdir().unwrap();
    let owner = "a".repeat(64);
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    journal.remember(&owner, "original", "credit").unwrap();
    let view = journal.review_views().unwrap().remove(0);
    review_manual_reset(
        home.path(),
        &owner,
        "original",
        &view.digest,
        /*acknowledge_unconfirmed*/ true,
    )
    .unwrap();
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    journal.remember(&owner, "original", "credit").unwrap();
    assert_eq!(
        journal.pending_for_owner(&owner),
        Some(PendingAccountRateLimitResetCredit {
            owner_key: owner,
            idempotency_key: "original".into(),
            credit_id: Some("credit".into()),
        })
    );
    assert!(codex_login::automatic_reset_spending_blocked(home.path()).unwrap());
}

#[test]
fn manual_review_refuses_a_busy_lock_and_never_changes_auth_or_quota() {
    let home = tempfile::tempdir().unwrap();
    let owner = "a".repeat(64);
    let mut journal = ManualResetCreditJournal::load(home.path()).unwrap();
    journal.remember(&owner, "original", "credit").unwrap();
    let original = std::fs::read(home.path().join(JOURNAL_FILE_NAME)).unwrap();
    let view = journal.review_views().unwrap().remove(0);
    std::fs::write(home.path().join("auth.json"), b"synthetic-auth-evidence").unwrap();
    std::fs::write(
        home.path().join("account-runtime-state.json"),
        b"synthetic-quota-evidence",
    )
    .unwrap();
    let store = codex_login::AccountRuntimeStateStore::new(home.path().to_path_buf());
    let lock = store.try_lock_reset_credit().unwrap().unwrap();
    assert!(
        review_manual_reset(
            home.path(),
            &owner,
            "original",
            &view.digest,
            /*acknowledge_unconfirmed*/ true
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(home.path().join(JOURNAL_FILE_NAME)).unwrap(),
        original
    );
    assert!(!home.path().join(".reset-credit-journal-archive").exists());
    drop(lock);
    review_manual_reset(
        home.path(),
        &owner,
        "original",
        &view.digest,
        /*acknowledge_unconfirmed*/ true,
    )
    .unwrap();
    assert_eq!(
        (
            std::fs::read(home.path().join("auth.json")).unwrap(),
            std::fs::read(home.path().join("account-runtime-state.json")).unwrap()
        ),
        (
            b"synthetic-auth-evidence".to_vec(),
            b"synthetic-quota-evidence".to_vec()
        )
    );
}
