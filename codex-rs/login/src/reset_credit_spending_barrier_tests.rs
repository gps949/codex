use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn manual(owner: &str, request: &str, phase: serde_json::Value) -> serde_json::Value {
    let mut record =
        json!({"ownerDigest":owner,"idempotencyKey":request,"creditId":"synthetic-credit"});
    if !phase.is_null() {
        record["phase"] = phase;
    }
    record
}

#[test]
fn pending_manual_operation_blocks_automatic_until_a_definitive_receipt() -> io::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join(".manual-rate-limit-reset-credits.json");
    let owner = "a".repeat(64);
    std::fs::write(
        &path,
        serde_json::to_vec(
            &json!({"version":2,"operations":[manual(&owner,"first",json!({"state":"pending"}))]}),
        )?,
    )?;
    assert!(automatic_reset_spending_blocked(home.path())?);
    std::fs::write(
        &path,
        serde_json::to_vec(
            &json!({"version":2,"operations":[manual(&owner,"first",json!({"state":"terminal","outcome":"alreadyRedeemed"}))]}),
        )?,
    )?;
    assert!(!automatic_reset_spending_blocked(home.path())?);
    for bytes in [
        b"{interrupted".to_vec(),
        serde_json::to_vec(&json!({"version":9,"operations":[]}))?,
        vec![b' '; 4 * 1024 * 1024 + 1],
    ] {
        std::fs::write(&path, &bytes)?;
        assert!(automatic_reset_spending_blocked(home.path()).is_err());
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    Ok(())
}

#[test]
fn legacy_binding_is_unknown_until_the_same_owner_has_a_later_terminal_record() -> io::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join(".manual-rate-limit-reset-credits.json");
    let owner = "b".repeat(64);
    let first = manual(&owner, "legacy", serde_json::Value::Null);
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"version":1,"operations":[first]}))?,
    )?;
    assert!(automatic_reset_spending_blocked(home.path())?);
    let newer = manual(
        &owner,
        "newer",
        json!({"state":"terminal","outcome":"noCredit"}),
    );
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"version":2,"operations":[first,newer]}))?,
    )?;
    assert!(!automatic_reset_spending_blocked(home.path())?);
    let other = manual(&"c".repeat(64), "other", serde_json::Value::Null);
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"version":2,"operations":[first,newer,other]}))?,
    )?;
    assert!(automatic_reset_spending_blocked(home.path())?);
    Ok(())
}

#[test]
fn automatic_unknown_or_pending_record_blocks_new_manual_spending() -> io::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home
        .path()
        .join(format!(".rate-limit-reset-credit-{}.json", "a".repeat(40)));
    let record = json!({"version":1,"scope":{"profileId":"synthetic","ownerKey":"opaque-owner"},
        "attemptedAt":1,"requestId":"original-request","phase":{"state":"pending"}});
    std::fs::write(&path, serde_json::to_vec(&record)?)?;
    assert!(manual_reset_spending_blocked(home.path())?);
    let mut terminal = record;
    terminal["phase"] = json!({"state":"confirmed","outcome":"quotaRecovered"});
    std::fs::write(&path, serde_json::to_vec(&terminal)?)?;
    assert!(!manual_reset_spending_blocked(home.path())?);
    std::fs::write(&path, b"{interrupted")?;
    assert!(manual_reset_spending_blocked(home.path()).is_err());
    Ok(())
}

#[test]
fn reviewed_unknown_manual_operation_unblocks_new_automatic_spending() -> io::Result<()> {
    let home = tempfile::tempdir()?;
    let owner = "a".repeat(64);
    let original = manual(&owner, "old", serde_json::Value::Null);
    let reviewed = manual(&owner, "latest", json!({"state":"reviewed","reviewedAt":1}));
    std::fs::write(
        home.path().join(".manual-rate-limit-reset-credits.json"),
        serde_json::to_vec(&json!({"version":2,"operations":[original,reviewed]}))?,
    )?;
    assert!(!automatic_reset_spending_blocked(home.path())?);
    Ok(())
}
