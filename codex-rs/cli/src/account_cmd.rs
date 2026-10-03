use codex_core::config::Config;
use codex_login::AccountLoginOutcomeKind;
use codex_login::AccountProfileId;
use codex_login::AccountProfileState;
use codex_login::AccountProfileStore;
use codex_login::AccountRuntimeStateStore;
use codex_login::AuthManager;
use codex_login::CLIENT_ID;
use codex_login::ServerOptions;
use codex_login::begin_account_browser_login;
use codex_login::begin_account_device_login;
use codex_login::logout_with_revoke;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_utils_cli::CliConfigOverrides;

use crate::account_config::format_rotation_strategy;
use crate::account_config::parse_rotation_strategy;
use crate::account_config::patch_account_pool_config;

mod display;
mod display_data;
mod display_table;
mod quota_display;

pub(crate) use display::AccountOutputFormat;
pub(crate) use display::AccountOutputOptions;

const DEFAULT_PRIORITY_STEP: u32 = 10;

pub(crate) async fn run_account_add(
    cli_config_overrides: CliConfigOverrides,
    label: Option<String>,
    priority: Option<u32>,
    device_auth: bool,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    if !config
        .auth_config()
        .is_login_method_allowed(ForcedLoginMethod::Chatgpt)
    {
        eprintln!("ChatGPT login is disabled by the current authentication policy.");
        std::process::exit(1);
    }

    let store = AccountProfileStore::new(config.codex_home.to_path_buf());
    if let Err(error) = register_existing_root_login(&config, &store).await {
        eprintln!("Error preparing existing ChatGPT login for account pooling: {error}");
        std::process::exit(1);
    }

    let priority = match priority {
        Some(priority) => priority,
        None => match next_priority(&store) {
            Ok(priority) => priority,
            Err(error) => {
                eprintln!("Error reading account profiles: {error}");
                std::process::exit(1);
            }
        },
    };

    let options = ServerOptions::new(
        config.codex_home.to_path_buf(),
        CLIENT_ID.to_string(),
        config.auth_config().effective_chatgpt_workspaces(),
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
        config.auth_route_config(),
    );

    let result = if device_auth {
        match begin_account_device_login(store, options, label, priority).await {
            Ok(pending) => {
                let profile_id = pending.profile().id.clone();
                eprintln!("Adding Codex account profile {profile_id}.");
                if let (Some(url), Some(code)) = (pending.verification_url(), pending.user_code()) {
                    eprintln!("Open this URL and enter the code:\n\n{url}\n\nCode: {code}\n");
                }
                pending.complete().await
            }
            Err(error) => Err(error),
        }
    } else {
        match begin_account_browser_login(store, options, label, priority) {
            Ok(pending) => {
                let profile_id = pending.profile().id.clone();
                if let (Some(port), Some(url)) = (pending.actual_port(), pending.auth_url()) {
                    eprintln!(
                        "Adding Codex account profile {profile_id}.\nStarting local login server on http://localhost:{port}.\nIf your browser did not open, navigate to:\n\n{url}\n"
                    );
                }
                pending.complete().await
            }
            Err(error) => Err(error),
        }
    };

    match result {
        Ok(outcome) => match outcome.kind {
            AccountLoginOutcomeKind::Added => {
                eprintln!(
                    "Added Codex account {}{} with priority {}.",
                    outcome.profile.id,
                    outcome
                        .profile
                        .label
                        .as_deref()
                        .map(|label| format!(" ({label})"))
                        .unwrap_or_default(),
                    outcome.profile.priority
                );
            }
            AccountLoginOutcomeKind::RefreshedExistingDuplicate => {
                eprintln!(
                    "This ChatGPT user is already configured as {}{}. Refreshed that profile's login instead of creating a duplicate.",
                    outcome.profile.id,
                    outcome
                        .profile
                        .label
                        .as_deref()
                        .map(|label| format!(" ({label})"))
                        .unwrap_or_default(),
                );
            }
        },
        Err(error) => {
            eprintln!("Error adding Codex account: {error}");
            std::process::exit(1);
        }
    }
    std::process::exit(0);
}

pub(crate) async fn run_account_relogin(
    cli_config_overrides: CliConfigOverrides,
    profile_id: String,
    device_auth: bool,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    if !config
        .auth_config()
        .is_login_method_allowed(ForcedLoginMethod::Chatgpt)
    {
        eprintln!("ChatGPT login is disabled by the current authentication policy.");
        std::process::exit(1);
    }
    let store = AccountProfileStore::new(config.codex_home.to_path_buf());
    let profile_id = resolve_profile_id_or_exit(&store, &profile_id);
    let options = ServerOptions::new(
        config.codex_home.to_path_buf(),
        CLIENT_ID.to_string(),
        config.auth_config().effective_chatgpt_workspaces(),
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
        config.auth_route_config(),
    );

    let result = if device_auth {
        match codex_login::begin_account_device_relogin(store, options, &profile_id).await {
            Ok(pending) => {
                eprintln!("Re-authenticating Codex account profile {profile_id}.");
                if let (Some(url), Some(code)) = (pending.verification_url(), pending.user_code()) {
                    eprintln!("Open this URL and enter the code:\n\n{url}\n\nCode: {code}\n");
                }
                pending.complete().await
            }
            Err(error) => Err(error),
        }
    } else {
        match codex_login::begin_account_browser_relogin(store, options, &profile_id) {
            Ok(pending) => {
                if let (Some(port), Some(url)) = (pending.actual_port(), pending.auth_url()) {
                    eprintln!(
                        "Re-authenticating Codex account profile {profile_id}.\nStarting local login server on http://localhost:{port}.\nIf your browser did not open, navigate to:\n\n{url}\n"
                    );
                }
                pending.complete().await
            }
            Err(error) => Err(error),
        }
    };

    match result {
        Ok(outcome) => {
            eprintln!("Re-authenticated Codex account {}.", outcome.profile.id);
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("Error re-authenticating Codex account: {error}");
            std::process::exit(1);
        }
    }
}

pub(crate) async fn run_account_set(
    cli_config_overrides: CliConfigOverrides,
    profile_id: String,
    priority: Option<u32>,
    label: Option<String>,
    clear_label: bool,
    disabled: Option<bool>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let store = AccountProfileStore::new(config.codex_home.to_path_buf());
    let profile_id = resolve_profile_id_or_exit(&store, &profile_id);
    let label_update = if clear_label {
        Some(codex_login::AccountLabelUpdate::Clear)
    } else {
        label.map(codex_login::AccountLabelUpdate::Set)
    };
    if priority.is_none() && label_update.is_none() && disabled.is_none() {
        eprintln!(
            "Nothing to update. Pass --priority, --label/--clear-label, or use enable/disable."
        );
        std::process::exit(1);
    }
    let update = codex_login::AccountProfileMetadataUpdate {
        priority,
        label: label_update,
        disabled,
    };
    match store.update_profile_metadata(&profile_id, update) {
        Ok(profile) => {
            eprintln!(
                "Updated account profile {} (priority {}{}{}).",
                profile.id,
                profile.priority,
                profile
                    .label
                    .as_deref()
                    .map(|label| format!(", label \"{label}\""))
                    .unwrap_or_default(),
                if profile.disabled { ", disabled" } else { "" },
            );
            eprintln!(
                "Running sessions adopt changes at their next refresh; command-line overrides take precedence."
            );
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("Error updating account profile: {error}");
            std::process::exit(1);
        }
    }
}

pub(crate) async fn run_account_list(
    cli_config_overrides: CliConfigOverrides,
    options: AccountOutputOptions,
) -> ! {
    display_data::run_account_view(cli_config_overrides, display::AccountView::List, options).await
}

pub(crate) async fn run_account_use(
    cli_config_overrides: CliConfigOverrides,
    profile_id: String,
    force: bool,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let store = AccountProfileStore::new(config.codex_home.to_path_buf());
    let records = match store.load_profile_records() {
        Ok(records) => records,
        Err(error) => {
            eprintln!("Error reading account profiles: {error}");
            std::process::exit(1);
        }
    };
    let record = match crate::account_selector::resolve_account(&records, &profile_id) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let profile_id = record.profile.id.clone();
    if record.profile.disabled {
        eprintln!(
            "Account profile {profile_id} is disabled. Enable it with `codex account enable {profile_id}` first."
        );
        std::process::exit(1);
    }
    if record.state != AccountProfileState::Ready {
        eprintln!("Account profile {profile_id} has not completed login.");
        std::process::exit(1);
    }

    let runtime_store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    if let Err(error) = runtime_store.select(
        profile_id.clone(),
        if force {
            codex_login::AccountSelectionMode::ForceProbe
        } else {
            codex_login::AccountSelectionMode::AvailableOnly
        },
    ) {
        eprintln!("Error selecting account: {error}");
        std::process::exit(1);
    }
    if let Err(error) = codex_login::AccountPoolRuntime::resume_home(&config.codex_home) {
        eprintln!("Selected account {profile_id}, but could not resume account pooling: {error}");
        std::process::exit(1);
    }
    eprintln!(
        "Selected Codex account {} ({profile_id}). Running sessions pick up the selection shortly. Open /account (or start a new turn) to load newly added accounts.",
        record
            .profile
            .label
            .as_deref()
            .unwrap_or(profile_id.as_str())
    );
    std::process::exit(0);
}

pub(crate) async fn run_account_remove(
    cli_config_overrides: CliConfigOverrides,
    profile_id: String,
    keep_credentials: bool,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let store = AccountProfileStore::new(config.codex_home.to_path_buf());
    let records = match store.load_profile_records() {
        Ok(records) => records,
        Err(error) => {
            eprintln!("Error reading account profiles: {error}");
            std::process::exit(1);
        }
    };
    let record = match crate::account_selector::resolve_account(&records, &profile_id) {
        Ok(record) => record.clone(),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let profile_id = record.profile.id.clone();

    if !keep_credentials
        && profile_id.as_str() != "legacy-root"
        && let Err(error) = logout_with_revoke(
            &record.profile.credential_home,
            config.cli_auth_credentials_store_mode,
            config.auth_keyring_backend_kind(),
            &config.auth_route_config(),
        )
        .await
    {
        eprintln!("Error deleting local account credentials: {error}");
        std::process::exit(1);
    }

    match store.remove_profile_metadata(&profile_id) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("Unknown Codex account profile: {profile_id}");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("Error removing account profile: {error}");
            std::process::exit(1);
        }
    }

    let runtime_store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    if let Err(error) = runtime_store.remove_profile(&profile_id) {
        eprintln!("Warning: failed to remove stale scheduler state: {error}");
    }

    if !keep_credentials
        && profile_id.as_str() != "legacy-root"
        && let Err(error) = store.purge_managed_credentials(&profile_id)
    {
        eprintln!("Error deleting account credentials: {error}");
        std::process::exit(1);
    }

    if profile_id.as_str() == "legacy-root" {
        eprintln!(
            "Removed legacy-root from the account pool. Root Codex credentials were left untouched."
        );
    } else if keep_credentials {
        eprintln!("Removed account profile {profile_id}; its credential directory was preserved.");
    } else {
        eprintln!(
            "Removed account profile {profile_id} and its local credentials. Server revocation was attempted."
        );
    }
    std::process::exit(0);
}

pub(crate) async fn run_account_pool(
    cli_config_overrides: CliConfigOverrides,
    options: AccountOutputOptions,
) -> ! {
    display_data::run_account_view(cli_config_overrides, display::AccountView::Pool, options).await
}

pub(crate) async fn run_account_config_show(cli_config_overrides: CliConfigOverrides) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let pool = &config.account_pool;
    println!(
        "rotation_strategy={}",
        format_rotation_strategy(pool.effective_rotation_strategy())
    );
    println!(
        "return_to_preferred={}",
        pool.effective_return_to_preferred()
    );
    match pool.effective_preemptive_switch_percent() {
        Some(percent) => println!("preemptive_switch_percent={percent:.0}"),
        None => println!("preemptive_switch_percent=disabled"),
    }
    println!(
        "auto_reset_credits={}",
        match pool.effective_auto_reset_credits() {
            codex_config::AutoResetCredits::Never => "never",
            codex_config::AutoResetCredits::WhenPoolExhausted => "when_pool_exhausted",
        }
    );
    println!(
        "auto_reset_credit_min_wait_minutes={}",
        pool.effective_reset_credit_min_wait_minutes()
    );
    println!("window_warmup={}", pool.effective_window_warmup());
    println!(
        "window_warmup_interval_minutes={}",
        pool.effective_window_warmup_interval().as_secs() / 60
    );
    println!(
        "resume_after_reset={}",
        !pool.effective_reset_wait().is_zero()
    );
    println!(
        "max_reset_wait_minutes={}",
        pool.effective_reset_wait().as_secs() / 60
    );
    println!(
        "\nWarmup uses a tiny request to start standby quota windows. Reset credits are opt-in."
    );
    println!("Settings: codex account config --help");
    std::process::exit(0);
}

pub(crate) async fn run_account_config_set_rotation_strategy(
    cli_config_overrides: CliConfigOverrides,
    strategy: String,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let strategy = match parse_rotation_strategy(&strategy) {
        Ok(strategy) => strategy,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    match patch_account_pool_config(&config.codex_home, |pool| {
        pool.rotation_strategy = Some(strategy);
    }) {
        Ok(pool) => {
            eprintln!(
                "Updated rotation_strategy to {} in config.toml.",
                format_rotation_strategy(pool.effective_rotation_strategy())
            );
            eprintln!(
                "Running sessions adopt changes at their next refresh; command-line overrides take precedence."
            );
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("Error updating config.toml: {error}");
            std::process::exit(1);
        }
    }
}

pub(crate) async fn run_account_config_set_return_to_preferred(
    cli_config_overrides: CliConfigOverrides,
    enabled: bool,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    match patch_account_pool_config(&config.codex_home, |pool| {
        pool.return_to_preferred = Some(enabled);
    }) {
        Ok(pool) => {
            eprintln!(
                "Updated return_to_preferred to {} in config.toml.",
                pool.effective_return_to_preferred()
            );
            eprintln!(
                "Running sessions adopt changes at their next refresh; command-line overrides take precedence."
            );
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("Error updating config.toml: {error}");
            std::process::exit(1);
        }
    }
}

pub(crate) async fn run_account_config_set_preemptive_switch_percent(
    cli_config_overrides: CliConfigOverrides,
    percent: f64,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
        eprintln!("Expected a finite percentage from 0 to 100; use 0 to disable early rotation.");
        std::process::exit(1);
    }
    match patch_account_pool_config(&config.codex_home, |pool| {
        pool.preemptive_switch_percent = Some(percent);
    }) {
        Ok(pool) => {
            match pool.effective_preemptive_switch_percent() {
                Some(percent) => {
                    eprintln!("Updated preemptive_switch_percent to {percent:.0} in config.toml.");
                }
                None => eprintln!("Disabled preemptive switching in config.toml."),
            }
            eprintln!(
                "Running sessions adopt changes at their next refresh; command-line overrides take precedence."
            );
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("Error updating config.toml: {error}");
            std::process::exit(1);
        }
    }
}

async fn register_existing_root_login(
    config: &Config,
    store: &AccountProfileStore,
) -> Result<(), Box<dyn std::error::Error>> {
    if store.manifest_path().exists() {
        return Ok(());
    }
    let manager =
        AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false).await?;
    if manager
        .auth()
        .await
        .is_some_and(|auth| matches!(auth, codex_login::CodexAuth::Chatgpt(_)))
    {
        store.ensure_legacy_root_profile(Some("Existing login".to_string()), 0)?;
    }
    Ok(())
}

fn next_priority(
    store: &AccountProfileStore,
) -> Result<u32, codex_login::AccountProfileStoreError> {
    let max_priority = store
        .load_profile_records()?
        .into_iter()
        .map(|record| record.profile.priority)
        .max();
    Ok(max_priority
        .map(|priority| priority.saturating_add(DEFAULT_PRIORITY_STEP))
        .unwrap_or(0))
}

fn resolve_profile_id_or_exit(store: &AccountProfileStore, selector: &str) -> AccountProfileId {
    let records = match store.load_profile_records() {
        Ok(records) => records,
        Err(error) => {
            eprintln!("Error reading account profiles: {error}");
            std::process::exit(1);
        }
    };
    match crate::account_selector::resolve_account(&records, selector) {
        Ok(record) => record.profile.id.clone(),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

pub(crate) async fn load_config_or_exit(cli_config_overrides: CliConfigOverrides) -> Config {
    let cli_overrides = match cli_config_overrides.parse_overrides() {
        Ok(overrides) => overrides,
        Err(error) => {
            eprintln!("Error parsing -c overrides: {error}");
            std::process::exit(1);
        }
    };
    match Config::load_with_cli_overrides(cli_overrides).await {
        Ok(config) => match config.auth_config().validate() {
            Ok(()) => config,
            Err(error) => {
                eprintln!("Error loading configuration: {error}");
                std::process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("Error loading configuration: {error}");
            std::process::exit(1);
        }
    }
}
