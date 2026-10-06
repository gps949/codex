//! Durable manual reset operations. Callers hold the shared spending lock during mutations.

use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome;
use codex_app_server_protocol::PendingAccountRateLimitResetCredit;
use codex_login::CodexAuth;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use std::collections::HashSet;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

const JOURNAL_FILE_NAME: &str = ".manual-rate-limit-reset-credits.json";
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_OPERATIONS: usize = 4096;
pub(crate) const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
pub(crate) const MAX_CREDIT_ID_BYTES: usize = 256;
const JOURNAL_UNAVAILABLE: &str = "reset operation journal is unavailable; no credit was spent";
const JOURNAL_INVALID: &str = "reset operation journal is invalid; no credit was spent";
const JOURNAL_FULL: &str = "reset operation journal is full; no credit was spent";
const BINDING_CHANGED: &str =
    "idempotencyKey is bound to a different account or credit; retry the original operation";
const PENDING_OPERATION: &str =
    "an unconfirmed reset exists for this account; retry the original operation";

#[path = "reset_credit_journal_review.rs"]
mod review;
pub use review::ManualResetCreditReviewView;
pub(crate) use review::review_manual_reset;

pub(crate) fn valid_owner_key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn owner_key(base_url: &str, auth: &CodexAuth) -> Result<String, &'static str> {
    let account = auth
        .get_account_id()
        .filter(|id| !id.trim().is_empty())
        .ok_or("account identity required to bind rate limit reset retries")?;
    let user = auth
        .get_chatgpt_user_id()
        .filter(|id| !id.trim().is_empty())
        .ok_or("account identity required to bind rate limit reset retries")?;
    let mut owner = sha2::Sha256::new();
    owner.update(b"codex-manual-reset-credit-owner-v1\0");
    for value in [
        base_url.trim_end_matches('/'),
        account.as_str(),
        user.as_str(),
    ] {
        owner.update((value.len() as u64).to_be_bytes());
        owner.update(value.as_bytes());
    }
    Ok(format!("{:x}", owner.finalize()))
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
enum ManualResetCreditPhase {
    Pending,
    /// An explicit acknowledgement releases new spending without claiming a backend result.
    Reviewed {
        #[serde(rename = "reviewedAt")]
        reviewed_at: i64,
    },
    Terminal {
        outcome: ConsumeAccountRateLimitResetCreditOutcome,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManualResetCreditRecord {
    owner_digest: String,
    idempotency_key: String,
    credit_id: String,
    /// Missing phases are legacy history with an unknown outcome, never inferred success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<ManualResetCreditPhase>,
}

impl ManualResetCreditRecord {
    fn is_valid(&self) -> bool {
        valid_owner_key(&self.owner_digest)
            && !self.idempotency_key.is_empty()
            && self.idempotency_key.len() <= MAX_IDEMPOTENCY_KEY_BYTES
            && !self.credit_id.is_empty()
            && self.credit_id.len() <= MAX_CREDIT_ID_BYTES
            && match self.phase {
                Some(ManualResetCreditPhase::Reviewed { reviewed_at }) => reviewed_at >= 0,
                Some(ManualResetCreditPhase::Pending | ManualResetCreditPhase::Terminal { .. })
                | None => true,
            }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManualResetCreditRecords {
    version: u32,
    operations: Vec<ManualResetCreditRecord>,
}

pub(crate) struct ManualResetCreditJournal {
    path: PathBuf,
    records: ManualResetCreditRecords,
    source_bytes: Option<Vec<u8>>,
}

impl ManualResetCreditJournal {
    pub(crate) fn load(codex_home: &Path) -> Result<Self, &'static str> {
        let path = codex_home.join(JOURNAL_FILE_NAME);
        if let Ok(metadata) = std::fs::symlink_metadata(&path)
            && !metadata.is_file()
        {
            return Err(JOURNAL_UNAVAILABLE);
        }
        let mut source_bytes = None;
        let records = match std::fs::File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_JOURNAL_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| JOURNAL_UNAVAILABLE)?;
                if bytes.len() as u64 > MAX_JOURNAL_BYTES {
                    return Err(JOURNAL_INVALID);
                }
                let records: ManualResetCreditRecords =
                    serde_json::from_slice(&bytes).map_err(|_| JOURNAL_INVALID)?;
                let mut keys = HashSet::new();
                let mut pending_owners = HashSet::new();
                if !matches!(records.version, 1 | 2)
                    || records.operations.len() > MAX_OPERATIONS
                    || records.operations.iter().any(|record| {
                        !record.is_valid()
                            || !keys.insert(record.idempotency_key.as_str())
                            || records.version == 1 && record.phase.is_some()
                            || matches!(record.phase, Some(ManualResetCreditPhase::Pending))
                                && !pending_owners.insert(record.owner_digest.as_str())
                    })
                {
                    return Err(JOURNAL_INVALID);
                }
                source_bytes = Some(bytes);
                records
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ManualResetCreditRecords {
                    version: 2,
                    operations: Vec::new(),
                }
            }
            Err(_) => return Err(JOURNAL_UNAVAILABLE),
        };
        Ok(Self {
            path,
            records,
            source_bytes,
        })
    }

    pub(crate) fn known_credit(
        &self,
        owner_digest: &str,
        idempotency_key: &str,
        expected_credit_id: Option<&str>,
    ) -> Result<Option<&str>, &'static str> {
        let Some(record) = self
            .records
            .operations
            .iter()
            .find(|record| record.idempotency_key == idempotency_key)
        else {
            return Ok(None);
        };
        if record.owner_digest != owner_digest
            || expected_credit_id.is_some_and(|id| record.credit_id != id)
        {
            return Err(BINDING_CHANGED);
        }
        Ok(Some(record.credit_id.as_str()))
    }

    pub(crate) fn terminal_outcome(
        &self,
        owner_digest: &str,
        idempotency_key: &str,
    ) -> Result<Option<ConsumeAccountRateLimitResetCreditOutcome>, &'static str> {
        self.known_credit(
            owner_digest,
            idempotency_key,
            /*expected_credit_id*/ None,
        )?;
        Ok(self.records.operations.iter().find_map(|record| {
            if record.idempotency_key == idempotency_key
                && let Some(ManualResetCreditPhase::Terminal { outcome }) = record.phase
            {
                Some(outcome)
            } else {
                None
            }
        }))
    }

    pub(crate) fn pending_for_owner(
        &self,
        owner_digest: &str,
    ) -> Option<PendingAccountRateLimitResetCredit> {
        let record = self
            .records
            .operations
            .iter()
            .find(|record| {
                record.owner_digest == owner_digest
                    && matches!(record.phase, Some(ManualResetCreditPhase::Pending))
            })
            .or_else(|| {
                self.records
                    .operations
                    .iter()
                    .rev()
                    .find(|record| record.owner_digest == owner_digest)
                    .filter(|record| record.phase.is_none())
            })?;
        Some(PendingAccountRateLimitResetCredit {
            owner_key: record.owner_digest.clone(),
            idempotency_key: record.idempotency_key.clone(),
            credit_id: Some(record.credit_id.clone()),
        })
    }

    pub(crate) fn check_pending(
        &self,
        owner_digest: &str,
        idempotency_key: &str,
    ) -> Result<(), &'static str> {
        if self
            .pending_for_owner(owner_digest)
            .is_some_and(|pending| pending.idempotency_key != idempotency_key)
        {
            return Err(PENDING_OPERATION);
        }
        Ok(())
    }

    pub(crate) fn remember(
        &mut self,
        owner_digest: &str,
        idempotency_key: &str,
        credit_id: &str,
    ) -> Result<(), &'static str> {
        self.known_credit(owner_digest, idempotency_key, Some(credit_id))?;
        self.check_pending(owner_digest, idempotency_key)?;
        if self
            .known_credit(owner_digest, idempotency_key, Some(credit_id))?
            .is_none()
            && codex_login::manual_reset_spending_blocked(
                self.path.parent().ok_or(JOURNAL_UNAVAILABLE)?,
            )
            .map_err(|_| JOURNAL_INVALID)?
        {
            return Err(
                "an automatic reset is unconfirmed; review its outcome before using another credit",
            );
        }
        if let Some(index) = self
            .records
            .operations
            .iter()
            .position(|record| record.idempotency_key == idempotency_key)
        {
            if matches!(
                self.records.operations[index].phase,
                None | Some(ManualResetCreditPhase::Reviewed { .. })
            ) {
                let previous = self.records.operations[index].phase;
                self.records.operations[index].phase = Some(ManualResetCreditPhase::Pending);
                if let Err(error) = self.persist() {
                    self.records.operations[index].phase = previous;
                    return Err(error);
                }
            }
            return Ok(());
        }
        // Never evict an old binding: a later replay must not become a fresh selection.
        if self.records.operations.len() >= MAX_OPERATIONS {
            return Err(JOURNAL_FULL);
        }
        let record = ManualResetCreditRecord {
            owner_digest: owner_digest.into(),
            idempotency_key: idempotency_key.into(),
            credit_id: credit_id.into(),
            phase: Some(ManualResetCreditPhase::Pending),
        };
        if !record.is_valid() {
            return Err(JOURNAL_INVALID);
        }
        self.records.operations.push(record);
        let persisted = self.persist();
        if persisted.is_err() {
            self.records.operations.pop();
        }
        persisted
    }

    pub(crate) fn complete(
        &mut self,
        owner_digest: &str,
        idempotency_key: &str,
        outcome: ConsumeAccountRateLimitResetCreditOutcome,
    ) -> Result<(), &'static str> {
        self.known_credit(
            owner_digest,
            idempotency_key,
            /*expected_credit_id*/ None,
        )?
        .ok_or(JOURNAL_INVALID)?;
        let index = self
            .records
            .operations
            .iter()
            .position(|record| record.idempotency_key == idempotency_key)
            .ok_or(JOURNAL_INVALID)?;
        let previous = self.records.operations[index].phase;
        self.records.operations[index].phase = Some(ManualResetCreditPhase::Terminal { outcome });
        if let Err(error) = self.persist() {
            self.records.operations[index].phase = previous;
            return Err(error);
        }
        Ok(())
    }

    fn persist(&mut self) -> Result<(), &'static str> {
        self.records.version = 2;
        let bytes = serde_json::to_vec(&self.records).map_err(|_| JOURNAL_INVALID)?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(JOURNAL_FULL);
        }
        let parent = self.path.parent().ok_or(JOURNAL_UNAVAILABLE)?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| JOURNAL_UNAVAILABLE)?;
        temporary
            .write_all(&bytes)
            .map_err(|_| JOURNAL_UNAVAILABLE)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| JOURNAL_UNAVAILABLE)?;
        temporary
            .persist(&self.path)
            .map_err(|_| JOURNAL_UNAVAILABLE)?;
        self.source_bytes = Some(bytes);
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| JOURNAL_UNAVAILABLE)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "reset_credit_journal_tests.rs"]
mod tests;
