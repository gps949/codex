//! Explicit review of old, unbound reset operations; original records are kept as backups.

use super::Locale;
use super::Operation;
use super::display::clean;
use super::input::prompt;
use codex_app_server::account_management::ResetJournalView;

pub(super) async fn choose(
    journals: &[ResetJournalView],
    locale: Locale,
) -> anyhow::Result<Option<Operation>> {
    println!("{}", locale.text("Interrupted reset operations"));
    for (index, journal) in journals.iter().enumerate() {
        let profile = journal
            .manual
            .as_ref()
            .map(|manual| manual.idempotency_key.as_str())
            .or(journal.profile_id.as_deref())
            .unwrap_or("Unknown account");
        println!(
            "{:>3}  {} · {}",
            index + 1,
            clean(locale.message(profile)),
            clean(locale.message(&journal.message))
        );
    }
    let selected = prompt(locale, "Record number (Enter returns)", "").await?;
    if selected.is_empty() {
        return Ok(None);
    }
    let record = selected
        .parse::<usize>()
        .ok()
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| journals.get(index))
        .ok_or_else(|| anyhow::anyhow!(locale.text("Choose a listed record number")))?;
    if let Some(manual) = &record.manual {
        println!("{}", locale.text("Check quota and credit history independently first. This credit may already have been consumed. Review retains the original binding and a backup; its outcome stays unknown. Later operations may use another credit. Quota and account access stay unchanged."));
        if prompt(
            locale,
            "Type REVIEW to acknowledge the unknown reset outcome",
            "",
        )
        .await?
            != "REVIEW"
        {
            return Ok(None);
        }
        return Ok(Some(Operation::ManualResetReview {
            owner_key: manual.owner_key.clone(),
            idempotency_key: manual.idempotency_key.clone(),
            expected_digest: manual.digest.clone(),
            acknowledge_unconfirmed: true,
        }));
    }
    anyhow::ensure!(
        record.archive_available,
        locale.text("This record cannot be archived safely. Refresh quota or review its status.")
    );
    println!("{}", locale.text("Check this account's quota and reset history first. Archiving abandons the old request and may allow another credit to be used later. A backup is kept; quota is unchanged."));
    if prompt(locale, "Type yes to archive the reviewed record", "no").await? != "yes" {
        return Ok(None);
    }
    Ok(Some(Operation::ResetJournalArchive {
        file_name: record.file_name.clone(),
        expected_digest: record.digest.clone(),
        acknowledge_unconfirmed: true,
    }))
}
