use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn snapshot(status: PrimaryRemoteStatus, at: i64) -> PrimaryRuntimeSnapshot {
    PrimaryRuntimeSnapshot {
        source_revision: 4,
        email: Some("fixture@example.com".into()),
        profile_id: Some("fixture-seat".into()),
        remote_status: status,
        observed_at: at,
    }
}

#[test]
fn runtime_snapshot_roundtrip_preserves_only_current_revision_and_expires() -> io::Result<()> {
    let home = TempDir::new()?;
    let writer = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let now = 1800000000;
    let current = snapshot(PrimaryRemoteStatus::Connected, now);
    writer.publish(&current)?;
    assert_eq!(
        read_current_at(home.path(), /*revision*/ 4, now),
        Some(current.clone())
    );
    assert_eq!(read_current_at(home.path(), /*revision*/ 3, now), None);
    assert_eq!(read_current_at(home.path(), /*revision*/ 4, now - 1), None);
    assert_eq!(read_current_at(home.path(), /*revision*/ 4, now + 10), None);
    let saved: SavedSnapshot = serde_json::from_slice(&fs::read(&writer.path)?).unwrap();
    assert_eq!(
        (saved.schema_version, saved.pid, saved.snapshot),
        (1, std::process::id(), current)
    );
    let serialized: serde_json::Value = serde_json::from_slice(&fs::read(&writer.path)?).unwrap();
    assert_eq!(serialized["snapshot"]["remoteStatus"], "connected");
    assert!(!serialized.to_string().contains("token"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&writer.path)?.permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&writer.lease_path)?.permissions().mode() & 0o777,
            0o600
        );
    }
    let path = writer.path.clone();
    let lease = writer.lease_path.clone();
    drop(writer);
    assert!(!path.exists());
    assert!(!lease.exists());
    Ok(())
}

#[test]
fn concurrent_writer_conflicts_are_unknown_and_drop_removes_only_its_owner() -> io::Result<()> {
    let home = TempDir::new()?;
    let first = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let second = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let now = 1800000000;
    let current = snapshot(PrimaryRemoteStatus::Connected, now);
    first.publish(&current)?;
    let mut disabled = current.clone();
    disabled.remote_status = PrimaryRemoteStatus::Disabled;
    second.publish(&disabled)?;
    assert_eq!(read_current_at(home.path(), /*revision*/ 4, now), None);
    disabled.remote_status = PrimaryRemoteStatus::Connected;
    disabled.email = Some("other-seat@example.com".into());
    second.publish(&disabled)?;
    assert_eq!(read_current_at(home.path(), /*revision*/ 4, now), None);
    second.publish(&current)?;
    assert_eq!(read_current_at(home.path(), /*revision*/ 4, now), None);
    let second_path = second.path.clone();
    drop(first);
    assert!(second_path.exists());
    assert_eq!(
        read_current_at(home.path(), /*revision*/ 4, now),
        Some(current)
    );
    Ok(())
}

#[test]
fn oversized_corrupt_and_mismatched_session_files_cannot_claim_connected() -> io::Result<()> {
    let home = TempDir::new()?;
    let writer = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let now = 1800000000;
    let current = snapshot(PrimaryRemoteStatus::Connected, now);
    writer.publish(&current)?;
    let saved = fs::read(&writer.path)?;
    fs::write(&writer.path, vec![b' '; MAX_BYTES as usize + 1])?;
    assert_eq!(read_current_at(home.path(), /*revision*/ 4, now), None);
    fs::write(&writer.path, b"corrupt status")?;
    assert_eq!(read_current_at(home.path(), /*revision*/ 4, now), None);
    let mut mismatched: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    mismatched["sessionId"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
    fs::write(&writer.path, serde_json::to_vec(&mismatched).unwrap())?;
    assert_eq!(read_current_at(home.path(), /*revision*/ 4, now), None);
    writer.publish(&current)?;
    assert_eq!(
        read_current_at(home.path(), /*revision*/ 4, now),
        Some(current)
    );
    Ok(())
}

#[test]
fn runtime_slot_limit_is_bounded_without_deleting_other_process_files() -> io::Result<()> {
    let home = TempDir::new()?;
    let first = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let now = 1800000000;
    let current = snapshot(PrimaryRemoteStatus::Connected, now);
    first.publish(&current)?;
    let mut writers = vec![first];
    for _ in 1..MAX_FILES {
        writers.push(PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?);
    }
    writers[0].clear()?;
    assert!(PrimaryRuntimeStatusStore::new(home.path().to_path_buf()).is_err());
    writers[0].publish(&current)?;
    assert!(PrimaryRuntimeStatusStore::new(home.path().to_path_buf()).is_err());
    assert_eq!(
        snapshot_paths(&home.path().join(DIRECTORY))?.unwrap().len(),
        MAX_FILES
    );
    let own_path = writers[0].path.clone();
    drop(writers.pop());
    let replacement = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    assert!(own_path.exists());
    drop(replacement);
    assert_eq!(
        read_current_at(home.path(), /*revision*/ 4, now),
        Some(current)
    );
    Ok(())
}

#[test]
fn invalid_metadata_is_rejected_without_overwriting_previous_observation() -> io::Result<()> {
    let home = TempDir::new()?;
    let writer = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let now = 1800000000;
    let current = snapshot(PrimaryRemoteStatus::Disabled, now);
    writer.publish(&current)?;
    let mut invalid = current.clone();
    invalid.email = Some("fixture@example.com\nother-user".into());
    assert!(writer.publish(&invalid).is_err());
    assert_eq!(
        read_current_at(home.path(), /*revision*/ 4, now),
        Some(current)
    );
    Ok(())
}

#[test]
fn primary_runtime_status_clear_retains_an_independent_writer_for_republication() {
    let home = tempfile::TempDir::new().unwrap();
    let writer = PrimaryRuntimeStatusStore::new(home.path().to_path_buf()).unwrap();
    let snapshot = PrimaryRuntimeSnapshot {
        source_revision: 1,
        email: Some("fixture@example.com".into()),
        profile_id: None,
        remote_status: PrimaryRemoteStatus::Disabled,
        observed_at: chrono::Utc::now().timestamp(),
    };
    writer.publish(&snapshot).unwrap();
    writer.clear().unwrap();
    assert_eq!(
        PrimaryRuntimeStatusStore::read_current(home.path(), 1),
        None
    );
    writer.publish(&snapshot).unwrap();
    assert_eq!(
        PrimaryRuntimeStatusStore::read_current(home.path(), 1),
        Some(snapshot)
    );
}

fn abandoned_writer(
    directory: &Path,
    snapshot: &PrimaryRuntimeSnapshot,
) -> io::Result<(PathBuf, PathBuf)> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let json_path = directory.join(format!("{session_id}.json"));
    let lease_path = directory.join(format!("{session_id}.lease"));
    // An unlocked lease represents a crashed owner: reclaiming it must depend on the
    // kernel lease rather than the snapshot's recent timestamp or this process's PID.
    fs::write(&lease_path, b"")?;
    fs::write(
        &json_path,
        serde_json::to_vec(&SavedSnapshot {
            schema_version: 1,
            pid: std::process::id(),
            session_id,
            snapshot: snapshot.clone(),
        })
        .unwrap(),
    )?;
    Ok((json_path, lease_path))
}

#[test]
fn runtime_writer_registration_reclaims_crashed_leases_and_retains_live_observations()
-> io::Result<()> {
    let home = TempDir::new()?;
    let live = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let now = 1800000000;
    let current = snapshot(PrimaryRemoteStatus::Connected, now);
    live.publish(&current)?;
    let mut stale = current.clone();
    stale.remote_status = PrimaryRemoteStatus::Disabled;
    let abandoned = (1..MAX_FILES)
        .map(|_| abandoned_writer(&live.directory, &stale))
        .collect::<io::Result<Vec<_>>>()?;
    let replacement = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    assert_eq!(snapshot_paths(&live.directory)?.unwrap().len(), 2);
    for (json, lease) in abandoned {
        assert_eq!((json.exists(), lease.exists()), (false, false));
    }
    assert_eq!(
        read_current_at(home.path(), /*revision*/ 4, now),
        Some(current)
    );
    assert!(live.path.exists() && live.lease_path.exists());
    drop(replacement);
    Ok(())
}

#[test]
fn runtime_writer_registration_reclaims_all_crashed_slots_after_restart() -> io::Result<()> {
    let home = TempDir::new()?;
    let writer = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let directory = writer.directory.clone();
    drop(writer);
    let current = snapshot(PrimaryRemoteStatus::Disabled, 1800000000);
    let abandoned = (0..MAX_FILES)
        .map(|_| abandoned_writer(&directory, &current))
        .collect::<io::Result<Vec<_>>>()?;
    let replacement = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    assert_eq!(snapshot_paths(&directory)?.unwrap().len(), 1);
    for (json, lease) in abandoned {
        assert_eq!((json.exists(), lease.exists()), (false, false));
    }
    assert!(replacement.path.exists() && replacement.lease_path.exists());
    Ok(())
}

#[test]
fn runtime_writer_registration_reclaims_only_after_the_kernel_lease_is_released() -> io::Result<()>
{
    let home = TempDir::new()?;
    let live = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let now = 1800000000;
    let current = snapshot(PrimaryRemoteStatus::Connected, now);
    live.publish(&current)?;
    let abandoned = snapshot(PrimaryRemoteStatus::Disabled, now);
    let (json_path, lease_path) = abandoned_writer(&live.directory, &abandoned)?;
    let owner = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lease_path)?;
    owner.lock()?;
    let replacement = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    assert_eq!((json_path.exists(), lease_path.exists()), (true, true));
    drop(owner);
    let restarted = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    assert_eq!((json_path.exists(), lease_path.exists()), (false, false));
    assert_eq!(
        read_current_at(home.path(), /*revision*/ 4, now),
        Some(current)
    );
    assert!(live.path.exists() && live.lease_path.exists());
    drop((replacement, restarted));
    Ok(())
}

#[test]
fn runtime_writer_registration_recovers_a_crash_before_snapshot_reservation() -> io::Result<()> {
    let home = TempDir::new()?;
    let live = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let orphan = live
        .directory
        .join(format!("{}.lease", uuid::Uuid::new_v4()));
    fs::write(&orphan, b"")?;
    let replacement = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    assert!(!orphan.exists());
    assert!(live.lease_path.exists() && replacement.lease_path.exists());
    Ok(())
}

#[test]
fn runtime_writer_registration_reclaims_only_the_abandoned_sessions_temporary_files()
-> io::Result<()> {
    let home = TempDir::new()?;
    let live = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let (json_path, lease_path) = abandoned_writer(
        &live.directory,
        &snapshot(PrimaryRemoteStatus::Disabled, /*at*/ 1800000000),
    )?;
    let session_id = json_path.file_stem().unwrap().to_str().unwrap();
    let temporary = live
        .directory
        .join(format!("{session_id}.tmp-{}", uuid::Uuid::new_v4()));
    let unknown = live.directory.join(format!("{session_id}.tmp-unknown"));
    fs::write(&temporary, b"incomplete heartbeat")?;
    fs::write(&unknown, b"unrecognized temporary file")?;
    let replacement = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    assert_eq!(
        (json_path.exists(), lease_path.exists(), temporary.exists()),
        (false, false, false)
    );
    assert_eq!(fs::read(&unknown)?, b"unrecognized temporary file");
    assert!(live.path.exists() && replacement.path.exists());
    Ok(())
}

#[test]
fn runtime_writer_registration_cannot_exceed_the_entry_limit_with_unknown_files() -> io::Result<()>
{
    let home = TempDir::new()?;
    let live = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    for index in 3..MAX_ENTRIES {
        fs::write(live.directory.join(format!("unknown-{index}")), b"retain")?;
    }
    assert!(PrimaryRuntimeStatusStore::new(home.path().to_path_buf()).is_err());
    assert_eq!(fs::read_dir(&live.directory)?.count(), MAX_ENTRIES);
    assert!(live.path.exists() && live.lease_path.exists());
    Ok(())
}

#[test]
fn runtime_writer_registration_leaves_unleased_or_unknown_files_untouched() -> io::Result<()> {
    let home = TempDir::new()?;
    let live = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let unleased = live
        .directory
        .join(format!("{}.json", uuid::Uuid::new_v4()));
    let unknown_json = live.directory.join("unknown.json");
    let unknown_lease = live.directory.join("unknown.lease");
    fs::write(&unleased, b"older unleased writer")?;
    fs::write(&unknown_json, b"unknown json")?;
    fs::write(&unknown_lease, b"unknown lease")?;
    let replacement = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    assert_eq!(
        (
            fs::read(&unleased)?,
            fs::read(&unknown_json)?,
            fs::read(&unknown_lease)?
        ),
        (
            b"older unleased writer".to_vec(),
            b"unknown json".to_vec(),
            b"unknown lease".to_vec()
        )
    );
    drop(replacement);
    Ok(())
}

#[test]
fn runtime_writer_registration_rejects_nonfile_leases_and_observations() -> io::Result<()> {
    let home = TempDir::new()?;
    let live = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let lease_id = uuid::Uuid::new_v4().to_string();
    let lease_directory = live.directory.join(format!("{lease_id}.lease"));
    let lease_json = live.directory.join(format!("{lease_id}.json"));
    fs::create_dir(&lease_directory)?;
    fs::write(&lease_json, b"retain the associated observation")?;
    let observation_id = uuid::Uuid::new_v4().to_string();
    let observation_directory = live.directory.join(format!("{observation_id}.json"));
    let observation_lease = live.directory.join(format!("{observation_id}.lease"));
    fs::create_dir(&observation_directory)?;
    fs::write(&observation_lease, b"")?;
    let replacement = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    assert!(lease_directory.is_dir() && observation_directory.is_dir());
    assert_eq!(fs::read(&lease_json)?, b"retain the associated observation");
    assert!(observation_lease.exists());
    drop(replacement);
    Ok(())
}

#[cfg(unix)]
#[test]
fn runtime_writer_registration_rejects_symlinked_leases_and_observations() -> io::Result<()> {
    use std::os::unix::fs::symlink;
    let home = TempDir::new()?;
    let live = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    let outside = home.path().join("unrelated-data");
    fs::write(&outside, b"must retain unrelated data")?;
    let lease_id = uuid::Uuid::new_v4().to_string();
    let lease_json = live.directory.join(format!("{lease_id}.json"));
    let symlink_lease = live.directory.join(format!("{lease_id}.lease"));
    fs::write(&lease_json, b"must retain a symlink-associated observation")?;
    symlink(&outside, &symlink_lease)?;
    let observation_id = uuid::Uuid::new_v4().to_string();
    let symlink_json = live.directory.join(format!("{observation_id}.json"));
    let observation_lease = live.directory.join(format!("{observation_id}.lease"));
    fs::write(&observation_lease, b"")?;
    symlink(&outside, &symlink_json)?;
    let replacement = PrimaryRuntimeStatusStore::new(home.path().to_path_buf())?;
    assert_eq!(fs::read(&outside)?, b"must retain unrelated data");
    assert!(
        fs::symlink_metadata(&symlink_lease)?
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(&symlink_json)?
            .file_type()
            .is_symlink()
    );
    assert!(lease_json.exists() && observation_lease.exists());
    drop(replacement);
    Ok(())
}
