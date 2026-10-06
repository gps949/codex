//! Confirms an interrupted automatic reset only from persisted, original-owner recovery proof.

use crate::AccountPool;
use crate::AccountRuntimeState;
use crate::AccountRuntimeStateStore;
use crate::CodexAuth;
use crate::reset_credit_spending_barrier::AutomaticOutcome;
use crate::reset_credit_spending_barrier::AutomaticPhase;
use crate::reset_credit_spending_barrier::AutomaticRecord;
use crate::reset_credit_spending_barrier::AutomaticRecovery;
use chrono::DateTime;
use chrono::Utc;
use sha1::Digest;
use std::io;
use std::io::Read;
use std::io::Write;

fn owner_key(auth: &CodexAuth) -> Option<String> {
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

/// Records historical recovery without modifying quota, selection, authentication or request IDs.
/// The caller must hold the home-scoped reset-credit spending lock; it is never reacquired here.
/// Metadata-only reads, force probes, different owners and pre-request observations prove nothing.
pub async fn reconcile_reset_credit_recovery(
    pool: &AccountPool,
    store: &AccountRuntimeStateStore,
    state: &AccountRuntimeState,
) -> io::Result<()> {
    let state_path = store.path();
    let home = state_path
        .parent()
        .ok_or_else(|| io::Error::other("Reset operation path has no parent"))?;
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
        if records > 128 {
            return Err(io::Error::other(
                "Too many reset journals to safely reconcile",
            ));
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.len() > 4096 {
            return Err(io::Error::other(
                "Reset operation journal cannot be read safely",
            ));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)?
            .take(/*limit*/ 4097)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(io::Error::other("Reset operation journal is oversized"));
        }
        let mut record: AutomaticRecord =
            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if !record.valid() {
            return Err(io::Error::other("Reset operation journal is invalid"));
        }
        if !matches!(record.phase, AutomaticPhase::Pending) {
            continue;
        }
        let profile_key = format!(
            "{:x}",
            sha1::Sha1::digest(record.scope.profile_id.as_bytes())
        );
        if path != home.join(format!(".rate-limit-reset-credit-{profile_key}.json")) {
            return Err(io::Error::other(
                "Reset operation profile does not match its journal",
            ));
        }
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
                .and_then(|time| DateTime::from_timestamp(time, /*nsecs*/ 0))
        });
        if Some(epoch.timestamp_millis()) <= record.scope.quota_epoch
            || attempted_at.is_none_or(|attempt| observed_at <= attempt)
            || observed_at > Utc::now()
        {
            continue;
        }
        let Some((_, manager)) = managers.iter().find(|(id, _)| id == &profile.profile_id) else {
            continue;
        };
        manager.reload().await;
        let Some(auth) = manager.auth_cached() else {
            continue;
        };
        let revision = manager.auth_change_state_receiver();
        let generation = revision.borrow().owner_generation;
        if owner_key(&auth).as_deref() != Some(record.scope.owner_key.as_str())
            || !store
                .validate_profile_auth(pool, &profile.profile_id, &auth)
                .map_err(io::Error::other)?
            || revision.borrow().owner_generation != generation
            || manager
                .auth_cached()
                .as_ref()
                .and_then(owner_key)
                .as_deref()
                != Some(record.scope.owner_key.as_str())
        {
            continue;
        }
        // A newer refusal does not erase this proof and is never cleared by journal reconciliation.
        record.phase = AutomaticPhase::Confirmed {
            outcome: AutomaticOutcome::QuotaRecovered,
        };
        record.reconciled_recovery = Some(AutomaticRecovery {
            quota_epoch: epoch.timestamp_millis(),
            observed_at,
        });
        let updated = serde_json::to_vec(&record).map_err(io::Error::other)?;
        if updated.len() > 4096 {
            return Err(io::Error::other("Reset operation journal is oversized"));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(home)?;
        temporary.write_all(&updated)?;
        temporary.as_file().sync_all()?;
        if std::fs::read(&path)? != bytes {
            return Err(io::Error::other(
                "Reset operation journal changed during reconciliation",
            ));
        }
        temporary.persist(&path).map_err(|error| error.error)?;
        #[cfg(unix)]
        std::fs::File::open(home)?.sync_all()?;
    }
    Ok(())
}
