//! Review keeps an unknown manual outcome and its original binding while releasing new spending.

use super::*;
use codex_login::AccountRuntimeStateStore;
use std::collections::HashMap;

/// Nonsecret evidence for reviewing an operation even when its original account is unavailable.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualResetCreditReviewView {
    pub owner_key: String,
    pub idempotency_key: String,
    pub credit_id: String,
    pub digest: String,
    pub legacy: bool,
}

impl ManualResetCreditJournal {
    pub(crate) fn review_views(&self) -> Result<Vec<ManualResetCreditReviewView>, &'static str> {
        let latest = self
            .records
            .operations
            .iter()
            .enumerate()
            .map(|(index, record)| (record.owner_digest.as_str(), index))
            .collect::<HashMap<_, _>>();
        let Some(bytes) = &self.source_bytes else {
            return Ok(Vec::new());
        };
        let digest = format!("{:x}", sha2::Sha256::digest(bytes));
        Ok(self
            .records
            .operations
            .iter()
            .enumerate()
            .filter(|(index, record)| {
                matches!(record.phase, Some(ManualResetCreditPhase::Pending))
                    || record.phase.is_none()
                        && latest.get(record.owner_digest.as_str()) == Some(index)
            })
            .map(|(_, record)| ManualResetCreditReviewView {
                owner_key: record.owner_digest.clone(),
                idempotency_key: record.idempotency_key.clone(),
                credit_id: record.credit_id.clone(),
                digest: digest.clone(),
                legacy: record.phase.is_none(),
            })
            .collect())
    }
}

pub(crate) fn review_manual_reset(
    home: &Path,
    owner_key: &str,
    idempotency_key: &str,
    expected_digest: &str,
    acknowledge_unconfirmed: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        acknowledge_unconfirmed,
        "Review requires independently checking quota and credit history; the outcome may remain unknown and later operations may use another credit"
    );
    anyhow::ensure!(
        valid_owner_key(owner_key) && valid_owner_key(expected_digest),
        "Reset review owner or digest is invalid"
    );
    let store = AccountRuntimeStateStore::new(home.to_path_buf());
    let _lock = store
        .try_lock_reset_credit()?
        .ok_or_else(|| anyhow::anyhow!("Reset credit operation is busy"))?;
    let mut journal = ManualResetCreditJournal::load(home).map_err(anyhow::Error::msg)?;
    let bytes = journal
        .source_bytes
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!(JOURNAL_UNAVAILABLE))?;
    anyhow::ensure!(
        format!("{:x}", sha2::Sha256::digest(bytes)) == expected_digest,
        "Reset journal changed; inspect it again before reviewing"
    );
    let views = journal.review_views().map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        views
            .iter()
            .any(|view| view.owner_key == owner_key && view.idempotency_key == idempotency_key),
        "Only the current unresolved operation may be reviewed; its original binding is retained"
    );
    let directory = home.join(".reset-credit-journal-archive");
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
    let backup = tempfile::tempdir_in(&directory)?.keep();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(backup.join(JOURNAL_FILE_NAME))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    for parent in [&backup, &directory] {
        std::fs::File::open(parent)?.sync_all()?;
    }
    let current = ManualResetCreditJournal::load(home).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        current.source_bytes == journal.source_bytes,
        "Reset journal changed while saving its backup; inspect it again"
    );
    let record = journal
        .records
        .operations
        .iter_mut()
        .find(|record| record.idempotency_key == idempotency_key)
        .ok_or_else(|| anyhow::anyhow!(JOURNAL_INVALID))?;
    record.phase = Some(ManualResetCreditPhase::Reviewed {
        reviewed_at: chrono::Utc::now().timestamp(),
    });
    journal.persist().map_err(anyhow::Error::msg)
}
