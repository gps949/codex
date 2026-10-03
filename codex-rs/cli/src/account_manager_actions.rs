//! Guided account actions for the terminal manager.

use super::clean;
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
) -> anyhow::Result<Option<Operation>> {
    println!(
        "\n{} · {}",
        clean(&account.label),
        clean(account.plan.as_deref().unwrap_or("Unknown plan"))
    );
    println!(
        "Status: {} · Priority: {}",
        super::display::availability(&account.availability),
        account.priority
    );
    println!(
        "Quota percentages show used allowance. Refresh checks the backend without starting a task or spending a credit."
    );
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
                format!("{:.0}% used", window.used_percent.clamp(0.0, 100.0))
            } else {
                "Unknown usage".into()
            };
            println!(
                "{label}: {used} · Duration {} minutes",
                window
                    .window_minutes
                    .map_or_else(|| "unknown".into(), |minutes| minutes.to_string())
            );
            if window.used_percent == 0.0 {
                println!(
                    "  Window start unconfirmed. A reset timestamp alone does not prove warmup completed."
                );
            }
            if let Some(at) = window.resets_at {
                let remaining = at.saturating_sub(chrono::Utc::now().timestamp());
                if remaining > 0 {
                    println!(
                        "  Reported reset in {}h {}m",
                        remaining / 3600,
                        remaining % 3600 / 60
                    );
                } else {
                    println!("  Reset time reached; refresh to confirm current allowance.");
                }
            }
            if let Some(at) = observed {
                let age = chrono::Utc::now().timestamp().saturating_sub(at);
                if age < -300 {
                    println!("  Cached timestamp is ahead of this host's clock; refresh quota.");
                } else {
                    println!("  Cached observation: {} minutes ago", age.max(0) / 60);
                }
            }
        } else {
            println!("{label}: Not checked. R refreshes quota.");
        }
    }
    if let Some(refresh) = &account.refresh {
        println!("Last check: {}", clean(&refresh.message));
    }
    println!(
        "[U] Use  [R] Refresh  [T] Retry after external reset  [C] Reset credits  [L] Relogin  [E] Edit  [D] Enable/disable  [X] Remove  [Enter] Back"
    );
    let id = account.profile_id.clone();
    Ok(
        match prompt("Action", "").await?.to_ascii_lowercase().as_str() {
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
            "e" => Some(Operation::Update {
                profile_id: id,
                label: Some(prompt("Label", &account.label).await?),
                priority: Some(
                    prompt("Priority", &account.priority.to_string())
                        .await?
                        .parse()?,
                ),
                disabled: None,
            }),
            "x" => {
                if prompt("Remove this account? Type REMOVE", "").await? == "REMOVE" {
                    Some(Operation::Remove {
                        profile_id: id,
                        keep_credentials: prompt("Keep local credentials? y/n", "y").await? != "n",
                    })
                } else {
                    None
                }
            }
            "c" => {
                redeem(manager, account, pending).await?;
                None
            }
            _ => None,
        },
    )
}

pub(super) async fn api_actions(manager: &Arc<AccountManager>) -> anyhow::Result<()> {
    let inventory = manager.inventory().await?;
    for (index, account) in inventory.api_accounts.iter().enumerate() {
        println!(
            "{}: {} · {} · {}",
            index + 1,
            clean(&account.account.label),
            clean(&account.account.model),
            if account.account.disabled {
                "Disabled"
            } else if !account.has_key {
                "Key missing"
            } else {
                "Provider billed"
            }
        );
    }
    println!(
        "\nAPI accounts\n[A] Add  [U] Select  [E] Edit  [K] Replace key  [D] Enable/disable  [X] Remove\n[O] Automatic subscriptions  [F] Configure final fallback  [Enter] Back"
    );
    let input = prompt("Action", "").await?.to_ascii_lowercase();
    let operation = match input.as_str() {
        "a" => {
            println!(
                "The endpoint must support Responses. The key is read from stdin without printing it."
            );
            let label = prompt("Label", "").await?;
            let base_url = prompt("HTTPS endpoint", "").await?;
            let model = prompt("Model", "").await?;
            let api_key = secret_prompt().await?;
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
            let index: usize = prompt("API account number", "").await?.parse()?;
            let account = index
                .checked_sub(1)
                .and_then(|index| inventory.api_accounts.get(index))
                .ok_or_else(|| anyhow::anyhow!("Invalid account number"))?;
            if input == "u" {
                Operation::ApiUse {
                    profile_id: account.account.id.clone(),
                }
            } else if input == "k" {
                Operation::ApiReplaceKey {
                    profile_id: account.account.id.clone(),
                    api_key: secret_prompt().await?,
                }
            } else if input == "x" {
                if prompt("Remove this account and its local key? Type REMOVE", "").await?
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
                    updated.label = prompt("Label", &updated.label).await?;
                    updated.base_url = prompt("HTTPS endpoint", &updated.base_url).await?;
                    updated.model = prompt("Model", &updated.model).await?;
                    updated.context_window =
                        prompt("Context window", &updated.context_window.to_string())
                            .await?
                            .parse()?;
                    updated.images = prompt(
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
            println!(
                "Final API fallback incurs provider charges and sends this conversation to that provider."
            );
            let choice = prompt(
                "Type ENABLE to turn on, DISABLE to turn off, or Enter to return",
                "",
            )
            .await?;
            let enabled = match choice.as_str() {
                "ENABLE" => true,
                "DISABLE" => false,
                "" => return Ok(()),
                _ => anyhow::bail!("No change made. Type ENABLE or DISABLE."),
            };
            let profile_id = if enabled {
                let index: usize = prompt("API account number", "").await?.parse()?;
                let account = index
                    .checked_sub(1)
                    .and_then(|index| inventory.api_accounts.get(index))
                    .ok_or_else(|| anyhow::anyhow!("Invalid account number"))?;
                anyhow::ensure!(
                    !account.account.disabled && account.has_key,
                    "Choose an enabled API account with a saved key"
                );
                Some(account.account.id.clone())
            } else {
                None
            };
            let wait_minutes = prompt("Subscription waiting minutes before fallback", "5")
                .await?
                .parse()?;
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
        Ok(result) => println!("{}", result.message),
        Err(error) => println!("{error}"),
    }
    let _ = prompt("Enter to return", "").await?;
    Ok(())
}

/// Retains the exact operation when the backend outcome is not yet confirmed.
pub(super) struct PendingRedemption {
    credit_id: String,
    operation_id: String,
}

async fn redeem(
    manager: &Arc<AccountManager>,
    account: &ManagedAccountView,
    pending: &mut std::collections::HashMap<String, PendingRedemption>,
) -> anyhow::Result<()> {
    let id = &account.profile_id;
    if let Some(operation) = pending.get(id) {
        println!(
            "A previous reset has an unconfirmed outcome. Credit: {} · Operation: {}",
            clean(&operation.credit_id),
            operation.operation_id
        );
        println!(
            "R refreshes quota. RETRY uses the same credit and operation ID. REVIEW clears the pending record only after you check its outcome."
        );
        match prompt("Choose R, RETRY, REVIEW, or Enter to return", "")
            .await?
            .as_str()
        {
            "R" | "r" => {
                let result = manager
                    .execute(Operation::Refresh {
                        profile_ids: Some(vec![id.clone()]),
                    })
                    .await?;
                println!("{}", clean(&result.message));
                let _ = prompt("Enter to return", "").await?;
                return Ok(());
            }
            "REVIEW" => {
                let result = manager
                    .execute(Operation::Credits {
                        profile_id: id.clone(),
                    })
                    .await?;
                let credit = result.data["credits"].as_array().and_then(|credits| {
                    credits
                        .iter()
                        .find(|credit| credit["id"].as_str() == Some(operation.credit_id.as_str()))
                });
                println!(
                    "Original credit status: {}",
                    credit.map_or("No longer listed", |credit| credit["status"]
                        .as_str()
                        .unwrap_or("Unknown"))
                );
                println!(
                    "Clear only after checking the provider's quota and credit history. This forgets the pending record; it does not undo a redemption."
                );
                if prompt("Type REVIEWED to confirm you checked the outcome", "").await?
                    == "REVIEWED"
                {
                    pending.remove(id);
                }
                return Ok(());
            }
            "RETRY" => {}
            _ => return Ok(()),
        }
    } else {
        let result = manager
            .execute(Operation::Credits {
                profile_id: id.clone(),
            })
            .await?;
        let credits = result.data["credits"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Credit list unavailable"))?;
        let available: Vec<_> = credits
            .iter()
            .filter(|credit| {
                credit["status"].as_str() == Some("available")
                    && credit["resetType"].as_str() == Some("codex_rate_limits")
                    && credit["expiresAt"].as_str().is_none_or(|at| {
                        chrono::DateTime::parse_from_rfc3339(at)
                            .is_ok_and(|at| at > chrono::Utc::now())
                    })
            })
            .collect();
        if available.is_empty() {
            println!("No available reset credits. Refresh quota or wait for the natural reset.");
            let _ = prompt("Enter to return", "").await?;
            return Ok(());
        }
        for (index, credit) in available.iter().enumerate() {
            println!(
                "{}: {} · {} · Expires {}",
                index + 1,
                clean(credit["title"].as_str().unwrap_or("Reset credit")),
                clean(credit["resetType"].as_str().unwrap_or("Unknown scope")),
                clean(credit["expiresAt"].as_str().unwrap_or("No expiry reported"))
            );
        }
        let choice = prompt("Credit number (Enter returns)", "").await?;
        if choice.is_empty() {
            return Ok(());
        }
        let credit = choice
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| available.get(index))
            .ok_or_else(|| anyhow::anyhow!("Choose a listed credit number"))?;
        println!(
            "This consumes one reset credit for {}.",
            clean(&account.label)
        );
        if prompt("Type REDEEM to confirm", "").await? != "REDEEM" {
            return Ok(());
        }
        pending.insert(
            id.clone(),
            PendingRedemption {
                credit_id: credit["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Credit ID missing"))?
                    .into(),
                operation_id: format!(
                    "manager-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_nanos()
                ),
            },
        );
    }
    let operation = &pending[id];
    // Show a recoverable identifier before contacting the backend, even if input closes.
    println!("Reset operation: {}", operation.operation_id);
    match manager
        .execute(Operation::Redeem {
            profile_id: id.clone(),
            credit_id: operation.credit_id.clone(),
            idempotency_key: operation.operation_id.clone(),
        })
        .await
    {
        Ok(result) => {
            pending.remove(id);
            println!("{}", clean(&result.message));
        }
        Err(error) => {
            println!(
                "{}\nThe operation ID was retained. Refresh quota before retrying.",
                clean(&error.to_string())
            );
        }
    }
    let _ = prompt("Enter to return", "").await?;
    Ok(())
}
