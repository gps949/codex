//! Bounded, nonsecret local heartbeats for independently running host processes.

use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

const DIRECTORY: &str = ".primary-login-runtime";
const MAX_FILES: usize = 64;
const MAX_ENTRIES: usize = MAX_FILES * 3 + 1;
const MAX_BYTES: u64 = 16 * 1024;
const LIFETIME_SECONDS: i64 = 10;

/// Actual Remote service state reported by the process owning the connection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PrimaryRemoteStatus {
    Disabled,
    Connecting,
    Connected,
    Errored,
    RequirementsDisabled,
    AuthenticationDenied,
}

impl PrimaryRemoteStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Errored => "errored",
            Self::RequirementsDisabled => "requirementsDisabled",
            Self::AuthenticationDenied => "authenticationDenied",
        }
    }
}

/// A recent observation from a live host, with no bearer token or backend owner identifier.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrimaryRuntimeSnapshot {
    pub source_revision: u64,
    pub email: Option<String>,
    pub profile_id: Option<String>,
    pub remote_status: PrimaryRemoteStatus,
    pub observed_at: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SavedSnapshot {
    schema_version: u32,
    pid: u32,
    session_id: String,
    snapshot: PrimaryRuntimeSnapshot,
}

/// Owns one UUID-named heartbeat file and its exclusively locked lifetime lease.
/// Concurrent hosts never overwrite one another or append a heartbeat history.
pub struct PrimaryRuntimeStatusStore {
    directory: PathBuf,
    path: PathBuf,
    lease_path: PathBuf,
    lease: Option<fs::File>,
    session_id: String,
    write_lock: Mutex<()>,
}

impl PrimaryRuntimeStatusStore {
    pub fn new(home: PathBuf) -> io::Result<Self> {
        let directory = home.join(DIRECTORY);
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory)?;
        let directory_lock = private_options().open(directory.join(".writers.lock"))?;
        directory_lock.lock()?;
        let entries = runtime_paths(&directory)?.ok_or_else(|| {
            io::Error::other("host runtime status directory exceeds its bounded file limit")
        })?;
        reclaim_abandoned(&directory, &entries)?;
        let entries = runtime_paths(&directory)?.ok_or_else(|| {
            io::Error::other("host runtime status directory exceeds its bounded file limit")
        })?;
        let snapshot_count = entries
            .iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .count();
        if snapshot_count >= MAX_FILES || entries.len() + 2 > MAX_ENTRIES {
            return Err(io::Error::other("host runtime status writer limit reached"));
        }
        let session_id = uuid::Uuid::new_v4().to_string();
        let path = directory.join(format!("{session_id}.json"));
        let lease_path = directory.join(format!("{session_id}.lease"));
        let lease = private_options().create_new(true).open(&lease_path)?;
        let reservation = (|| {
            lease.lock()?;
            private_options().create_new(true).open(&path)?;
            Ok(())
        })();
        if let Err(error) = reservation {
            drop(lease);
            let _ = fs::remove_file(&lease_path);
            return Err(error);
        }
        Ok(Self {
            directory,
            path,
            lease_path,
            lease: Some(lease),
            session_id,
            write_lock: Mutex::new(()),
        })
    }

    /// Clears this process's observation while retaining its independently owned writer.
    pub fn clear(&self) -> io::Result<()> {
        let _write = self
            .write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Keep this writer's allocated slot. Removing it would let a new writer take its
        // capacity before a later heartbeat recreates the file and exceeds the bounded scan.
        let mut file = private_options().truncate(true).open(&self.path)?;
        file.write_all(b"null")
    }

    /// Atomically publishes a caller-observed identity and connection state.
    /// The caller must derive these fields from its live facade rather than stored credentials.
    pub fn publish(&self, snapshot: &PrimaryRuntimeSnapshot) -> io::Result<()> {
        validate_snapshot(snapshot)?;
        let _write = self
            .write_lock
            .lock()
            .map_err(|_| io::Error::other("host status writer lock is poisoned"))?;
        let saved = SavedSnapshot {
            schema_version: 1,
            pid: std::process::id(),
            session_id: self.session_id.clone(),
            snapshot: snapshot.clone(),
        };
        let bytes = serde_json::to_vec(&saved).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "host runtime status exceeds its size limit",
            ));
        }
        let temporary = self
            .directory
            .join(format!("{}.tmp-{}", self.session_id, self.session_id));
        // This writer owns one deterministic staging path under its lifetime lease. A failed
        // prior publication cannot accumulate additional temporary files on a later heartbeat.
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let result = (|| {
            let mut file = private_options().create_new(true).open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    /// Returns only a fresh, matching-revision observation. Conflicting live hosts are ambiguous.
    pub fn read_current(home: &Path, revision: u64) -> Option<PrimaryRuntimeSnapshot> {
        read_current_at(home, revision, chrono::Utc::now().timestamp())
    }
}

impl Drop for PrimaryRuntimeStatusStore {
    fn drop(&mut self) {
        let Ok(directory_lock) = private_options().open(self.directory.join(".writers.lock"))
        else {
            return;
        };
        if directory_lock.lock().is_err() {
            return;
        }
        // Registration only tries leases without waiting, so taking the directory lock
        // while owning this lease cannot deadlock. Close before unlinking on Windows.
        drop(self.lease.take());
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            // Retain the unlocked lease so a later registration can retry cleanup.
            return;
        }
        let temporary = self
            .directory
            .join(format!("{}.tmp-{}", self.session_id, self.session_id));
        if let Err(error) = fs::remove_file(temporary)
            && error.kind() != io::ErrorKind::NotFound
        {
            return;
        }
        let _ = fs::remove_file(&self.lease_path);
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn runtime_paths(directory: &Path) -> io::Result<Option<Vec<PathBuf>>> {
    let mut paths = Vec::new();
    for (index, entry) in fs::read_dir(directory)?.enumerate() {
        if index >= MAX_ENTRIES {
            return Ok(None);
        }
        paths.push(entry?.path());
    }
    Ok(Some(paths))
}

fn snapshot_paths(directory: &Path) -> io::Result<Option<Vec<PathBuf>>> {
    let Some(entries) = runtime_paths(directory)? else {
        return Ok(None);
    };
    let paths: Vec<_> = entries
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    Ok((paths.len() <= MAX_FILES).then_some(paths))
}

fn reclaim_abandoned(directory: &Path, entries: &[PathBuf]) -> io::Result<()> {
    for lease_path in entries {
        if lease_path
            .extension()
            .is_none_or(|extension| extension != "lease")
        {
            continue;
        }
        let Some(session_id) = lease_path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if uuid::Uuid::parse_str(session_id).is_err()
            || !fs::symlink_metadata(lease_path).is_ok_and(|metadata| metadata.is_file())
        {
            continue;
        }
        let path = directory.join(format!("{session_id}.json"));
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) | Err(_) => continue,
        }
        // Never create a missing lease or infer abandonment from a timestamp or PID.
        let Ok(lease) = OpenOptions::new().read(true).write(true).open(lease_path) else {
            continue;
        };
        if lease.try_lock().is_err() {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let temporary_prefix = format!("{session_id}.tmp-");
        for temporary in entries {
            if temporary
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix(&temporary_prefix))
                .is_some_and(|suffix| uuid::Uuid::parse_str(suffix).is_ok())
                && fs::symlink_metadata(temporary).is_ok_and(|metadata| metadata.is_file())
            {
                fs::remove_file(temporary)?;
            }
        }
        // The directory lock keeps registration serialized while the kernel lease is
        // released. Every future writer allocates a new UUID rather than reusing it.
        drop(lease);
        fs::remove_file(lease_path)?;
    }
    Ok(())
}

fn read_current_at(home: &Path, revision: u64, now: i64) -> Option<PrimaryRuntimeSnapshot> {
    let paths = snapshot_paths(&home.join(DIRECTORY)).ok().flatten()?;
    let mut selected: Option<PrimaryRuntimeSnapshot> = None;
    for path in paths {
        let saved = (|| {
            let metadata = fs::symlink_metadata(&path).ok()?;
            if !metadata.is_file() || metadata.len() > MAX_BYTES {
                return None;
            }
            let mut bytes = Vec::new();
            fs::File::open(&path)
                .ok()?
                .take(MAX_BYTES + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() as u64 > MAX_BYTES {
                return None;
            }
            let saved: SavedSnapshot = serde_json::from_slice(&bytes).ok()?;
            if saved.schema_version != 1
                || saved.pid == 0
                || uuid::Uuid::parse_str(&saved.session_id).is_err()
                || path.file_stem()?.to_str()? != saved.session_id
                || validate_snapshot(&saved.snapshot).is_err()
            {
                return None;
            }
            let age = now.checked_sub(saved.snapshot.observed_at)?;
            (saved.snapshot.source_revision == revision && (0..LIFETIME_SECONDS).contains(&age))
                .then_some(saved.snapshot)
        })();
        let Some(snapshot) = saved else {
            continue;
        };
        // A displayed email does not identify a workspace. Multiple live hosts can use the
        // same email with different external owners, so their observations are ambiguous.
        if selected.is_some() {
            return None;
        }
        selected = Some(snapshot);
    }
    selected
}

fn validate_snapshot(snapshot: &PrimaryRuntimeSnapshot) -> io::Result<()> {
    if snapshot.email.as_ref().is_some_and(|email| {
        email.is_empty()
            || email.len() > 320
            || email.trim() != email
            || email.chars().any(char::is_control)
    }) || snapshot
        .profile_id
        .as_ref()
        .is_some_and(|id| id.len() > 256 || crate::AccountProfileId::new(id).is_err())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "host runtime identity metadata is invalid",
        ));
    }
    Ok(())
}
