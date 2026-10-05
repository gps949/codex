//! Durable, bounded reset-credit operations. The caller holds the shared spending lock.

use chrono::DateTime;
use chrono::Utc;
use codex_login::AccountPool;
use codex_login::AccountRuntimeState;
use codex_login::AccountRuntimeStateStore;
use codex_login::CodexAuth;
use serde::Deserialize;
use serde::Serialize;
use sha1::Digest;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

const MAX_RECORD_BYTES: u64 = 4096;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ResetCreditScope {
    pub(super) profile_id: String,
    pub(super) owner_key: String,
    pub(super) credit_id: Option<String>,
    pub(super) reset_key: Option<i64>,
    pub(super) quota_epoch: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ResetCreditCompletion {
    QuotaRecovered,
    NoCredit,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum ResetCreditPhase {
    Pending,
    Confirmed { outcome: ResetCreditCompletion },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResetCreditRecord {
    version: u32,
    scope: ResetCreditScope,
    attempted_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempted_at_precise: Option<DateTime<Utc>>,
    request_id: String,
    phase: ResetCreditPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reconciled_recovery: Option<ResetCreditRecovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_confirmed: Option<Box<ResetCreditRecord>>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResetCreditRecovery {
    quota_epoch: i64,
    observed_at: DateTime<Utc>,
}

pub(super) struct ResetCreditOperation {
    path: PathBuf,
    record: ResetCreditRecord,
}

fn read_record(path: &Path) -> anyhow::Result<Option<ResetCreditRecord>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_RECORD_BYTES,
        "Reset operation journal is oversized"
    );
    // Old unversioned attempts have no owner binding and must also block new spending.
    let record: ResetCreditRecord = serde_json::from_slice(&bytes)?;
    for attempt in std::iter::once(&record).chain(record.previous_confirmed.as_deref()) {
        anyhow::ensure!(
            attempt.version == 1
                && !attempt.request_id.is_empty()
                && attempt.request_id.len() <= 128
                && !attempt.scope.profile_id.is_empty()
                && !attempt.scope.owner_key.is_empty()
                && attempt
                    .attempted_at_precise
                    .is_none_or(|time| time.timestamp() == attempt.attempted_at),
            "Reset operation journal is invalid"
        );
    }
    anyhow::ensure!(
        record.previous_confirmed.as_ref().is_none_or(|previous| {
            previous.previous_confirmed.is_none()
                && previous.phase != ResetCreditPhase::Pending
                && previous.scope.profile_id == record.scope.profile_id
                && previous.scope.owner_key == record.scope.owner_key
                && previous.scope.credit_id == record.scope.credit_id
        }),
        "Reset operation history is invalid"
    );
    Ok(Some(record))
}

pub(super) fn owner_key(auth: &CodexAuth) -> Option<String> {
    if !auth.is_chatgpt_auth()
        || auth.get_account_id().is_none_or(|id| id.trim().is_empty())
        || auth
            .get_chatgpt_user_id()
            .is_none_or(|id| id.trim().is_empty())
    {
        return None;
    }
    let owner = serde_json::to_vec(&(auth.get_account_id(), auth.get_chatgpt_user_id())).ok()?;
    Some(format!("{:x}", sha1::Sha1::digest(owner)))
}

impl ResetCreditOperation {
    /// Ends only the exact persisted pending request whose original seat has confirmed recovery.
    /// The caller holds the spending lock; a later refusal does not erase this historical proof.
    pub(super) async fn reconcile_pending(
        pool: &AccountPool,
        store: &AccountRuntimeStateStore,
        state: &AccountRuntimeState,
    ) -> anyhow::Result<()> {
        let state_path = store.path();
        let home = state_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Reset operation path has no parent"))?;
        let managers = pool.auth_managers();
        let mut records = 0;
        for entry in std::fs::read_dir(home)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(".rate-limit-reset-credit-") || !name.ends_with(".json") {
                continue;
            }
            records += 1;
            anyhow::ensure!(
                records <= 128,
                "Too many reset operation journals to safely reconcile"
            );
            let Some(mut record) = read_record(&entry.path())? else {
                continue;
            };
            if record.phase != ResetCreditPhase::Pending {
                continue;
            }
            let profile_key = format!(
                "{:x}",
                sha1::Sha1::digest(record.scope.profile_id.as_bytes())
            );
            anyhow::ensure!(
                entry.path() == home.join(format!(".rate-limit-reset-credit-{profile_key}.json")),
                "Reset operation profile does not match its journal"
            );
            let Some(profile) = state
                .profiles
                .iter()
                .find(|profile| profile.profile_id.as_str() == record.scope.profile_id)
            else {
                continue;
            };
            let (Some(epoch), Some(observed_at)) =
                (profile.quota_reset_at, profile.quota_reset_observed_at)
            else {
                continue;
            };
            let attempted_at = record.attempted_at_precise.or_else(|| {
                record
                    .attempted_at
                    .checked_add(1)
                    .and_then(|time| DateTime::from_timestamp(time, 0))
            });
            if Some(epoch.timestamp_millis()) <= record.scope.quota_epoch
                || attempted_at.is_none_or(|attempted_at| observed_at <= attempted_at)
                || observed_at > Utc::now()
            {
                continue;
            }
            let Some((_, manager)) = managers.iter().find(|(id, _)| id == &profile.profile_id)
            else {
                continue;
            };
            manager.reload().await;
            let Some(auth) = manager.auth_cached() else {
                continue;
            };
            let revision = manager.auth_change_state_receiver();
            let owner_generation = revision.borrow().owner_generation;
            if owner_key(&auth).as_deref() != Some(record.scope.owner_key.as_str())
                || !store.validate_profile_auth(pool, &profile.profile_id, &auth)?
                || revision.borrow().owner_generation != owner_generation
                || manager
                    .auth_cached()
                    .as_ref()
                    .and_then(owner_key)
                    .as_deref()
                    != Some(record.scope.owner_key.as_str())
            {
                continue;
            }
            record.phase = ResetCreditPhase::Confirmed {
                outcome: ResetCreditCompletion::QuotaRecovered,
            };
            record.reconciled_recovery = Some(ResetCreditRecovery {
                quota_epoch: epoch.timestamp_millis(),
                observed_at,
            });
            Self {
                path: entry.path(),
                record,
            }
            .persist()?;
        }
        Ok(())
    }

    pub(super) fn prepare(
        codex_home: &Path,
        scope: ResetCreditScope,
        proposed_request_id: &str,
    ) -> anyhow::Result<Self> {
        let profile_key = format!("{:x}", sha1::Sha1::digest(scope.profile_id.as_bytes()));
        let path = codex_home.join(format!(".rate-limit-reset-credit-{profile_key}.json"));
        let mut other_records = 0;
        for entry in std::fs::read_dir(codex_home)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.path() == path
                || !name.starts_with(".rate-limit-reset-credit-")
                || !name.ends_with(".json")
            {
                continue;
            }
            other_records += 1;
            anyhow::ensure!(
                other_records <= 128,
                "Too many reset operation journals to safely spend"
            );
            if let Some(record) = read_record(&entry.path())? {
                anyhow::ensure!(
                    record.phase != ResetCreditPhase::Pending,
                    "Another profile has an unconfirmed reset operation; no new credit was spent"
                );
            }
        }
        let mut previous = read_record(&path)?;
        if let Some(record) = previous.as_ref() {
            anyhow::ensure!(
                record.scope.profile_id == scope.profile_id
                    && record.scope.owner_key == scope.owner_key
                    && record.scope.credit_id == scope.credit_id,
                "Reset operation owner changed; reconcile the original operation before spending"
            );
            match &record.phase {
                ResetCreditPhase::Pending => {
                    return Ok(Self {
                        path,
                        record: record.clone(),
                    });
                }
                ResetCreditPhase::Confirmed {
                    outcome: ResetCreditCompletion::QuotaRecovered,
                } => {
                    anyhow::ensure!(
                        scope.quota_epoch > record.scope.quota_epoch,
                        "A new confirmed quota epoch is required before spending another credit"
                    );
                }
                ResetCreditPhase::Confirmed {
                    outcome: ResetCreditCompletion::NoCredit,
                } => {}
            }
        }
        // An existing owner-bound pending operation above reuses its original request.
        // Only new spending is paused by an unresolved manual operation.
        anyhow::ensure!(
            !codex_login::automatic_reset_spending_blocked(codex_home)?,
            "Manual reset remains unconfirmed; no new automatic credit was spent"
        );
        anyhow::ensure!(
            !proposed_request_id.is_empty() && proposed_request_id.len() <= 128,
            "Reset operation ID is invalid"
        );
        if let Some(record) = previous.as_mut() {
            // Keep one confirmed predecessor, never an unbounded nested operation history.
            record.previous_confirmed = None;
        }
        let attempted_at = Utc::now();
        let operation = Self {
            path,
            record: ResetCreditRecord {
                version: 1,
                scope,
                attempted_at: attempted_at.timestamp(),
                attempted_at_precise: Some(attempted_at),
                request_id: proposed_request_id.into(),
                phase: ResetCreditPhase::Pending,
                reconciled_recovery: None,
                previous_confirmed: previous.map(Box::new),
            },
        };
        operation.persist()?;
        Ok(operation)
    }

    pub(super) fn request_id(&self) -> &str {
        &self.record.request_id
    }

    pub(super) fn complete(mut self, outcome: ResetCreditCompletion) -> anyhow::Result<()> {
        self.record.phase = ResetCreditPhase::Confirmed { outcome };
        self.persist()
    }

    fn persist(&self) -> anyhow::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Reset journal path has no parent"))?;
        let bytes = serde_json::to_vec(&self.record)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_RECORD_BYTES,
            "Reset operation journal is oversized"
        );
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "reset_credit_operation_tests.rs"]
mod tests;
