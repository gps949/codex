//! Durable manual request bindings. The caller holds the shared reset-credit spending lock.

use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

const JOURNAL_FILE_NAME: &str = ".manual-rate-limit-reset-credits.json";
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_OPERATIONS: usize = 4096;
pub(super) const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
pub(super) const MAX_CREDIT_ID_BYTES: usize = 256;
const JOURNAL_UNAVAILABLE: &str = "reset operation journal is unavailable; no credit was spent";
const JOURNAL_INVALID: &str = "reset operation journal is invalid; no credit was spent";
const JOURNAL_FULL: &str = "reset operation journal is full; no credit was spent";
const BINDING_CHANGED: &str =
    "idempotencyKey is bound to a different account or credit; retry the original operation";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManualResetCreditRecord {
    owner_digest: String,
    idempotency_key: String,
    credit_id: String,
}

impl ManualResetCreditRecord {
    fn is_valid(&self) -> bool {
        self.owner_digest.len() == 64
            && self
                .owner_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && !self.idempotency_key.is_empty()
            && self.idempotency_key.len() <= MAX_IDEMPOTENCY_KEY_BYTES
            && !self.credit_id.is_empty()
            && self.credit_id.len() <= MAX_CREDIT_ID_BYTES
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManualResetCreditRecords {
    version: u32,
    operations: Vec<ManualResetCreditRecord>,
}

pub(super) struct ManualResetCreditJournal {
    path: PathBuf,
    records: ManualResetCreditRecords,
}

impl ManualResetCreditJournal {
    pub(super) fn load(codex_home: &Path) -> Result<Self, &'static str> {
        let path = codex_home.join(JOURNAL_FILE_NAME);
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
                if records.version != 1
                    || records.operations.len() > MAX_OPERATIONS
                    || records.operations.iter().any(|record| {
                        !record.is_valid() || !keys.insert(record.idempotency_key.as_str())
                    })
                {
                    return Err(JOURNAL_INVALID);
                }
                records
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ManualResetCreditRecords {
                    version: 1,
                    operations: Vec::new(),
                }
            }
            Err(_) => return Err(JOURNAL_UNAVAILABLE),
        };
        Ok(Self { path, records })
    }

    pub(super) fn known_credit(
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

    pub(super) fn remember(
        &mut self,
        owner_digest: &str,
        idempotency_key: &str,
        credit_id: &str,
    ) -> Result<(), &'static str> {
        if self
            .known_credit(owner_digest, idempotency_key, Some(credit_id))?
            .is_some()
        {
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
        };
        if !record.is_valid() {
            return Err(JOURNAL_INVALID);
        }
        self.records.operations.push(record);
        let persisted = (|| {
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
            #[cfg(unix)]
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| JOURNAL_UNAVAILABLE)?;
            Ok(())
        })();
        if persisted.is_err() {
            self.records.operations.pop();
        }
        persisted
    }
}

#[cfg(test)]
#[path = "manual_reset_credit_journal_tests.rs"]
mod tests;
