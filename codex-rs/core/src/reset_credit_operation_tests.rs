use super::*;
use pretty_assertions::assert_eq;

fn scope(epoch: Option<i64>) -> ResetCreditScope {
    ResetCreditScope {
        profile_id: "synthetic-profile".into(),
        owner_key: "synthetic-owner".into(),
        credit_id: None,
        reset_key: Some(123),
        quota_epoch: epoch,
    }
}

#[test]
fn pending_operation_survives_restart_and_the_old_five_minute_boundary() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let operation =
        ResetCreditOperation::prepare(home.path(), scope(/*epoch*/ None), "original-request")?;
    let path = operation.path.clone();
    drop(operation);
    let mut record: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    record["attemptedAt"] = serde_json::json!(Utc::now().timestamp() - 3600);
    record
        .as_object_mut()
        .expect("legacy journal")
        .remove("attemptedAtPrecise");
    std::fs::write(&path, serde_json::to_vec(&record)?)?;
    let mut newer_scope = scope(Some(456));
    newer_scope.reset_key = Some(999);
    let resumed = ResetCreditOperation::prepare(home.path(), newer_scope, "replacement")?;
    assert_eq!(resumed.request_id(), "original-request");
    assert_eq!(resumed.record.phase, ResetCreditPhase::Pending);
    Ok(())
}

#[test]
fn corrupt_or_unowned_pending_operation_blocks_a_fresh_request() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let operation =
        ResetCreditOperation::prepare(home.path(), scope(/*epoch*/ None), "original-request")?;
    let path = operation.path.clone();
    drop(operation);
    let mut replacement_owner = scope(/*epoch*/ None);
    replacement_owner.owner_key = "replacement-owner".into();
    assert!(ResetCreditOperation::prepare(home.path(), replacement_owner, "replacement").is_err());
    for bytes in [b"{interrupted".to_vec(), vec![b' '; 4097],
        br#"{"profileId":"synthetic-profile","resetKey":123,"quotaEpoch":null,"attemptedAt":1,"requestId":"original-request"}"#.to_vec()] {
        std::fs::write(&path, &bytes)?;
        assert!(ResetCreditOperation::prepare(home.path(), scope(/*epoch*/ None), "replacement").is_err());
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    Ok(())
}

#[test]
fn unresolved_other_profile_blocks_a_second_credit_operation() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let pending =
        ResetCreditOperation::prepare(home.path(), scope(/*epoch*/ None), "original-request")?;
    let mut other = scope(/*epoch*/ None);
    other.profile_id = "other-profile".into();
    assert!(ResetCreditOperation::prepare(home.path(), other.clone(), "another-request").is_err());
    pending.complete(ResetCreditCompletion::NoCredit)?;
    assert_eq!(
        ResetCreditOperation::prepare(home.path(), other, "another-request")?.request_id(),
        "another-request"
    );
    Ok(())
}

#[test]
fn confirmed_recovery_requires_a_new_quota_epoch_before_another_operation() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    ResetCreditOperation::prepare(home.path(), scope(/*epoch*/ None), "original-request")?
        .complete(ResetCreditCompletion::QuotaRecovered)?;
    assert!(
        ResetCreditOperation::prepare(home.path(), scope(/*epoch*/ None), "replacement").is_err()
    );
    let next = ResetCreditOperation::prepare(home.path(), scope(Some(456)), "replacement")?;
    assert_eq!(next.request_id(), "replacement");
    Ok(())
}

#[test]
fn manual_unknown_history_blocks_new_automatic_spending_but_keeps_the_original_replay()
-> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let pending =
        ResetCreditOperation::prepare(home.path(), scope(/*epoch*/ None), "original-request")?;
    let path = pending.path;
    let original = std::fs::read(&path)?;
    std::fs::write(
        home.path().join(".manual-rate-limit-reset-credits.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "operations": [{"ownerDigest": "a".repeat(64), "creditId": "manual-credit", "idempotencyKey": "manual-request"}]
        }))?,
    )?;
    let resumed =
        ResetCreditOperation::prepare(home.path(), scope(Some(456)), "replacement-request")?;
    assert_eq!(
        (resumed.request_id(), std::fs::read(&path)?),
        ("original-request", original)
    );
    resumed.complete(ResetCreditCompletion::NoCredit)?;
    let completed = std::fs::read(&path)?;
    assert!(ResetCreditOperation::prepare(home.path(), scope(Some(456)), "new-request").is_err());
    assert_eq!(std::fs::read(path)?, completed);
    Ok(())
}
