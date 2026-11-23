use codex_login::AccountProfileRecord;
use codex_login::AccountProfileState;
use codex_login::AuthConfig;

/// Preserves exact IDs, then resolves the union of local custom-label and email matches,
/// falling back to unique case-insensitive ID/label/email matches (with optional `@` prefix).
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
    let trimmed = selector.trim();
    let name = trimmed.strip_prefix('@').unwrap_or(trimmed).trim();
    if name.is_empty() {
        return Err(format!(
            "No account matches {selector:?}. Use `codex account list` (or `codex account list --show-profile`) to see emails, labels, and ids."
        ));
    }
    if let Some(record) = records
        .iter()
        .find(|record| record.profile.id.as_str() == name)
    {
        return Ok(record);
    }

    let ready_email = |record: &AccountProfileRecord| -> Option<String> {
        if record.state != AccountProfileState::Ready {
            return None;
        }
        codex_login::account_identity::load_login_identity(
            &record.profile.credential_home,
            auth_config.auth_credentials_store_mode,
            auth_config.keyring_backend_kind,
        )
        .ok()
        .flatten()
        .and_then(|identity| identity.email)
        .map(|email| email.trim().to_string())
    };

    let exact_matches: Vec<_> = records
        .iter()
        .filter(|record| {
            record.profile.label.as_deref().map(str::trim) == Some(name)
                || ready_email(record).as_deref() == Some(name)
        })
        .collect();
    match exact_matches.len() {
        1 => return Ok(exact_matches[0]),
        count if count > 1 => {
            return Err(format!(
                "Account name {selector:?} is ambiguous (matches more than one profile). Select an exact profile ID from `codex account list --show-profile`, or assign a unique label with `codex account set <id> --label <label>`."
            ));
        }
        _ => {}
    }

    let ci_matches: Vec<_> = records
        .iter()
        .filter(|record| {
            record.profile.id.as_str().eq_ignore_ascii_case(name)
                || record
                    .profile
                    .label
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|label| label.eq_ignore_ascii_case(name))
                || ready_email(record)
                    .as_deref()
                    .is_some_and(|email| email.eq_ignore_ascii_case(name))
        })
        .collect();
    match ci_matches.len() {
        1 => Ok(ci_matches[0]),
        0 => Err(format!(
            "No account matches {selector:?}. Use `codex account list` (or `codex account list --show-profile`) to see emails, labels, and ids."
        )),
        _ => Err(format!(
            "Account name {selector:?} is ambiguous (matches more than one profile). Select an exact profile ID from `codex account list --show-profile`, or assign a unique label with `codex account set <id> --label <label>`."
        )),
    }
}

#[cfg(test)]
#[path = "account_selector_tests.rs"]
mod tests;
