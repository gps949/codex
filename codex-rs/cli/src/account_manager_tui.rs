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
#[path = "account_manager_settings.rs"]
mod settings;
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
                "s" => settings::choose(&inventory.settings).await?,
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
