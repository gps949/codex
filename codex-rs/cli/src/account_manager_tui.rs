//! A task-independent terminal administration screen backed by the shared manager.

use codex_app_server::account_management::AccountManager;
use codex_app_server::account_management::AccountManagerOperation as Operation;
use std::sync::Arc;

#[path = "account_manager_display.rs"]
mod display;

#[path = "account_manager_actions.rs"]
mod actions;
#[path = "account_manager_input.rs"]
mod input;
use input::prompt;

pub(crate) async fn run(manager: Arc<AccountManager>) -> anyhow::Result<()> {
    let mut notice = String::new();
    let mut pending = std::collections::HashMap::new();
    loop {
        let inventory = manager.inventory().await?;
        print!("\x1b[2J\x1b[H");
        let columns = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
        println!("{}", display::render(&inventory, columns));
        for (index, job) in inventory.login_jobs.iter().enumerate() {
            show_login(index + 1, job);
        }
        if !notice.is_empty() {
            println!("\n{}", clean(&notice));
        }
        println!(
            "\n[R] Refresh  [A] Add  [S] Settings  [P] API accounts  [O] Automatic subscriptions\n[C] Cancel login  [number] Account actions  [Q] Quit"
        );
        let input = input::dashboard_prompt(&manager, &inventory.login_jobs).await?;
        if input.eq_ignore_ascii_case("q") {
            break;
        }
        let next = async {
            let operation = match input.trim().to_ascii_lowercase().as_str() {
                "" => None,
                "r" => Some(Operation::Refresh { profile_ids: None }),
                "o" => Some(Operation::Automatic),
                "a" => Some(Operation::Login {
                    profile_id: None,
                    label: Some(prompt("Account label", "").await?),
                }),
                "c" => {
                    let value = prompt("Login number (Enter returns)", "").await?;
                    if value.is_empty() {
                        None
                    } else {
                        let job = value
                            .parse::<usize>()
                            .ok()
                            .and_then(|index| index.checked_sub(1))
                            .and_then(|index| inventory.login_jobs.get(index))
                            .ok_or_else(|| anyhow::anyhow!("Choose a listed login number"))?;
                        Some(Operation::CancelLogin {
                            operation_id: job.operation_id.clone(),
                        })
                    }
                }
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
                "p" => {
                    actions::api_actions(&manager).await?;
                    None
                }
                _ => match input
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| inventory.accounts.get(index))
                {
                    Some(account) => {
                        actions::account_actions(&manager, account, &mut pending).await?
                    }
                    None => anyhow::bail!("Choose an account number or a listed action."),
                },
            };
            Ok::<_, anyhow::Error>(operation)
        }
        .await;
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

fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|ch| {
            !ch.is_control() && !matches!(*ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(240)
        .collect()
}

fn show_login(index: usize, job: &codex_app_server::account_management::LoginProgress) {
    println!("\nLogin {index} · {}", clean(&job.message));
    if let Some(url) = &job.verification_url {
        println!("Open: {}", clean(url));
    }
    if let Some(code) = &job.user_code {
        println!("Verification code: {}", clean(code));
    }
}
