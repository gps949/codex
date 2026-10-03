//! A task-independent terminal administration screen backed by the shared manager.

use codex_app_server::account_management::AccountManager;
use codex_app_server::account_management::AccountManagerOperation as Operation;
use codex_app_server::account_management::ManagedAccountView;
use std::io::Write;
use std::sync::Arc;

#[path = "account_manager_display.rs"]
mod display;

pub(crate) async fn run(manager: Arc<AccountManager>) -> anyhow::Result<()> {
    let mut notice = String::new();
    loop {
        let inventory = manager.inventory().await?;
        print!("\x1b[2J\x1b[H");
        let columns = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
        println!("{}", display::render(&inventory, columns));
        for job in &inventory.login_jobs {
            println!(
                "\nLogin {} · {}",
                clean(&job.operation_id),
                clean(&job.message)
            );
            if let Some(url) = &job.verification_url {
                println!("Open: {}", clean(url));
            }
            if let Some(code) = &job.user_code {
                println!("Verification code: {}", clean(code));
            }
        }
        if !notice.is_empty() {
            println!("\n{}", clean(&notice));
        }
        println!(
            "\n[R] Refresh  [A] Add  [S] Settings  [P] API accounts  [O] Automatic subscriptions\n[C] Cancel login  [number] Account actions  [Q] Quit"
        );
        let input = prompt("Choose", "").await?;
        if input.eq_ignore_ascii_case("q") {
            break;
        }
        let next = async {
        let operation = match input.trim().to_ascii_lowercase().as_str() {
            "r" => Some(Operation::Refresh { profile_ids: None }),
            "o" => Some(Operation::Automatic),
            "a" => Some(Operation::Login { profile_id: None, label: Some(prompt("Account label", "").await?) }),
            "c" => Some(Operation::CancelLogin { operation_id: prompt("Login operation ID", "").await? }),
            "s" => {
                println!("\n1 Rotation strategy  2 Early switch percent  3 Return to preferred\n4 Standby warmup  5 Warmup interval  6 Wait for quota recovery\n7 Maximum waiting minutes  8 Automatic reset credits  9 Credit waiting threshold\nEnter returns. J edits advanced JSON.");
                let choice = prompt("Setting", "").await?;
                if choice.is_empty() { None }
                else if choice.eq_ignore_ascii_case("j") {
                    println!("{}", serde_json::to_string_pretty(&inventory.settings)?);
                    let values = prompt("Settings JSON (empty returns)", "").await?;
                    if values.is_empty() { None } else { Some(Operation::Settings {values: serde_json::from_str(&values)?}) }
                } else {
                    let (key, label, fallback) = match choice.as_str() {
                        "1" => ("rotation_strategy", "Strategy: fill_first or earliest_reset", "fill_first"),
                        "2" => ("preemptive_switch_percent", "Early switch at used percent (0 disables)", "95"),
                        "3" => ("return_to_preferred", "Return to preferred account? true/false", "true"),
                        "4" => ("window_warmup", "Warm standby windows? true/false (uses a small amount of quota)", "true"),
                        "5" => ("window_warmup_interval_minutes", "Warmup check interval in minutes", "5"),
                        "6" => ("resume_after_reset", "Wait for quota recovery? true/false", "true"),
                        "7" => ("max_reset_wait_minutes", "Maximum waiting minutes per turn", "360"),
                        "8" => ("auto_reset_credits", "Automatic credits: never or when_pool_exhausted", "never"),
                        "9" => ("auto_reset_credit_min_wait_minutes", "Only spend a credit when the natural reset is farther away (minutes)", "60"),
                        _ => anyhow::bail!("Choose a setting from 1 to 9."),
                    };
                    let current = inventory.settings.get(key).filter(|value| !value.is_null())
                        .map_or_else(|| fallback.into(), |value| value.as_str().map_or_else(|| value.to_string(), str::to_string));
                    let value = prompt(label, &current).await?;
                    let value = if choice == "1" { serde_json::Value::String(value.replace('-', "_")) }
                        else if choice == "8" { serde_json::Value::String(value) }
                        else { serde_json::from_str(&value)? };
                    Some(Operation::Settings {values: serde_json::Value::Object(serde_json::Map::from_iter([(key.into(), value)]))})
                }
            }
            "p" => { api_actions(&manager).await?; None }
            _ => match input.parse::<usize>().ok().and_then(|index| index.checked_sub(1)).and_then(|index| inventory.accounts.get(index)) {
                Some(account) => account_actions(&manager, account).await?,
                None => anyhow::bail!("Choose an account number or a listed action."),
            },
        };
        Ok::<_, anyhow::Error>(operation)
        }.await;
        let operation = match next {
            Ok(operation) => operation,
            Err(error) => {
                notice = error.to_string();
                continue;
            }
        };
        if let Some(operation) = operation {
            notice = match manager.execute(operation).await {
                Ok(result) => result.message,
                Err(error) => error.to_string(),
            };
        }
    }
    Ok(())
}

async fn account_actions(
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

async fn api_actions(manager: &Arc<AccountManager>) -> anyhow::Result<()> {
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

async fn prompt(label: &str, default: &str) -> anyhow::Result<String> {
    print!(
        "{label}{}: ",
        if default.is_empty() {
            String::new()
        } else {
            format!(" [{default}]")
        }
    );
    std::io::stdout().flush()?;
    let default = default.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut value = String::new();
        anyhow::ensure!(
            std::io::stdin().read_line(&mut value)? != 0,
            "Terminal input closed"
        );
        Ok(if value.trim().is_empty() {
            default
        } else {
            value.trim().to_string()
        })
    })
    .await?
}

async fn secret_prompt() -> anyhow::Result<String> {
    print!("API key (hidden): ");
    std::io::stdout().flush()?;
    tokio::task::spawn_blocking(|| {
        crossterm::terminal::enable_raw_mode()?;
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = crossterm::terminal::disable_raw_mode();
            }
        }
        let _restore = Restore;
        let mut result = String::new();
        loop {
            match crossterm::event::read()? {
                crossterm::event::Event::Key(key)
                    if key.kind != crossterm::event::KeyEventKind::Release =>
                {
                    match key.code {
                        crossterm::event::KeyCode::Enter => break,
                        crossterm::event::KeyCode::Esc => anyhow::bail!("Key entry cancelled"),
                        crossterm::event::KeyCode::Char('c' | 'd')
                            if key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            anyhow::bail!("Key entry cancelled")
                        }
                        crossterm::event::KeyCode::Char('u')
                            if key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            result.clear()
                        }
                        crossterm::event::KeyCode::Backspace => {
                            result.pop();
                        }
                        crossterm::event::KeyCode::Char(ch)
                            if !ch.is_control()
                                && !key
                                    .modifiers
                                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            anyhow::ensure!(
                                result.len() + ch.len_utf8() <= 16384,
                                "API key is too long"
                            );
                            result.push(ch);
                        }
                        _ => {}
                    }
                }
                crossterm::event::Event::Paste(paste) => {
                    anyhow::ensure!(
                        result.len() + paste.len() <= 16384 && !paste.chars().any(char::is_control),
                        "Invalid API key paste"
                    );
                    result.push_str(&paste);
                }
                _ => {}
            }
        }
        println!();
        Ok(result)
    })
    .await?
}

fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(240)
        .collect()
}
