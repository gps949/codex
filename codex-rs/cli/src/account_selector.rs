use codex_login::AccountProfileRecord;
use codex_login::AccountProfileState;
use codex_login::AuthConfig;

/// Preserves exact IDs, then resolves the union of local custom-label and email matches.
pub(crate) fn resolve_account_with_config<'a>(
    records: &'a [AccountProfileRecord],
    selector: &str,
    auth_config: &AuthConfig,
) -> Result<&'a AccountProfileRecord, String> {
    if let Some(record) = records
        .iter()
        .find(|record| record.profile.id.as_str() == selector)
    {
        return Ok(record);
    }
    let name = selector.trim();
    let mut matches = records.iter().filter(|record| {
        if name.is_empty() {
            return false;
        }
        if record.profile.label.as_deref().map(str::trim) == Some(name) {
            return true;
        }
        record.state == AccountProfileState::Ready
            && codex_login::account_identity::load_login_identity(
                &record.profile.credential_home,
                auth_config.auth_credentials_store_mode,
                auth_config.keyring_backend_kind,
            )
            .ok()
            .flatten()
            .and_then(|identity| identity.email)
            .is_some_and(|email| email.trim() == name)
    });
    let Some(record) = matches.next() else {
        return Err(format!(
            "No account matches {selector:?}. Use `codex account list` (or `codex account list --show-profile`) to see emails, labels, and ids."
        ));
    };
    if matches.next().is_some() {
        return Err(format!(
            "Account name {selector:?} is ambiguous (matches more than one profile). Select an exact profile ID from `codex account list --show-profile`, or assign a unique label with `codex account set <id> --label <label>`."
        ));
    }
    Ok(record)
}

#[cfg(test)]
#[path = "account_selector_tests.rs"]
mod tests;
