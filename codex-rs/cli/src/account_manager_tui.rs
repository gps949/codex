//! A task-independent terminal administration screen backed by the shared manager.

use codex_app_server::account_management::AccountManager;
use codex_app_server::account_management::AccountManagerOperation as Operation;
use std::sync::Arc;

#[path = "account_manager_display.rs"]
mod display;

#[path = "account_manager_decision.rs"]
mod decision;
#[path = "account_manager_reset_journal.rs"]
mod reset_journal;
#[path = "account_manager_routing.rs"]
mod routing;

#[path = "account_manager_actions.rs"]
mod actions;
#[path = "account_manager_credits.rs"]
mod credits;
#[path = "account_manager_input.rs"]
mod input;
#[path = "account_manager_locale.rs"]
mod locale;
#[path = "account_manager_primary.rs"]
mod primary;
#[path = "account_manager_settings.rs"]
mod settings;
#[path = "account_manager_warmup.rs"]
mod warmup;
use input::prompt;
pub(crate) use locale::Locale;

pub(crate) async fn run(manager: Arc<AccountManager>, mut locale: Locale) -> anyhow::Result<()> {
    let mut notice = String::new();
    let mut pending = std::collections::HashMap::new();
    loop {
        let inventory = manager.inventory().await?;
        print!("\x1b[2J\x1b[H");
        let columns = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
        println!("{}", display::render(&inventory, columns, locale));
        for (index, job) in inventory.login_jobs.iter().enumerate() {
            show_login(index + 1, job, locale);
        }
        if !notice.is_empty() {
            println!("\n{}", clean(&notice));
        }
        println!("\n{}", locale.main_menu());
        let input = input::dashboard_prompt(&manager, &inventory.login_jobs, locale).await?;
        if input.eq_ignore_ascii_case("q") {
            break;
        }
        if input.eq_ignore_ascii_case("g") {
            locale = locale.toggle();
            notice = match manager.save_preferences(
                codex_app_server::account_management::ManagerPreferences {
                    language: locale.preference(),
                },
            ) {
                Ok(()) => locale
                    .text("Language saved for browser and terminal managers.")
                    .into(),
                Err(error) => locale.format(
                    "Language changed for this session; saving failed: {}",
                    &[&clean(&error.to_string())],
                ),
            };
            continue;
        }
        let next = async {
            let operation = match input.trim().to_ascii_lowercase().as_str() {
                "" => None,
                "r" => Some(Operation::Refresh { profile_ids: None }),
                "o" => Some(Operation::Automatic),
                "a" => Some(Operation::Login {
                    profile_id: None,
                    label: Some(prompt(locale, "Account label", "").await?),
                }),
                "c" => {
                    let value = prompt(locale, "Login number (Enter returns)", "").await?;
                    if value.is_empty() {
                        None
                    } else {
                        let job = value
                            .parse::<usize>()
                            .ok()
                            .and_then(|index| index.checked_sub(1))
                            .and_then(|index| inventory.login_jobs.get(index))
                            .ok_or_else(|| {
                                anyhow::anyhow!(locale.text("Choose a listed login number"))
                            })?;
                        Some(Operation::CancelLogin {
                            operation_id: job.operation_id.clone(),
                        })
                    }
                }
                "s" => settings::choose(&inventory.settings, locale).await?,
                "d" => {
                    decision::choose(&manager, locale).await?;
                    None
                }
                "m" => {
                    routing::choose(&manager, locale).await?;
                    None
                }
                "j" => reset_journal::choose(&inventory.reset_journals, locale).await?,
                "h" => primary::choose(&inventory, locale).await?,
                "p" => {
                    actions::api_actions(&manager, locale).await?;
                    None
                }
                _ => match input
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| inventory.accounts.get(index))
                {
                    Some(account) => {
                        actions::account_actions(&manager, account, &mut pending, locale).await?
                    }
                    None => {
                        anyhow::bail!(locale.text("Choose an account number or a listed action."))
                    }
                },
            };
            Ok::<_, anyhow::Error>(operation)
        }
        .await;
        let operation = match next {
            Ok(operation) => operation,
            Err(error) => {
                notice = locale.message(&error.to_string()).to_string();
                continue;
            }
        };
        if let Some(operation) = operation {
            notice = match manager.execute(operation).await {
                Ok(result) => locale.notice(&result.message),
                Err(error) => locale.message(&error.to_string()).to_string(),
            };
        }
    }
    Ok(())
}

fn clean(value: &str) -> String {
    display::clean(value)
}

fn show_login(
    index: usize,
    job: &codex_app_server::account_management::LoginProgress,
    locale: Locale,
) {
    let message = if job.status == "completed" {
        locale.format(
            "Signed in: {}",
            &[job.profile_id.as_deref().unwrap_or_default()],
        )
    } else {
        locale.message(&job.message).to_string()
    };
    println!(
        "\n{}",
        locale.format("Login {} · {}", &[&index.to_string(), &clean(&message)])
    );
    if let Some(url) = &job.verification_url {
        println!("{}", locale.format("Open: {}", &[&clean(url)]));
    }
    if let Some(code) = &job.user_code {
        println!(
            "{}",
            locale.format("Verification code: {}", &[&clean(code)])
        );
    }
}
