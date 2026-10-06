//! Direct, local model-policy controls; external task sharing requires its own flag.

use clap::Args;
use clap::ValueEnum;
use codex_app_server::account_management::AccountManager;
use codex_app_server::account_management::AccountManagerOperation;
use codex_config::ModelRoutingMode;
use codex_config::ModelRoutingRole;
use codex_config::ModelRoutingSource;
use codex_utils_cli::CliConfigOverrides;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    Off,
    Preview,
    Automatic,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Source {
    Local,
    DecisionService,
}

#[derive(Debug, Args)]
pub(crate) struct AccountRoutingArgs {
    /// Omit settings to show the saved and effective policy.
    #[arg(long, value_enum)]
    mode: Option<Mode>,
    #[arg(long, value_enum)]
    source: Option<Source>,
    /// 0 favors longer use; 100 favors capability. This is not a quota multiplier.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    preference: Option<u8>,
    #[arg(long, action = clap::ArgAction::Set)]
    main_tasks: Option<bool>,
    #[arg(long, action = clap::ArgAction::Set)]
    subagents: Option<bool>,
    #[arg(long, value_parser = ["minimal", "low", "medium", "high", "xhigh", "max"])]
    max_effort: Option<String>,
    /// Consent to send up to 2048 bytes of task text to the configured Jev/Clef service.
    #[arg(long, action = clap::ArgAction::Set)]
    send_task_description: Option<bool>,
    #[arg(long, action = clap::ArgAction::Set)]
    local_fallback: Option<bool>,
    /// Restrict candidates by exact model name. Repeat for multiple models.
    #[arg(long = "allow-model", conflicts_with = "all_models")]
    allowed_models: Vec<String>,
    #[arg(long)]
    all_models: bool,
    /// Assign a relative role: MODEL=economy, MODEL=balanced or MODEL=capability.
    #[arg(long = "role")]
    roles: Vec<String>,
    /// Simulate a task using local rules; do not save changes or call a decision service.
    #[arg(long)]
    preview: Option<String>,
    /// Read the selected account's model catalog, without running inference.
    #[arg(long)]
    refresh_models: bool,
    #[arg(long)]
    json: bool,
}

pub(crate) async fn run(
    overrides: CliConfigOverrides,
    args: AccountRoutingArgs,
) -> anyhow::Result<()> {
    let config = codex_core::config::ConfigBuilder::default()
        .cli_overrides(overrides.parse_overrides().map_err(anyhow::Error::msg)?)
        .build()
        .await?;
    let manager = AccountManager::new(config);
    if args.refresh_models {
        manager
            .execute(AccountManagerOperation::RoutingRefreshModels)
            .await?;
    }
    let view = manager.model_routing_view().await?;
    let mut policy = view.config.clone();
    if let Some(mode) = args.mode {
        policy.mode = match mode {
            Mode::Off => ModelRoutingMode::Off,
            Mode::Preview => ModelRoutingMode::Preview,
            Mode::Automatic => ModelRoutingMode::Automatic,
        };
    }
    if let Some(source) = args.source {
        policy.source = match source {
            Source::Local => ModelRoutingSource::Local,
            Source::DecisionService => ModelRoutingSource::DecisionService,
        };
    }
    if let Some(value) = args.preference {
        policy.preference = value;
    }
    if let Some(value) = args.main_tasks {
        policy.main_tasks = value;
    }
    if let Some(value) = args.subagents {
        policy.subagents = value;
    }
    if let Some(value) = args.max_effort {
        policy.max_effort = value;
    }
    if let Some(value) = args.send_task_description {
        policy.send_task_description = value;
    }
    if let Some(value) = args.local_fallback {
        policy.local_fallback = value;
    }
    if args.all_models {
        policy.allowed_models.clear();
    }
    if !args.allowed_models.is_empty() {
        policy.allowed_models = args.allowed_models;
    }
    for assignment in args.roles {
        let (model, role) = assignment
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Use --role MODEL=economy|balanced|capability"))?;
        let role = match role {
            "economy" => ModelRoutingRole::Economy,
            "balanced" => ModelRoutingRole::Balanced,
            "capability" => ModelRoutingRole::Capability,
            _ => anyhow::bail!("Role must be economy, balanced or capability"),
        };
        policy.model_roles.insert(model.into(), role);
    }
    if let Some(task) = args.preview {
        let result = manager
            .execute(AccountManagerOperation::RoutingPreview {
                task,
                config: policy,
            })
            .await?;
        println!("{}", serde_json::to_string_pretty(&result.data)?);
        return Ok(());
    }
    if policy != view.config {
        manager
            .execute(AccountManagerOperation::RoutingSave {
                config: policy,
                expected_version: Some(view.user_config_version),
            })
            .await?;
    }
    let view = manager.model_routing_view().await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        let policy = &view.effective_config;
        println!("Automatic model selection");
        println!(
            "  Mode        {:?}\n  Source      {:?}\n  Preference  {:>3} / 100\n  Main tasks  {}\n  Subagents   {}\n  Max effort  {}",
            policy.mode,
            policy.source,
            policy.preference,
            policy.main_tasks,
            policy.subagents,
            policy.max_effort
        );
        if view.overridden {
            println!("  Higher-priority settings override the saved policy.");
        }
        println!("Manage visually: codex account manage (or --tui, then M).");
        println!("Manual choices win; use /model auto in a conversation to release its model pin.");
    }
    Ok(())
}
