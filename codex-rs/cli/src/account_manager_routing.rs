//! Task-model policy controls, independent from tool ranking and service setup.

use super::Locale;
use super::clean;
use super::decision;
use super::input::prompt;
use codex_app_server::account_management::AccountManager;
use codex_app_server::account_management::AccountManagerOperation;
use codex_app_server::account_management::ModelRoutingView;
use codex_config::ModelRoutingConfigToml;
use codex_config::ModelRoutingMode;
use codex_config::ModelRoutingRole;
use codex_config::ModelRoutingSource;
use std::sync::Arc;

pub(super) fn render(view: &ModelRoutingView, locale: Locale) -> String {
    let config = &view.config;
    let enabled = |value| locale.text(if value { "On" } else { "Off" });
    let mode = |mode| {
        locale.text(match mode {
            ModelRoutingMode::Off => "Off",
            ModelRoutingMode::Preview => "Preview only",
            ModelRoutingMode::Automatic => "Automatic model selection",
        })
    };
    let mut lines = vec![
        locale.text("Task model selection").into(),
        locale.format("Saved mode: {}", &[mode(config.mode)]),
        locale.format("Source: {}", &[locale.text(match config.source {
            ModelRoutingSource::Local => "Local rules",
            ModelRoutingSource::DecisionService => "Jev / Clef service",
        })]),
        locale.format("Main tasks: {} · Subagents: {}", &[enabled(config.main_tasks), enabled(config.subagents)]),
        locale.format("Balance: {} / 100 · Highest effort: {}", &[&config.preference.to_string(), &config.max_effort]),
        locale.text("0 favors endurance; 100 favors capability. Model roles are relative preferences, not measured quota costs.").into(),
    ];
    if config.source == ModelRoutingSource::DecisionService {
        lines.push(
            locale
                .text(if view.decision_service_ready {
                    "Decision service ready; task-text consent is separate from tool assistance."
                } else {
                    "Decision service missing; use D to configure Jev / Clef first."
                })
                .into(),
        );
        lines.push(locale.format(
            "Task-text consent: {} · Local fallback: {}",
            &[
                enabled(config.send_task_description),
                enabled(config.local_fallback),
            ],
        ));
    }
    if view.overridden {
        lines.push(locale.format(
            "Effective mode: {} · Higher-priority settings override the saved policy.",
            &[mode(view.effective_config.mode)],
        ));
    }
    if let Some(last) = &view.last_decision
        && let Some(model) = last["model"].as_str()
    {
        let status = if last["applied"].as_bool() == Some(true)
            || last["status"].as_str() == Some("applied")
        {
            "Last applied choice: {} · {}"
        } else {
            "Last recorded suggestion: {} · {}"
        };
        lines.push(locale.format(
            status,
            &[&clean(model), last["effort"].as_str().unwrap_or("default")],
        ));
    }
    lines.push(locale.text("New tasks read this policy. Manual model choices and inherited full-history children keep their model.").into());
    lines.push(locale.text("Preview records a suggestion; Automatic applies a validated choice. Local simulation sends no requests.").into());
    lines.push(locale.text("[0] Off  [1] Preview  [2] Automatic\n[S] Source  [T] Main tasks  [A] Subagents  [B] Balance  [E] Effort\n[M] Models / roles  [P] Local simulation  [F] Local fallback  [D] Jev / Clef  [Enter] Back").into());
    lines.join("\n")
}

fn save_operation(
    view: &ModelRoutingView,
    config: ModelRoutingConfigToml,
) -> anyhow::Result<AccountManagerOperation> {
    config.validate()?;
    anyhow::ensure!(
        config.mode == ModelRoutingMode::Off
            || config.source == ModelRoutingSource::Local
            || view.decision_service_ready,
        "Configure Jev / Clef before enabling decision-service model selection."
    );
    Ok(AccountManagerOperation::RoutingSave {
        config,
        expected_version: Some(view.user_config_version.clone()),
    })
}

pub(super) async fn choose(manager: &Arc<AccountManager>, locale: Locale) -> anyhow::Result<()> {
    let mut notice = String::new();
    loop {
        let view = manager.model_routing_view().await?;
        let columns = crossterm::terminal::size().map_or(80, |(width, _)| usize::from(width));
        print!("\x1b[2J\x1b[H");
        for line in render(&view, locale).lines() {
            for wrapped in textwrap::wrap(line, columns.max(20)) {
                println!("{wrapped}");
            }
        }
        if !notice.is_empty() {
            println!("\n{}", clean(&notice));
        }
        let choice = prompt(locale, "Model selection action", "").await?;
        let mut config = view.config.clone();
        let operation = match choice.to_ascii_lowercase().as_str() {
            "" => return Ok(()),
            "0" => {
                config.mode = ModelRoutingMode::Off;
                None
            }
            "1" => {
                config.mode = ModelRoutingMode::Preview;
                None
            }
            "2" => {
                config.mode = ModelRoutingMode::Automatic;
                None
            }
            "t" => {
                config.main_tasks = !config.main_tasks;
                None
            }
            "a" => {
                config.subagents = !config.subagents;
                None
            }
            "f" => {
                config.local_fallback = !config.local_fallback;
                None
            }
            "b" => {
                let value =
                    prompt(locale, "Balance (0 to 100)", &config.preference.to_string()).await?;
                let Some(preference) = value.parse::<u8>().ok().filter(|value| *value <= 100)
                else {
                    notice = locale.text("Enter a whole number from 0 to 100").into();
                    continue;
                };
                config.preference = preference;
                None
            }
            "e" => {
                println!(
                    "{}",
                    locale.text("1 minimal  2 low  3 medium  4 high  5 xhigh  6 max")
                );
                let value = prompt(locale, "Effort number (Enter returns)", "").await?;
                if value.is_empty() {
                    continue;
                }
                config.max_effort = match value.as_str() {
                    "1" => "minimal",
                    "2" => "low",
                    "3" => "medium",
                    "4" => "high",
                    "5" => "xhigh",
                    "6" => "max",
                    _ => {
                        notice = locale.text("Choose a listed number").into();
                        continue;
                    }
                }
                .into();
                None
            }
            "s" => {
                println!("{}", locale.text("1 Local rules  2 Jev / Clef service"));
                match prompt(locale, "Source number (Enter returns)", "")
                    .await?
                    .as_str()
                {
                    "" => continue,
                    "1" => {
                        config.source = ModelRoutingSource::Local;
                        config.send_task_description = false;
                    }
                    "2" => {
                        if !view.decision_service_ready {
                            notice = locale.text("Decision service missing; use D to configure Jev / Clef first.").into();
                            continue;
                        }
                        config.source = ModelRoutingSource::DecisionService;
                        config.send_task_description = false;
                    }
                    _ => {
                        notice = locale.text("Choose a listed number").into();
                        continue;
                    }
                }
                None
            }
            "m" => {
                match model_settings(manager, &view, &config, locale).await {
                    Ok(Some(next)) => config = next,
                    Ok(None) => continue,
                    Err(error) => {
                        notice = locale.message(&error.to_string()).into();
                        continue;
                    }
                }
                None
            }
            "p" => {
                println!(
                    "{}",
                    locale
                        .text("Local simulation uses no history and sends nothing to Jev / Clef.")
                );
                let task = prompt(locale, "Example task (Enter returns)", "").await?;
                if task.is_empty() {
                    continue;
                }
                Some(AccountManagerOperation::RoutingPreview {
                    task,
                    config: config.clone(),
                })
            }
            "d" => {
                decision::choose(manager, locale).await?;
                continue;
            }
            _ => {
                notice = locale.text("Choose a listed action").into();
                continue;
            }
        };
        let preview = operation.is_some();
        if !preview
            && config.mode != ModelRoutingMode::Off
            && config.source == ModelRoutingSource::DecisionService
            && !config.send_task_description
        {
            if !view.decision_service_ready {
                notice = locale
                    .text("Decision service missing; use D to configure Jev / Clef first.")
                    .into();
                continue;
            }
            println!("{}", locale.text("This service receives up to 2048 bytes of each eligible task description and may charge separately. No history, source files, account details or quota is sent."));
            if prompt(locale, "Type ROUTE to allow task-text requests", "").await? != "ROUTE" {
                continue;
            }
            config.send_task_description = true;
        }
        let operation = match operation {
            Some(operation) => operation,
            None => match save_operation(&view, config) {
                Ok(operation) => operation,
                Err(error) => {
                    notice = locale.message(&error.to_string()).into();
                    continue;
                }
            },
        };
        match manager.execute(operation).await {
            Ok(result) => {
                notice = locale.message(&result.message).into();
                if preview {
                    println!("{}", clean(&notice));
                    if result.data.is_null() {
                        println!(
                            "{}",
                            locale.text("No valid local choice; keep the current model.")
                        );
                    } else {
                        println!(
                            "{}",
                            locale.format(
                                "Local suggestion: {} · {}",
                                &[
                                    result.data["model"].as_str().unwrap_or("?"),
                                    result.data["effort"].as_str().unwrap_or("default")
                                ]
                            )
                        );
                        println!(
                            "{}",
                            clean(result.data["reason"].as_str().unwrap_or_default())
                        );
                    }
                    let _ = prompt(locale, "Press Enter to return", "").await?;
                }
            }
            Err(error) => notice = locale.message(&error.to_string()).into(),
        }
    }
}

fn render_models(view: &ModelRoutingView, locale: Locale) -> String {
    let config = &view.config;
    let mut lines = vec![locale.text("Models / relative roles").to_string()];
    if view.models.is_empty() {
        lines.push(
            locale
                .text("No eligible catalog models; automatic selection keeps the current model.")
                .into(),
        );
    }
    for (index, model) in view.models.iter().enumerate() {
        let allowed =
            config.allowed_models.is_empty() || config.allowed_models.contains(&model.model);
        lines.push(format!(
            "{}. {} · {} · {}",
            index + 1,
            clean(&model.model),
            locale.message(model.role.as_deref().unwrap_or("unassigned")),
            locale.text(if allowed { "Included" } else { "Excluded" })
        ));
    }
    lines.push(locale.text("* includes every supported model. Choose a model number to change its role or eligibility.").into());
    lines.push(
        locale
            .text("[R] Refresh model catalog; no inference request is sent.")
            .into(),
    );
    lines.join("\n")
}

async fn model_settings(
    manager: &Arc<AccountManager>,
    view: &ModelRoutingView,
    draft: &ModelRoutingConfigToml,
    locale: Locale,
) -> anyhow::Result<Option<ModelRoutingConfigToml>> {
    let mut view = view.clone();
    view.config = draft.clone();
    loop {
        let mut config = view.config.clone();
        println!("{}", render_models(&view, locale));
        let value = prompt(locale, "Model number, * or R (Enter returns)", "").await?;
        if value.eq_ignore_ascii_case("r") {
            match manager
                .execute(AccountManagerOperation::RoutingRefreshModels)
                .await
            {
                Ok(result) => println!("{}", clean(locale.message(&result.message))),
                Err(error) => println!("{}", clean(locale.message(&error.to_string()))),
            }
            let mut refreshed = manager.model_routing_view().await?;
            refreshed.config = config;
            view = refreshed;
            continue;
        }
        if value.is_empty() {
            return Ok(None);
        }
        if value == "*" {
            config.allowed_models.clear();
            return Ok(Some(config));
        }
        let model = value
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| view.models.get(index))
            .ok_or_else(|| anyhow::anyhow!(locale.text("Choose a listed number")))?;
        println!(
            "{}",
            locale
                .text("0 Default role  1 Economy  2 Balanced  3 Capability  4 Toggle eligibility")
        );
        match prompt(locale, "Model setting number (Enter returns)", "")
            .await?
            .as_str()
        {
            "" => return Ok(None),
            "0" => {
                config.model_roles.remove(&model.model);
            }
            "1" => {
                config
                    .model_roles
                    .insert(model.model.clone(), ModelRoutingRole::Economy);
            }
            "2" => {
                config
                    .model_roles
                    .insert(model.model.clone(), ModelRoutingRole::Balanced);
            }
            "3" => {
                config
                    .model_roles
                    .insert(model.model.clone(), ModelRoutingRole::Capability);
            }
            "4" => {
                if config.allowed_models.is_empty() {
                    config.allowed_models = view
                        .models
                        .iter()
                        .map(|model| model.model.clone())
                        .collect();
                }
                if config.allowed_models.contains(&model.model) {
                    anyhow::ensure!(
                        config.allowed_models.len() > 1,
                        locale.text("Keep at least one model, or use * to allow all.")
                    );
                    config.allowed_models.retain(|name| name != &model.model);
                } else {
                    config.allowed_models.push(model.model.clone());
                }
            }
            _ => anyhow::bail!(locale.text("Choose a listed number")),
        }
        return Ok(Some(config));
    }
}

#[cfg(test)]
#[path = "account_manager_routing_tests.rs"]
mod tests;
