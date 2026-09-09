//! Bounded account views for narrow mobile surfaces. Quota values are observations, not balances.

use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AccountPoolAccount;
use codex_app_server_protocol::AccountPoolAvailability;
use codex_app_server_protocol::AccountPoolRateLimitWindow;
use codex_app_server_protocol::AccountPoolReadResponse;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_login::format_primary_window_reset;
use codex_login::format_relative_reset;

pub(crate) fn pool_caption(pool: &AccountPoolReadResponse) -> Option<String> {
    pool.enabled.then(|| {
        let ready = pool
            .accounts
            .iter()
            .filter(|account| matches!(account.availability, AccountPoolAvailability::Available))
            .count();
        let name = pool
            .accounts
            .iter()
            .find(|account| account.is_active)
            .map(|account| compact_label(&label(account), 16))
            .unwrap_or_else(|| "Codex".into());
        format!("{name} · {ready}/{} ready", pool.accounts.len())
    })
}

pub(crate) fn account_caption(pool: &AccountPoolReadResponse) -> String {
    pool_caption(pool).unwrap_or_else(|| "Account pool is not configured.".into())
}

pub(crate) fn label(account: &AccountPoolAccount) -> String {
    compact_label(
        account
            .label
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .or(account.email.as_deref())
            .unwrap_or("Account"),
        32,
    )
}

pub(crate) fn compact_label(text: &str, max: usize) -> String {
    let mut chars = text
        .chars()
        .map(|character| match character {
            '\n' | '\r' | '\t' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '\\' => ' ',
            other => other,
        })
        .filter(|character| {
            !character.is_control()
                && !matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        });
    let mut label: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        label.push('…');
    }
    label
}

pub(crate) fn overlay_snapshot(snapshot: &mut RateLimitSnapshot, caption: &str) {
    if snapshot.limit_id.as_deref().is_none_or(|id| id == "codex") {
        snapshot.limit_name = Some(caption.to_string());
    }
}

pub(crate) fn resolve<'a>(
    pool: &'a AccountPoolReadResponse,
    selector: &str,
) -> Result<&'a AccountPoolAccount, String> {
    let selector = selector.trim();
    let selector = if let Some(quote @ ('"' | '\'')) = selector.chars().next() {
        selector
            .strip_prefix(quote)
            .and_then(|value| value.strip_suffix(quote))
            .ok_or("Close the quoted account name.")?
    } else {
        selector
    };
    if selector.is_empty() {
        return Err("Specify an account: /account use <label>".into());
    }
    if let Some(account) = pool
        .accounts
        .iter()
        .find(|account| account.profile_id == selector)
    {
        return Ok(account);
    }
    let mut matches = pool.accounts.iter().filter(|account| {
        account.label.as_deref() == Some(selector)
            || account.email.as_deref() == Some(selector)
            || label(account) == selector
    });
    match (matches.next(), matches.next()) {
        (Some(account), None) => Ok(account),
        (None, _) => Err("Account not found. Use /account to see available names.".into()),
        (Some(_), Some(_)) => Err("Ambiguous account name. Use its exact profile ID from `codex account list` on the host.".into()),
    }
}

pub(crate) fn list(pool: &AccountPoolReadResponse, page: usize) -> Result<String, String> {
    const PAGE_SIZE: usize = 4;
    let pages = pool.accounts.len().div_ceil(PAGE_SIZE).max(1);
    if page == 0 || page > pages {
        return Err(format!(
            "Page out of range. Use /account list 1 through {pages}."
        ));
    }
    let mut accounts: Vec<_> = pool.accounts.iter().collect();
    accounts.sort_by_key(|account| (!account.is_active, account.priority, &account.profile_id));
    let mut lines = vec![format!("Accounts · {page}/{pages}")];
    for account in accounts
        .into_iter()
        .skip((page - 1) * PAGE_SIZE)
        .take(PAGE_SIZE)
    {
        let name = label(account);
        let state = availability(account);
        let current = if account.is_active { " · Current" } else { "" };
        lines.push(format!("\n{name}{current}{state}"));
        let primary = usage(account.rate_limits.primary.as_ref());
        let secondary = usage(account.rate_limits.secondary.as_ref());
        let cached = if account
            .rate_limits
            .observed_at
            .is_none_or(|time| chrono::Utc::now().timestamp().saturating_sub(time) > 120)
        {
            " · cached"
        } else {
            ""
        };
        lines.push(format!(
            "Used: primary {primary} · secondary {secondary}{cached}"
        ));
    }
    if page < pages {
        lines.push(format!("\nNext: /account list {}", page + 1));
    }
    lines.push("\nDetails: /account show <name>\nControls: /account help".into());
    Ok(lines.join("\n"))
}

pub(crate) fn detail(pool: &AccountPoolReadResponse, selector: &str) -> Result<String, String> {
    let account = resolve(pool, selector)?;
    let name = label(account);
    let state = availability(account);
    let current = if account.is_active { " · Current" } else { "" };
    let mut lines = vec![format!("{name}{current}{state}")];
    let now = Utc::now();
    for (name, window, primary) in [
        ("Primary", account.rate_limits.primary.as_ref(), true),
        ("Secondary", account.rate_limits.secondary.as_ref(), false),
    ] {
        lines.push(format!(
            "{name}: {} used\nReset: {}",
            usage(window),
            reset_label(window, primary, now)
        ));
    }
    lines.push(format!(
        "Checked: {}",
        timestamp(account.rate_limits.observed_at)
    ));
    lines.push("Cached values remain if refresh fails.".into());
    Ok(lines.join("\n"))
}

fn usage(window: Option<&AccountPoolRateLimitWindow>) -> String {
    window
        .filter(|window| window.used_percent.is_finite())
        .map(|window| format!("{:.0}%", window.used_percent.clamp(0.0, 100.0)))
        .unwrap_or_else(|| "unknown".into())
}

fn availability(account: &AccountPoolAccount) -> String {
    match &account.availability {
        AccountPoolAvailability::Available => String::new(),
        AccountPoolAvailability::Exhausted { resets_at } => match resets_at {
            Some(resets_at) => match DateTime::<Utc>::from_timestamp(*resets_at, 0) {
                Some(reset) => format!(
                    " · Cooling down {}",
                    format_relative_reset(reset, Utc::now())
                ),
                None => " · Cooling down".into(),
            },
            None => " · Cooling down".into(),
        },
        AccountPoolAvailability::Disabled => " · Disabled".into(),
        AccountPoolAvailability::AuthenticationUnavailable { .. } => " · Login required".into(),
    }
}

fn reset_label(
    window: Option<&AccountPoolRateLimitWindow>,
    primary: bool,
    now: DateTime<Utc>,
) -> String {
    let Some(window) = window else {
        return "unknown".into();
    };
    if primary && window.used_percent <= 0.0 {
        return format_primary_window_reset(window.used_percent, window.resets_at, now);
    }
    timestamp(window.resets_at)
}

fn timestamp(value: Option<i64>) -> String {
    value
        .and_then(|value| DateTime::from_timestamp(value, 0))
        .map(|time| time.format("%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
#[path = "mobile_account_status_tests.rs"]
mod tests;
