use super::*;
use pretty_assertions::assert_eq;

const NAME: &str = ".rate-limit-reset-credit-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json";
const LEGACY: &[u8] = br#"{"profileId":"synthetic-profile","resetKey":123,"quotaEpoch":null,"attemptedAt":1,"requestId":"private-request-id"}"#;
const PENDING: &[u8] = br#"{"version":1,"scope":{"profileId":"synthetic-profile","ownerKey":"private-owner-key","creditId":null,"resetKey":123,"quotaEpoch":null},"attemptedAt":1,"requestId":"private-request-id","phase":{"state":"pending"}}"#;

fn journal(home: &Path, bytes: &[u8]) -> anyhow::Result<std::path::PathBuf> {
    let path = home.join(NAME);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn archive_acknowledged(home: &Path, digest: &str) -> anyhow::Result<()> {
    archive(home, NAME, digest, /*acknowledge_unconfirmed*/ true)
}

#[test]
fn acknowledged_legacy_archive_preserves_original_bytes() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let path = journal(home.path(), LEGACY)?;
    let view = inspect(home.path())?.remove(0);
    assert!(view.archive_available);
    assert_eq!(
        (
            view.file_name.as_str(),
            view.profile_id.as_deref(),
            view.attempted_at,
            view.legacy
        ),
        (NAME, Some("synthetic-profile"), Some(1), true)
    );
    archive_acknowledged(home.path(), &view.digest)?;
    assert!(!path.exists());
    let directory = std::fs::read_dir(home.path().join(ARCHIVE_DIRECTORY))?
        .next()
        .expect("archive directory")?
        .path();
    assert_eq!(std::fs::read(directory.join(NAME))?, LEGACY);
    assert!(inspect(home.path())?.is_empty());
    Ok(())
}

#[test]
fn missing_acknowledgement_and_changed_digest_leave_source_untouched() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let path = journal(home.path(), LEGACY)?;
    let digest = inspect(home.path())?.remove(0).digest;
    let error = archive(
        home.path(),
        NAME,
        &digest,
        /*acknowledge_unconfirmed*/ false,
    )
    .expect_err("independent confirmation required");
    assert!(error.to_string().contains("independently checking"));
    assert_eq!(std::fs::read(&path)?, LEGACY);
    std::fs::write(&path, b"{interrupted")?;
    assert!(archive_acknowledged(home.path(), &digest).is_err());
    assert_eq!(std::fs::read(&path)?, b"{interrupted");
    assert!(!home.path().join(ARCHIVE_DIRECTORY).exists());
    Ok(())
}

#[test]
fn pending_owner_bound_journals_remain_available_for_automatic_reconciliation() -> anyhow::Result<()>
{
    let home = tempfile::tempdir()?;
    let path = journal(home.path(), PENDING)?;
    let views = inspect(home.path())?;
    let public = serde_json::to_string(&views)?;
    assert!(!public.contains("private-request-id"));
    assert!(!public.contains("private-owner-key"));
    assert!(!public.contains("requestId"));
    assert_eq!(
        (views.len(), views[0].legacy, views[0].archive_available),
        (1, false, false)
    );
    assert!(archive_acknowledged(home.path(), &views[0].digest).is_err());
    assert_eq!(std::fs::read(&path)?, PENDING);
    let mut confirmed: serde_json::Value = serde_json::from_slice(PENDING)?;
    confirmed["phase"] = serde_json::json!({"state":"confirmed", "outcome":"quotaRecovered"});
    std::fs::write(&path, serde_json::to_vec(&confirmed)?)?;
    assert!(inspect(home.path())?.is_empty());
    Ok(())
}

#[test]
fn damaged_bytes_can_be_archived_but_unknown_json_schema_cannot() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let path = journal(home.path(), b"{interrupted")?;
    let digest = inspect(home.path())?.remove(0).digest;
    archive_acknowledged(home.path(), &digest)?;
    std::fs::write(
        &path,
        b"{\"version\":2,\"requestId\":\"private-request-id\"}",
    )?;
    let digest = inspect(home.path())?.remove(0).digest;
    assert!(archive_acknowledged(home.path(), &digest).is_err());
    assert!(path.exists());
    std::fs::write(&path, b"{\"other\":true}")?;
    let digest = inspect(home.path())?.remove(0).digest;
    assert!(archive_acknowledged(home.path(), &digest).is_err());
    Ok(())
}

#[test]
fn busy_spending_lock_prevents_archive_without_waiting() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let path = journal(home.path(), LEGACY)?;
    let digest = inspect(home.path())?.remove(0).digest;
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    let _lock = store
        .try_lock_reset_credit()?
        .expect("available spending lock");
    assert!(archive_acknowledged(home.path(), &digest).is_err());
    assert_eq!(std::fs::read(&path)?, LEGACY);
    assert!(!home.path().join(ARCHIVE_DIRECTORY).exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn source_and_archive_directory_symlinks_are_rejected() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let target = outside.path().join("original");
    std::fs::write(&target, LEGACY)?;
    let source = home.path().join(NAME);
    std::os::unix::fs::symlink(&target, &source)?;
    assert!(inspect(home.path())?.is_empty());
    assert!(archive_acknowledged(home.path(), &"a".repeat(64)).is_err());
    assert_eq!(std::fs::read(&target)?, LEGACY);
    std::fs::remove_file(&source)?;
    journal(home.path(), LEGACY)?;
    let digest = inspect(home.path())?.remove(0).digest;
    std::os::unix::fs::symlink(outside.path(), home.path().join(ARCHIVE_DIRECTORY))?;
    assert!(archive_acknowledged(home.path(), &digest).is_err());
    assert_eq!(std::fs::read(&source)?, LEGACY);
    assert_eq!(std::fs::read_dir(outside.path())?.count(), 1);
    Ok(())
}

#[test]
fn inventory_bounds_reads_and_rejects_unbounded_candidate_counts() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let oversized = vec![b' '; 4097];
    let path = journal(home.path(), &oversized)?;
    let view = inspect(home.path())?.remove(0);
    assert!(view.digest.is_empty());
    assert!(view.message.contains("4096"));
    assert!(archive_acknowledged(home.path(), &view.digest).is_err());
    assert_eq!(std::fs::read(&path)?, oversized);
    std::fs::remove_file(path)?;
    std::fs::write(
        home.path().join(".rate-limit-reset-credit-invalid.json"),
        LEGACY,
    )?;
    assert!(inspect(home.path())?.is_empty());
    assert!(
        archive(
            home.path(),
            "../other",
            &"a".repeat(64),
            /*acknowledge_unconfirmed*/ true
        )
        .is_err()
    );
    for index in 0..129 {
        std::fs::write(
            home.path()
                .join(format!(".rate-limit-reset-credit-{index:040x}.json")),
            LEGACY,
        )?;
    }
    assert!(inspect(home.path()).is_err());
    Ok(())
}
