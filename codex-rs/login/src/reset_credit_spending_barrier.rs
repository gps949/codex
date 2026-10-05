//! Read-only counterpart journals prevent ambiguous resets from spending a second credit.
//! Callers hold the home-scoped spending lock; these checks never acquire it recursively.

use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::io::Read;
use std::path::Path;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManualJournal {
    version: u32,
    operations: Vec<ManualRecord>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManualRecord {
    owner_digest: String,
    idempotency_key: String,
    credit_id: String,
    phase: Option<ManualPhase>,
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
enum ManualPhase {
    Pending,
    Terminal { outcome: ManualOutcome },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum ManualOutcome {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AutomaticRecord {
    version: u32,
    scope: AutomaticScope,
    attempted_at: i64,
    attempted_at_precise: Option<chrono::DateTime<chrono::Utc>>,
    request_id: String,
    phase: AutomaticPhase,
    reconciled_recovery: Option<AutomaticRecovery>,
    previous_confirmed: Option<Box<AutomaticRecord>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AutomaticScope {
    profile_id: String,
    owner_key: String,
    credit_id: Option<String>,
    reset_key: Option<i64>,
    quota_epoch: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AutomaticRecovery {
    quota_epoch: i64,
    observed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
enum AutomaticPhase {
    Pending,
    Confirmed { outcome: AutomaticOutcome },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum AutomaticOutcome {
    QuotaRecovered,
    NoCredit,
}

impl AutomaticRecord {
    fn valid(&self) -> bool {
        self.version == 1
            && !self.request_id.is_empty()
            && self.request_id.len() <= 128
            && !self.scope.profile_id.is_empty()
            && !self.scope.owner_key.is_empty()
            && self
                .attempted_at_precise
                .is_none_or(|at| at.timestamp() == self.attempted_at)
            && self.previous_confirmed.as_ref().is_none_or(|previous| {
                previous.previous_confirmed.is_none()
                    && matches!(previous.phase, AutomaticPhase::Confirmed { .. })
                    && previous.scope.profile_id == self.scope.profile_id
                    && previous.scope.owner_key == self.scope.owner_key
                    && previous.scope.credit_id == self.scope.credit_id
                    && previous.valid()
            })
    }
}

fn bounded_read(path: &Path, maximum: u64) -> io::Result<Option<Vec<u8>>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(io::Error::other(
            "Unresolved reset journal cannot be read safely",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(io::Error::other("Unresolved reset journal is oversized"));
    }
    Ok(Some(bytes))
}

/// Blocks automatic spending while a manual operation has no definitive outcome.
pub fn automatic_reset_spending_blocked(home: &Path) -> io::Result<bool> {
    let Some(bytes) = bounded_read(
        &home.join(".manual-rate-limit-reset-credits.json"),
        /*maximum*/ 4 * 1024 * 1024,
    )?
    else {
        return Ok(false);
    };
    let journal: ManualJournal = serde_json::from_slice(&bytes)?;
    if !matches!(journal.version, 1 | 2) || journal.operations.len() > 4096 {
        return Err(io::Error::other("Unsupported manual reset journal"));
    }
    let mut keys = HashSet::new();
    let mut latest = HashMap::new();
    let mut pending = false;
    for record in &journal.operations {
        if record.owner_digest.len() != 64
            || !record
                .owner_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || record.idempotency_key.is_empty()
            || record.idempotency_key.len() > 128
            || !keys.insert(&record.idempotency_key)
            || record.credit_id.is_empty()
            || record.credit_id.len() > 256
            || journal.version == 1 && record.phase.is_some()
        {
            return Err(io::Error::other("Invalid manual reset journal"));
        }
        pending |= matches!(record.phase, Some(ManualPhase::Pending));
        if let Some(ManualPhase::Terminal { outcome }) = &record.phase {
            match outcome {
                ManualOutcome::Reset
                | ManualOutcome::NothingToReset
                | ManualOutcome::NoCredit
                | ManualOutcome::AlreadyRedeemed => {}
            }
        }
        latest.insert(&record.owner_digest, record.phase.is_none());
    }
    Ok(pending || latest.values().any(|legacy| *legacy))
}

/// Blocks new manual spending while an automatic operation is unresolved.
/// Exact known manual replays retain their original binding and are checked separately.
pub fn manual_reset_spending_blocked(home: &Path) -> io::Result<bool> {
    let entries = match std::fs::read_dir(home) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut count = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(".rate-limit-reset-credit-") || !name.ends_with(".json") {
            continue;
        }
        count += 1;
        if count > 128 {
            return Err(io::Error::other("Too many automatic reset journals"));
        }
        let Some(bytes) = bounded_read(&entry.path(), /*maximum*/ 4096)? else {
            continue;
        };
        // Legacy unbound, damaged and unknown schemas fail closed through deserialization.
        let record: AutomaticRecord = serde_json::from_slice(&bytes)?;
        if !record.valid() {
            return Err(io::Error::other("Invalid automatic reset journal"));
        }
        if let Some(recovery) = &record.reconciled_recovery {
            let _proof = (recovery.quota_epoch, recovery.observed_at);
        }
        let _epoch = (record.scope.reset_key, record.scope.quota_epoch);
        match record.phase {
            AutomaticPhase::Pending => return Ok(true),
            AutomaticPhase::Confirmed { outcome } => match outcome {
                AutomaticOutcome::QuotaRecovered | AutomaticOutcome::NoCredit => {}
            },
        }
    }
    Ok(false)
}

#[cfg(test)]
#[path = "reset_credit_spending_barrier_tests.rs"]
mod tests;
