//! Account actions for the standalone terminal manager.
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
) -> anyhow::Result<Option<Operation>> {
    println!(
        "\n{}\n{}",
        clean(&account.label),
        serde_json::to_string_pretty(account)?
    );
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
                match manager
                    .execute(Operation::Credits {
                        profile_id: id.clone(),
                    })
                    .await
                {
                    Ok(result) => {
                        println!("{}", serde_json::to_string_pretty(&result.data)?);
                        let credit = prompt("Credit ID to redeem (empty returns)", "").await?;
                        if !credit.is_empty()
                            && prompt("Consume one credit for this account? Type REDEEM", "")
                                .await?
                                == "REDEEM"
                        {
                            Some(Operation::Redeem {
                                profile_id: id,
                                credit_id: credit,
                                idempotency_key: format!(
                                    "manager-{}",
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)?
                                        .as_nanos()
                                ),
                            })
                        } else {
                            None
                        }
                    }
                    Err(error) => {
                        println!("{error}");
                        let _ = prompt("Enter to return", "").await?;
                        None
                    }
                }
            }
            _ => None,
        },
    )
}

pub(super) async fn api_actions(manager: &Arc<AccountManager>) -> anyhow::Result<()> {
    let inventory = manager.inventory().await?;
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
            let enabled = prompt("Enable final fallback? Type ENABLE (empty disables)", "").await?
                == "ENABLE";
            let profile_id = if enabled {
                let index: usize = prompt("API account number", "").await?.parse()?;
                Some(
                    index
                        .checked_sub(1)
                        .and_then(|index| inventory.api_accounts.get(index))
                        .ok_or_else(|| anyhow::anyhow!("Invalid account number"))?
                        .account
                        .id
                        .clone(),
                )
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
