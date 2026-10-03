//! Readable terminal inventory shared by the standalone account manager.

use codex_app_server::account_management::AccountManagerInventory;
use unicode_width::UnicodeWidthStr;

pub(super) fn render(inventory: &AccountManagerInventory, columns: usize) -> String {
    let mut rows = vec!["Codex Accounts".to_string(), String::new()];
    let target = match &inventory.api_selection {
        codex_login::ApiAccountSelection::Subscription => "Automatic subscriptions".into(),
        codex_login::ApiAccountSelection::Manual { profile_id } => inventory
            .api_accounts
            .iter()
            .find(|account| &account.account.id == profile_id)
            .map_or_else(
                || "API target unavailable".into(),
                |account| {
                    format!(
                        "Manual API: {} (provider billed)",
                        clean(&account.account.label)
                    )
                },
            ),
    };
    rows.push(format!("Target: {target}"));
    rows.push(format!(
        "Subscription pool: {}",
        if inventory.paused {
            "Paused"
        } else {
            "Enabled"
        }
    ));
    let ready = inventory
        .accounts
        .iter()
        .filter(|account| account.availability == "ready")
        .count();
    let checking = inventory
        .accounts
        .iter()
        .filter(|account| {
            account
                .refresh
                .as_ref()
                .is_some_and(|refresh| refresh.in_progress)
        })
        .count();
    rows.push(format!(
        "{} subscription accounts · {ready} ready · {checking} checking",
        inventory.accounts.len()
    ));
    rows.push("Quota percentages show USED allowance; ? means not checked.".into());
    rows.push(format!(
        "Next: {}",
        if matches!(
            inventory.api_selection,
            codex_login::ApiAccountSelection::Manual { .. }
        ) {
            "O returns to automatic subscriptions. API requests are provider billed."
        } else if inventory.paused {
            "O resumes subscription selection."
        } else if inventory.accounts.is_empty() {
            "A adds your first subscription account."
        } else if ready == 0
            && inventory
                .accounts
                .iter()
                .any(|account| account.login_state != "signedIn")
        {
            "Choose an account number, then L to finish login."
        } else if ready == 0 {
            "R checks for restored quota without spending a reset credit."
        } else {
            "Choose an account number to view quota, credits and actions."
        }
    ));
    rows.push(String::new());
    let wide = columns >= 86;
    if wide {
        rows.push(
            "     ACCOUNT                    PLAN       AVAILABILITY     PRIMARY  WEEKLY   CREDITS"
                .into(),
        );
    }
    for (index, account) in inventory.accounts.iter().enumerate() {
        let marker = if matches!(
            inventory.api_selection,
            codex_login::ApiAccountSelection::Subscription
        ) && inventory.active_profile_id.as_deref()
            == Some(account.profile_id.as_str())
        {
            '*'
        } else {
            ' '
        };
        let primary = percent(account.rate_limits.primary.as_ref());
        let weekly = percent(account.rate_limits.secondary.as_ref());
        let credits = account
            .reset_credit_count
            .map_or_else(|| "?".into(), |count| count.to_string());
        if wide {
            rows.push(format!(
                "{marker}{:>3} {} {} {} {:>7} {:>7} {:>8}",
                index + 1,
                cell(&account.label, 26),
                cell(account.plan.as_deref().unwrap_or("Unknown"), 10),
                cell(availability(&account.availability), 15),
                primary,
                weekly,
                credits
            ));
        } else {
            rows.push(format!(
                "{marker}{:>3} {}",
                index + 1,
                clean(&account.label)
            ));
            rows.push(format!(
                "     {} · {}",
                clean(account.plan.as_deref().unwrap_or("Unknown plan")),
                availability(&account.availability)
            ));
            rows.push(format!("     Primary {primary:>4} · Weekly {weekly:>4}"));
            rows.push(format!("     Reset credits {credits}"));
        }
        if let Some(refresh) = &account.refresh {
            if refresh.in_progress {
                rows.push("     Checking fresh quota…".into());
            } else if !refresh.succeeded {
                rows.push(format!("     Check failed: {}", clean(&refresh.message)));
            }
        }
    }
    if inventory.accounts.is_empty() {
        rows.push("No subscription accounts. Choose Add to begin.".into());
    }
    rows.push(String::new());
    rows.push(format!(
        "API accounts · fallback {}",
        if inventory.api_fallback.enabled {
            "explicitly enabled"
        } else {
            "off"
        }
    ));
    for (index, account) in inventory.api_accounts.iter().enumerate() {
        rows.push(format!(
            "  {:>3} {} · {} · {}",
            index + 1,
            clean(&account.account.label),
            clean(&account.account.model),
            if account.account.disabled {
                "disabled"
            } else if !account.has_key {
                "key missing"
            } else {
                "provider billed"
            }
        ));
    }
    rows.into_iter()
        .flat_map(|row| {
            textwrap::wrap(&row, columns.max(20))
                .into_iter()
                .map(std::borrow::Cow::into_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn percent(
    window: Option<&codex_app_server::account_management::ManagedRateLimitWindow>,
) -> String {
    window
        .filter(|window| window.used_percent.is_finite())
        .map_or_else(
            || "?".into(),
            |window| format!("{:.0}%", window.used_percent.clamp(0.0, 100.0)),
        )
}

fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|ch| {
            !ch.is_control() && !matches!(*ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(240)
        .collect()
}

fn cell(value: &str, width: usize) -> String {
    let mut text = clean(value);
    if UnicodeWidthStr::width(text.as_str()) > width {
        while UnicodeWidthStr::width(text.as_str()) >= width {
            text.pop();
        }
        text.push('…');
    }
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(UnicodeWidthStr::width(text.as_str())))
    )
}

#[cfg(test)]
#[path = "account_manager_display_tests.rs"]
mod tests;

pub(super) fn availability(value: &str) -> &str {
    match value {
        "ready" => "Ready",
        "coolingDown" => "Waiting reset",
        "needsLogin" => "Needs login",
        "disabled" => "Disabled",
        "paused" => "Paused",
        _ => "Check status",
    }
}
