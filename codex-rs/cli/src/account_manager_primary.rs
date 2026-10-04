//! Explicit host sign-in selection independent from inference-account actions.

use super::Locale;
use super::clean;
use super::input::prompt;
use codex_app_server::account_management::AccountManagerInventory;
use codex_app_server::account_management::AccountManagerOperation as Operation;

pub(super) fn render(inventory: &AccountManagerInventory, locale: Locale) -> String {
    let mut lines = vec![locale.text("Host sign-in / Remote Control").to_string()];
    if let Some(view) = &inventory.primary_login {
        let label = if view.source == "profile" {
            view.label.as_str()
        } else {
            locale.message(&view.label)
        };
        lines.push(locale.format("Saved source: {}", &[&clean(label)]));
        if let Some(email) = &view.email
            && email != label
        {
            lines.push(locale.format("Email: {}", &[&clean(email)]));
        }
        lines.push(locale.format(
            "Stored credentials: {}",
            &[locale.text(if view.status == "runtimeResolutionRequired" {
                "Resolved by running host"
            } else if view.ready {
                "Available"
            } else {
                "Needs attention"
            })],
        ));
        if let Some(message) = &view.message {
            lines.push(clean(locale.message(message)));
        }
        if let Some(runtime) = view.runtime.as_ref().filter(|runtime| {
            runtime.source_revision == view.revision
                && inventory.host_now.saturating_sub(runtime.observed_at) < 10
                && runtime.observed_at <= inventory.host_now
        }) {
            lines.push(
                locale.format(
                    "Observed host identity: {}",
                    &[&clean(
                        runtime
                            .email
                            .as_deref()
                            .unwrap_or_else(|| locale.text("Not reported")),
                    )],
                ),
            );
            lines.push(locale.format(
                "Remote service: {}",
                &[locale.text(match runtime.remote_status.as_str() {
                    "disabled" => "Disabled",
                    "connecting" => "Connecting",
                    "connected" => "Connected to relay",
                    "errored" => "Connection needs attention",
                    "requirementsDisabled" => "Remote disabled by account requirements",
                    "authenticationDenied" => "Host authentication denied by requirements",
                    _ => "Not reported",
                })],
            ));
        } else {
            lines.push(locale.text("No recent host confirmation for this selection. Stored credentials do not confirm Remote Control is connected.").into());
        }
    }
    lines.push(
        locale
            .text("Host sign-in and inference selection are independent.")
            .into(),
    );
    lines.push(locale.text("The phone must use the matching account and workspace. A new owner may require pairing again.").into());
    lines.push(String::new());
    for (index, account) in inventory.accounts.iter().enumerate() {
        lines.push(locale.format(
            "{} · {} · {}",
            &[
                &(index + 1).to_string(),
                &clean(&account.label),
                locale.text(if account.login_state == "signedIn" {
                    "Signed in"
                } else {
                    "Needs login"
                }),
            ],
        ));
    }
    lines.push(
        locale
            .text("Disabled inference accounts can still be used for host sign-in.")
            .into(),
    );
    lines.push(
        locale
            .text("[number] Choose host account  [R] Root login  [X] Sign out host  [Enter] Back")
            .into(),
    );
    lines.join("\n")
}

pub(super) async fn choose(
    inventory: &AccountManagerInventory,
    locale: Locale,
) -> anyhow::Result<Option<Operation>> {
    let columns = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
    for line in render(inventory, locale).lines() {
        for wrapped in textwrap::wrap(line, columns.max(20)) {
            println!("{wrapped}");
        }
    }
    let choice = prompt(locale, "Host sign-in action", "").await?;
    match choice.trim().to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "r" => Ok(
            (prompt(locale, "Use root login for host sign-in? Type APPLY", "").await? == "APPLY")
                .then_some(Operation::PrimaryRoot),
        ),
        "x" => Ok((prompt(
            locale,
            "Sign out host and disconnect Remote Control? Type SIGNOUT",
            "",
        )
        .await?
            == "SIGNOUT")
            .then_some(Operation::PrimaryLogout)),
        _ => {
            let account = choice
                .parse::<usize>()
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| inventory.accounts.get(index))
                .ok_or_else(|| anyhow::anyhow!(locale.text("Choose a listed account number")))?;
            anyhow::ensure!(
                account.login_state == "signedIn",
                locale.text("Complete account login first")
            );
            println!(
                "{}",
                locale.format("Account: {}", &[&clean(&account.label)])
            );
            println!(
                "{}",
                locale.format("Profile: {}", &[&clean(&account.profile_id)])
            );
            if let Some(email) = &account.email {
                println!("{}", locale.format("Email: {}", &[&clean(email)]));
            }
            Ok(
                (prompt(locale, "Use this account for host sign-in? Type APPLY", "").await?
                    == "APPLY")
                    .then(|| Operation::PrimaryUse {
                        profile_id: account.profile_id.clone(),
                    }),
            )
        }
    }
}

#[cfg(test)]
#[path = "account_manager_primary_tests.rs"]
mod tests;
