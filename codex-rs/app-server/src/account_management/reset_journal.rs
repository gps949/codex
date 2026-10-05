//! Explicit recovery for bounded legacy reset journals; never spends a reset credit.

use codex_login::AccountRuntimeStateStore;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::fs::File;
use std::io::Read;
use std::path::Path;

const MAX_BYTES: u64 = 4096;
const ARCHIVE_DIRECTORY: &str = ".reset-credit-journal-archive";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetJournalView {
    pub file_name: String,
    pub digest: String,
    pub profile_id: Option<String>,
    pub attempted_at: Option<i64>,
    pub legacy: bool,
    pub archive_available: bool,
    pub message: String,
}

enum JournalKind {
    Legacy(String, i64),
    Damaged,
    Pending(String, i64),
    Confirmed,
    Unknown,
}

fn valid_name(name: &str) -> bool {
    name.strip_prefix(".rate-limit-reset-credit-")
        .and_then(|name| name.strip_suffix(".json"))
        .is_some_and(|key| valid_hex(key, /*length*/ 40))
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(metadata.is_file(), "Reset journal must be a regular file");
    let file = File::open(path)?;
    let opened = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            metadata.dev() == opened.dev() && metadata.ino() == opened.ino(),
            "Reset journal changed while opening"
        );
    }
    anyhow::ensure!(
        opened.is_file() && opened.len() <= MAX_BYTES,
        "Reset journal exceeds the 4096-byte safety limit"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES).read_to_end(&mut bytes)?;
    let current = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        current.is_file() && current.len() == bytes.len() as u64,
        "Reset journal changed while reading"
    );
    Ok(bytes)
}

fn fields(record: &Value, allowed: &str) -> bool {
    record.as_object().is_some_and(|object| {
        object
            .keys()
            .all(|key| allowed.split_whitespace().any(|field| field == key))
    })
}

fn text(value: &Value, limit: usize) -> bool {
    value
        .as_str()
        .is_some_and(|value| !value.is_empty() && value.len() <= limit)
}

fn integer(value: &Value) -> bool {
    value.is_null() || value.as_i64().is_some()
}

fn known_v1(record: &Value) -> bool {
    let scope = &record["scope"];
    let phase = &record["phase"];
    let recovery = &record["reconciledRecovery"];
    let previous = &record["previousConfirmed"];
    let date = |value: &Value| {
        value
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
    };
    let valid_phase = match phase["state"].as_str() {
        Some("pending") => fields(phase, "state"),
        Some("confirmed") => {
            fields(phase, "state outcome")
                && matches!(
                    phase["outcome"].as_str(),
                    Some("quotaRecovered" | "noCredit")
                )
        }
        Some(_) | None => false,
    };
    record["version"].as_u64() == Some(1)
        && record["attemptedAt"].as_i64().is_some()
        && fields(
            record,
            "version scope attemptedAt attemptedAtPrecise requestId phase reconciledRecovery previousConfirmed",
        )
        && fields(scope, "profileId ownerKey creditId resetKey quotaEpoch")
        && text(&record["requestId"], /*limit*/ 128)
        && text(&scope["profileId"], /*limit*/ 256)
        && text(&scope["ownerKey"], /*limit*/ 256)
        && (scope["creditId"].is_null() || scope["creditId"].is_string())
        && integer(&scope["resetKey"])
        && integer(&scope["quotaEpoch"])
        && (record["attemptedAtPrecise"].is_null()
            || date(&record["attemptedAtPrecise"]).map(|date| date.timestamp())
                == record["attemptedAt"].as_i64())
        && valid_phase
        && (recovery.is_null()
            || fields(recovery, "quotaEpoch observedAt")
                && recovery["quotaEpoch"].as_i64().is_some()
                && date(&recovery["observedAt"]).is_some())
        && (previous.is_null()
            || previous["previousConfirmed"].is_null()
                && previous["phase"]["state"] == "confirmed"
                && previous["scope"]["profileId"] == scope["profileId"]
                && previous["scope"]["ownerKey"] == scope["ownerKey"]
                && previous["scope"]["creditId"] == scope["creditId"]
                && known_v1(previous))
}

fn classify(bytes: &[u8]) -> JournalKind {
    let Ok(record) = serde_json::from_slice::<Value>(bytes) else {
        return JournalKind::Damaged;
    };
    let profile = &record["profileId"];
    if record.as_object().is_some_and(|object| object.len() == 5)
        && fields(
            &record,
            "profileId resetKey quotaEpoch attemptedAt requestId",
        )
        && text(profile, /*limit*/ 256)
        && text(&record["requestId"], /*limit*/ 128)
        && integer(&record["resetKey"])
        && integer(&record["quotaEpoch"])
        && let Some(at) = record["attemptedAt"].as_i64()
        && let Some(profile) = profile.as_str()
    {
        return JournalKind::Legacy(profile.into(), at);
    }
    if known_v1(&record) {
        return if record["phase"]["state"] == "pending" {
            let (Some(id), Some(at)) = (
                record["scope"]["profileId"].as_str(),
                record["attemptedAt"].as_i64(),
            ) else {
                return JournalKind::Unknown;
            };
            JournalKind::Pending(id.into(), at)
        } else {
            JournalKind::Confirmed
        };
    }
    JournalKind::Unknown
}

pub(super) fn inspect(codex_home: &Path) -> anyhow::Result<Vec<ResetJournalView>> {
    let entries = match std::fs::read_dir(codex_home) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut views = Vec::new();
    let mut candidates = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| valid_name(name)) else {
            continue;
        };
        candidates += 1;
        anyhow::ensure!(
            candidates <= 128,
            "Too many reset journals to inspect safely; the limit is 128 files"
        );
        if !entry.file_type()?.is_file() {
            continue;
        }
        let bytes = read_bytes(&entry.path());
        let digest = bytes
            .as_ref()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
            .unwrap_or_default();
        let kind = bytes.as_deref().map(classify);
        let legacy = matches!(&kind, Ok(JournalKind::Legacy(..)));
        let (profile_id, attempted_at) = match &kind {
            Ok(JournalKind::Legacy(id, at) | JournalKind::Pending(id, at)) => {
                (Some(id.clone()), Some(*at))
            }
            Ok(JournalKind::Damaged | JournalKind::Confirmed | JournalKind::Unknown) | Err(_) => {
                (None, None)
            }
        };
        let message = match kind {
            Ok(JournalKind::Legacy(..)) => "Legacy reset attempt needs review.",
            Ok(JournalKind::Damaged) => "Damaged reset record needs review.",
            Ok(JournalKind::Pending(..)) => "Owner-bound reset is awaiting confirmed recovery.",
            Ok(JournalKind::Confirmed) => continue,
            Ok(JournalKind::Unknown) => "Unknown reset record needs manual review.",
            Err(_) => "Reset record is unreadable or exceeds 4096 bytes.",
        };
        views.push(ResetJournalView {
            file_name: name.to_owned(),
            digest,
            profile_id,
            attempted_at,
            legacy,
            archive_available: legacy
                || matches!(
                    bytes.as_deref().map(classify),
                    Ok(JournalKind::Damaged | JournalKind::Pending(..))
                ),
            message: message.to_owned(),
        });
    }
    views.sort_by(|left, right| left.file_name.cmp(&right.file_name));
    Ok(views)
}

pub(super) fn archive(
    codex_home: &Path,
    file_name: &str,
    expected_digest: &str,
    acknowledge_unconfirmed: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        acknowledge_unconfirmed,
        "Archiving requires independently checking quota and reset history before abandoning an unresolved operation"
    );
    anyhow::ensure!(valid_name(file_name), "Reset journal filename is invalid");
    anyhow::ensure!(
        valid_hex(expected_digest, /*length*/ 64),
        "Reset journal digest is invalid"
    );
    let store = AccountRuntimeStateStore::new(codex_home.to_path_buf());
    let _lock = store
        .try_lock_reset_credit()?
        .ok_or_else(|| anyhow::anyhow!("Reset credit operation is busy"))?;
    let source = codex_home.join(file_name);
    let bytes = read_bytes(&source)?;
    anyhow::ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == expected_digest,
        "Reset journal changed; inspect it again before archiving"
    );
    anyhow::ensure!(
        matches!(
            classify(&bytes),
            JournalKind::Legacy(..) | JournalKind::Damaged | JournalKind::Pending(..)
        ),
        "Only recognized unresolved or damaged reset journals may be archived; unknown schemas must be retained"
    );
    let directory = codex_home.join(ARCHIVE_DIRECTORY);
    match std::fs::symlink_metadata(&directory) {
        Ok(metadata) => anyhow::ensure!(
            metadata.is_dir(),
            "Reset journal archive must be a directory, never a symbolic link"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&directory)?
        }
        Err(error) => return Err(error.into()),
    }
    let unique = tempfile::tempdir_in(&directory)?.keep();
    let destination = unique.join(file_name);
    anyhow::ensure!(
        std::fs::symlink_metadata(&directory)?.is_dir(),
        "Reset journal archive changed"
    );
    anyhow::ensure!(
        read_bytes(&source)? == bytes,
        "Reset journal changed; inspect it again before archiving"
    );
    std::fs::rename(&source, &destination)?;
    #[cfg(unix)]
    for parent in [
        destination.as_path(),
        unique.as_path(),
        directory.as_path(),
        codex_home,
    ] {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "reset_journal_tests.rs"]
mod tests;
