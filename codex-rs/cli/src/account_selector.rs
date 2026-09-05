use codex_login::AccountProfileRecord;

/// Preserve exact ID selection for scripts, then resolve a unique human-readable label.
pub(crate) fn resolve_account<'a>(
    records: &'a [AccountProfileRecord],
    selector: &str,
) -> Result<&'a AccountProfileRecord, String> {
    if let Some(record) = records
        .iter()
        .find(|record| record.profile.id.as_str() == selector)
    {
        return Ok(record);
    }
    let mut matches = records
        .iter()
        .filter(|record| record.profile.label.as_deref() == Some(selector));
    let Some(record) = matches.next() else {
        return Err(format!(
            "No account matches {selector:?}. Use `codex account list` to see IDs and labels."
        ));
    };
    if matches.next().is_some() {
        return Err(format!(
            "Account label {selector:?} is ambiguous. Select an exact profile ID or assign a unique label with `codex account set <id> --label <label>`."
        ));
    }
    Ok(record)
}
