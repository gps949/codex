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
            "version": 1,
            "operations": (0..MAX_OPERATIONS).map(|index| json!({
                "ownerDigest":owner,"idempotencyKey":format!("request-{index}"),"creditId":"credit"
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
        json!({"version": 2, "operations": []}),
        json!({"version": 1, "operations": [], "extra": "unexpected"}),
        json!({"version": 1, "operations": [record.clone(), record]}),
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
