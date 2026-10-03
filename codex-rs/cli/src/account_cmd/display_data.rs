use std::collections::HashMap;
use std::io::IsTerminal;

use chrono::Utc;
use codex_login::AccountPoolRuntime;
use codex_login::AccountProfileState;
use codex_login::AccountProfileStore;
use codex_login::AccountRuntimeStateStore;
use codex_utils_cli::CliConfigOverrides;

use super::display::AccountInventory;
use super::display::AccountOutputOptions;
use super::display::AccountRow;
use super::display::AccountView;
use super::display::LoginState;
use super::display::OutputDestination;
use super::display::PoolSettings;
use super::load_config_or_exit;
use crate::account_config::format_rotation_strategy;

pub(super) async fn run_account_view(
    cli_config_overrides: CliConfigOverrides,
    view: AccountView,
    mut options: AccountOutputOptions,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let mut records =
        match AccountProfileStore::new(config.codex_home.to_path_buf()).load_profile_records() {
            Ok(records) => records,
            Err(error) => {
                eprintln!("Error reading account profiles: {error}");
                std::process::exit(/*code*/ 1);
            }
        };
    records.sort_by(|left, right| {
        left.profile
            .priority
            .cmp(&right.profile.priority)
            .then_with(|| left.profile.id.as_str().cmp(right.profile.id.as_str()))
    });
    let now = Utc::now();
    let suspended = AccountPoolRuntime::is_home_suspended(&config.codex_home);
    let mut warnings = Vec::new();
    let runtime = match AccountRuntimeStateStore::new(config.codex_home.to_path_buf()).load() {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            warnings.push(format!("Cached scheduler state could not be read: {error}"));
            None
        }
    };
    let runtime_by_id = runtime
        .as_ref()
        .map(|runtime| {
            runtime
                .profiles
                .iter()
                .map(|state| (&state.profile_id, state))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut accounts = Vec::with_capacity(records.len());
    for record in records {
        let profile = &record.profile;
        let state = runtime_by_id.get(&profile.id).copied();
        let active = !suspended
            && runtime
                .as_ref()
                .and_then(|runtime| runtime.active_profile_id.as_ref())
                == Some(&profile.id);
        let cooldown_until = state
            .and_then(|state| state.exhausted_until)
            .filter(|until| *until > now);
        // Read stored identity only. Metadata views never refresh a credential, including
        // disabled or pending profiles, or initialize the execution scheduler.
        let (plan, email, login) = if record.state == AccountProfileState::PendingLogin {
            (None, None, LoginState::Pending)
        } else {
            match codex_login::load_auth_dot_json(
                &profile.credential_home,
                config.cli_auth_credentials_store_mode,
                config.auth_keyring_backend_kind(),
            ) {
                Ok(Some(auth)) => {
                    let has_material = auth
                        .tokens
                        .as_ref()
                        .is_some_and(|tokens| !tokens.access_token.is_empty())
                        || auth
                            .openai_api_key
                            .as_ref()
                            .is_some_and(|key| !key.is_empty())
                        || auth
                            .personal_access_token
                            .as_ref()
                            .is_some_and(|token| !token.is_empty())
                        || auth.agent_identity.as_ref().is_some_and(
                            codex_login::auth::AgentIdentityStorage::has_auth_material,
                        )
                        || auth.bedrock_api_key.is_some()
                        || auth.bedrock_access_keys.is_some();
                    let (plan, email) = auth
                        .tokens
                        .as_ref()
                        .map(|tokens| {
                            (
                                tokens.id_token.get_chatgpt_plan_type(),
                                tokens.id_token.email.clone(),
                            )
                        })
                        .unwrap_or_default();
                    (
                        plan,
                        email,
                        if has_material {
                            LoginState::Cached
                        } else {
                            LoginState::Missing
                        },
                    )
                }
                Ok(None) => (None, None, LoginState::Missing),
                Err(_) => (None, None, LoginState::ReadFailed),
            }
        };
        let availability = if profile.disabled {
            "disabled"
        } else if suspended {
            "paused"
        } else if record.state == AccountProfileState::PendingLogin {
            "pending login"
        } else if cooldown_until.is_some() {
            "cooldown"
        } else {
            match login {
                LoginState::Missing => "login required",
                LoginState::ReadFailed => "unknown",
                LoginState::Cached if runtime.is_some() => "eligible",
                LoginState::Cached => "unknown",
                LoginState::Pending => "pending login",
            }
        }
        .to_string();
        let rate_limits = state
            .map(|state| state.rate_limits.clone())
            .unwrap_or_default();
        let warmup = state
            .and_then(|state| state.window_warmup.as_ref())
            .filter(|_| !active)
            .and_then(|observation| {
                codex_login::visible_window_warmup_status(
                    observation,
                    rate_limits
                        .primary
                        .as_ref()
                        .map(|window| window.used_percent),
                    now,
                )
            });
        accounts.push(AccountRow {
            profile_id: profile.id.to_string(),
            label: profile.label.clone(),
            active,
            priority: profile.priority,
            state: record.state,
            disabled: profile.disabled,
            login,
            availability,
            plan,
            email,
            cooldown_until,
            rate_limits,
            warmup,
        });
    }
    let settings = match view {
        AccountView::List => None,
        AccountView::Pool => Some(PoolSettings {
            rotation_strategy: format_rotation_strategy(
                config.account_pool.effective_rotation_strategy(),
            )
            .into(),
            return_to_preferred: config.account_pool.effective_return_to_preferred(),
            preemptive_switch: config
                .account_pool
                .effective_preemptive_switch_percent()
                .map(|percent| format!("{percent:.0}%"))
                .unwrap_or_else(|| "disabled".into()),
        }),
    };
    let inventory = AccountInventory {
        generated_at: now.timestamp(),
        suspended,
        warnings,
        settings,
        accounts,
    };
    options.format = options.format.resolve(if std::io::stdout().is_terminal() {
        OutputDestination::Terminal
    } else {
        OutputDestination::Pipe
    });
    let columns = crossterm::terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(/*default*/ 80);
    if matches!(options.format, super::display::AccountOutputFormat::Tsv) {
        for warning in &inventory.warnings {
            eprintln!("Warning: {warning}");
        }
    }
    print!(
        "{}",
        super::display::render(&inventory, view, options, columns)
    );
    std::process::exit(/*code*/ 0);
}
