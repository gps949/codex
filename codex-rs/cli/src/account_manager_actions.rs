//! Guided account actions for the terminal manager.

use super::Locale;
use super::clean;
use super::credits;
use super::credits::PendingRedemption;
use super::input::prompt;
use super::input::secret_prompt;

use codex_app_server::account_management::AccountManager;
use codex_app_server::account_management::AccountManagerOperation as Operation;
use codex_app_server::account_management::ManagedAccountView;
use std::sync::Arc;

pub(super) async fn account_actions(
    manager: &Arc<AccountManager>,
    account: &ManagedAccountView,
    pending: &mut std::collections::HashMap<String, PendingRedemption>,
    locale: Locale,
) -> anyhow::Result<Option<Operation>> {
    println!(
        "\n{} · {}",
        clean(&account.label),
        clean(
            account
                .plan
                .as_deref()
                .unwrap_or_else(|| locale.text("Unknown plan"))
        )
    );
    println!(
        "{}",
        locale.format("Profile: {}", &[&clean(&account.profile_id)])
    );
    if let Some(email) = &account.email
        && email != &account.label
    {
        println!("{}", locale.format("Email: {}", &[&clean(email)]));
    }
    println!(
        "{}",
        locale.text(
            "E edits a custom name; Enter keeps it. N clears it and uses email, then profile ID."
        )
    );
    println!(
        "{}",
        locale.format(
            "Status: {} · Priority: {}",
            &[
                super::display::availability(&account.availability, locale),
                &account.priority.to_string()
            ]
        )
    );
    println!("{}", locale.text(
        "Quota percentages show used allowance. Refresh checks the backend without starting a task or spending a credit."
    ));
    for (label, window, observed) in [
        (
            "Short window",
            account.rate_limits.primary.as_ref(),
            account.rate_limits.primary_observed_at,
        ),
        (
            "Long window",
            account.rate_limits.secondary.as_ref(),
            account.rate_limits.secondary_observed_at,
        ),
    ] {
        if let Some(window) = window {
            let used = if window.used_percent.is_finite() {
                locale.format(
                    "{}% used",
                    &[&format!("{:.0}", window.used_percent.clamp(0.0, 100.0))],
                )
            } else {
                locale.text("Unknown usage").into()
            };
            println!(
                "{}",
                locale.format(
                    "{}: {} · Duration {} minutes",
                    &[
                        locale.text(label),
                        &used,
                        &window.window_minutes.map_or_else(
                            || locale.text("unknown").into(),
                            |minutes| minutes.to_string()
                        )
                    ]
                )
            );
            if window.used_percent == 0.0 {
                println!("  {}", locale.text("Window start unconfirmed. A reset timestamp alone does not prove warmup completed."));
            }
            if let Some(at) = window.resets_at {
                let remaining = at.saturating_sub(chrono::Utc::now().timestamp());
                if remaining > 0 {
                    println!(
                        "  {}",
                        locale.format(
                            "Reported reset in {}h {}m",
                            &[
                                &(remaining / 3600).to_string(),
                                &(remaining % 3600 / 60).to_string()
                            ]
                        )
                    );
                } else {
                    println!(
                        "  {}",
                        locale.text("Reset time reached; refresh to confirm current allowance.")
                    );
                }
            }
            if let Some(at) = observed {
                let age = chrono::Utc::now().timestamp().saturating_sub(at);
                if age < -300 {
                    println!(
                        "  {}",
                        locale
                            .text("Cached timestamp is ahead of this host's clock; refresh quota.")
                    );
                } else {
                    println!(
                        "  {}",
                        locale.format(
                            "Cached observation: {} minutes ago",
                            &[&(age.max(0) / 60).to_string()]
                        )
                    );
                }
            }
        } else {
            println!(
                "{}",
                locale.format("{}: Not checked. R refreshes quota.", &[locale.text(label)])
            );
        }
    }
    if let Some(refresh) = &account.refresh {
        println!(
            "{}",
            locale.format(
                "Last check: {}",
                &[&clean(locale.message(&refresh.message))]
            )
        );
    }
    println!("{}", locale.text(
        "[U] Use  [H] Host sign-in  [R] Refresh  [T] Retry after external reset  [C] Reset credits  [W] Warmup details  [L] Relogin  [E] Edit  [N] Automatic name  [D] Enable/disable  [X] Remove  [Enter] Back"
    ));
    let id = account.profile_id.clone();
    Ok(
        match prompt(locale, "Action", "")
            .await?
            .to_ascii_lowercase()
            .as_str()
        {
            "h" => {
                if prompt(locale, "Use this account for host sign-in? Type APPLY", "").await?
                    == "APPLY"
                {
                    Some(Operation::PrimaryUse { profile_id: id })
                } else {
                    None
                }
            }
            "u" => Some(Operation::Use { profile_id: id }),
            "r" => Some(Operation::Refresh {
                profile_ids: Some(vec![id]),
            }),
            "t" => Some(Operation::Retry { profile_id: id }),
            "l" => Some(Operation::Login {
                profile_id: Some(id),
                label: None,
            }),
            "d" => Some(Operation::Update {
                profile_id: id,
                label: None,
                priority: None,
                disabled: Some(!account.disabled),
            }),
            "e" => {
                let default = account.custom_label.as_deref().unwrap_or_default();
                let entered = prompt(
                    locale,
                    "Custom label (Enter keeps the current name)",
                    default,
                )
                .await?;
                Some(Operation::Update {
                    profile_id: id,
                    label: (entered != default).then_some(entered),
                    priority: Some(
                        prompt(locale, "Priority", &account.priority.to_string())
                            .await?
                            .parse()
                            .map_err(|_| anyhow::anyhow!(locale.text("Enter a whole number")))?,
                    ),
                    disabled: None,
                })
            }
            "n" => Some(Operation::Update {
                profile_id: id,
                label: Some(String::new()),
                priority: None,
                disabled: None,
            }),
            "x" => {
                if prompt(locale, "Remove this account? Type REMOVE", "").await? == "REMOVE" {
                    Some(Operation::Remove {
                        profile_id: id,
                        keep_credentials: prompt(locale, "Keep local credentials? y/n", "y")
                            .await?
                            != "n",
                    })
                } else {
                    None
                }
            }
            "c" => {
                credits::redeem(manager, account, pending, locale).await?;
                None
            }
            "w" => {
                let columns =
                    crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
                for line in super::warmup::render(account, locale).lines() {
                    for wrapped in textwrap::wrap(line, columns.max(20)) {
                        println!("{wrapped}");
                    }
                }
                prompt(locale, "Enter to return", "").await?;
                None
            }
            _ => None,
        },
    )
}

pub(super) async fn api_actions(
    manager: &Arc<AccountManager>,
    locale: Locale,
) -> anyhow::Result<()> {
    let inventory = manager.inventory().await?;
    for (index, account) in inventory.api_accounts.iter().enumerate() {
        println!(
            "{}: {} · {} · {}",
            index + 1,
            clean(&account.account.label),
            clean(&account.account.model),
            if account.account.disabled {
                locale.text("Disabled")
            } else if !account.has_key {
                locale.text("Key missing")
            } else {
                locale.text("Provider billed")
            }
        );
    }
    println!("\n{}", locale.text("API accounts\n[A] Add  [U] Select  [E] Edit  [K] Replace key  [D] Enable/disable  [X] Remove\n[O] Automatic subscriptions  [F] Configure final fallback  [Enter] Back"));
    let input = prompt(locale, "Action", "").await?.to_ascii_lowercase();
    let operation = match input.as_str() {
        "a" => {
            println!("{}", locale.text(
                "The endpoint must support Responses. The key is read from stdin without printing it."
            ));
            let label = prompt(locale, "Label", "").await?;
            let base_url = prompt(locale, "HTTPS endpoint", "").await?;
            let model = prompt(locale, "Model", "").await?;
            let api_key = secret_prompt(locale).await?;
            Operation::ApiAdd {
                label,
                base_url,
                model,
                api_key,
                context_window: Some(32768),
                images: Some(false),
            }
        }
        "o" => Operation::Automatic,
        "u" | "x" | "e" | "d" | "k" => {
            let index: usize = prompt(locale, "API account number", "")
                .await?
                .parse()
                .map_err(|_| anyhow::anyhow!(locale.text("Enter a whole number")))?;
            let account = index
                .checked_sub(1)
                .and_then(|index| inventory.api_accounts.get(index))
                .ok_or_else(|| anyhow::anyhow!(locale.text("Invalid account number")))?;
            if input == "u" {
                Operation::ApiUse {
                    profile_id: account.account.id.clone(),
                }
            } else if input == "k" {
                Operation::ApiReplaceKey {
                    profile_id: account.account.id.clone(),
                    api_key: secret_prompt(locale).await?,
                }
            } else if input == "x" {
                if prompt(
                    locale,
                    "Remove this account and its local key? Type REMOVE",
                    "",
                )
                .await?
                    != "REMOVE"
                {
                    return Ok(());
                }
                Operation::ApiRemove {
                    profile_id: account.account.id.clone(),
                }
            } else {
                let mut updated = account.account.clone();
                if input == "d" {
                    updated.disabled = !updated.disabled;
                } else {
                    updated.label = prompt(locale, "Label", &updated.label).await?;
                    updated.base_url = prompt(locale, "HTTPS endpoint", &updated.base_url).await?;
                    updated.model = prompt(locale, "Model", &updated.model).await?;
                    updated.context_window = prompt(
                        locale,
                        "Context window",
                        &updated.context_window.to_string(),
                    )
                    .await?
                    .parse()
                    .map_err(|_| anyhow::anyhow!(locale.text("Enter a whole number")))?;
                    updated.images = prompt(
                        locale,
                        "Image input supported? y/n",
                        if updated.images { "y" } else { "n" },
                    )
                    .await?
                        == "y";
                }
                Operation::ApiUpdate { account: updated }
            }
        }
        "f" => {
            println!("{}", locale.text(
                "Final API fallback incurs provider charges and sends this conversation to that provider."
            ));
            let choice = prompt(
                locale,
                "Type ENABLE to turn on, DISABLE to turn off, or Enter to return",
                "",
            )
            .await?;
            let enabled = match choice.as_str() {
                "ENABLE" => true,
                "DISABLE" => false,
                "" => return Ok(()),
                _ => anyhow::bail!(locale.text("No change made. Type ENABLE or DISABLE.")),
            };
            let profile_id = if enabled {
                let index: usize = prompt(locale, "API account number", "")
                    .await?
                    .parse()
                    .map_err(|_| anyhow::anyhow!(locale.text("Enter a whole number")))?;
                let account = index
                    .checked_sub(1)
                    .and_then(|index| inventory.api_accounts.get(index))
                    .ok_or_else(|| anyhow::anyhow!(locale.text("Invalid account number")))?;
                anyhow::ensure!(
                    !account.account.disabled && account.has_key,
                    locale.text("Choose an enabled API account with a saved key")
                );
                Some(account.account.id.clone())
            } else {
                None
            };
            let wait_minutes = prompt(locale, "Subscription waiting minutes before fallback", "5")
                .await?
                .parse()
                .map_err(|_| anyhow::anyhow!(locale.text("Enter a whole number")))?;
            Operation::ApiFallback {
                config: codex_login::ApiAccountFallback {
                    enabled,
                    profile_id,
                    wait_minutes,
                },
            }
        }
        _ => return Ok(()),
    };
    match manager.execute(operation).await {
        Ok(result) => println!("{}", clean(locale.message(&result.message))),
        Err(error) => println!("{}", clean(locale.message(&error.to_string()))),
    }
    let _ = prompt(locale, "Enter to return", "").await?;
    Ok(())
}
