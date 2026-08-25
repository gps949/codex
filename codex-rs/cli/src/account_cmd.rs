use std::collections::HashMap;

use chrono::Utc;
use codex_core::config::Config;
use codex_login::AccountProfileId;
use codex_login::AccountProfileState;
use codex_login::AccountProfileStore;
use codex_login::AccountRuntimeProfileState;
use codex_login::AccountRuntimeStateStore;
use codex_login::AuthManager;
use codex_login::CLIENT_ID;
use codex_login::ServerOptions;
use codex_login::begin_account_browser_login;
use codex_login::begin_account_device_login;
use codex_login::logout_with_revoke;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_utils_cli::CliConfigOverrides;

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
        Ok(profile) => {
            eprintln!(
                "Added Codex account {}{} with priority {}.",
                profile.id,
                profile
                    .label
                    .as_deref()
                    .map(|label| format!(" ({label})"))
                    .unwrap_or_default(),
                profile.priority
            );
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("Error adding Codex account: {error}");
            std::process::exit(1);
        }
    }
}

pub(crate) async fn run_account_list(cli_config_overrides: CliConfigOverrides) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let store = AccountProfileStore::new(config.codex_home.to_path_buf());
    let records = match store.load_profile_records() {
        Ok(records) => records,
        Err(error) => {
            eprintln!("Error reading account profiles: {error}");
            std::process::exit(1);
        }
    };
    if records.is_empty() {
        eprintln!("No Codex account profiles are configured.");
        std::process::exit(0);
    }

    let runtime_state = AccountRuntimeStateStore::new(config.codex_home.to_path_buf())
        .load()
        .unwrap_or_default();
    let runtime_by_id = runtime_state
        .profiles
        .iter()
        .map(|state| (state.profile_id.clone(), state))
        .collect::<HashMap<_, _>>();

    let mut records = records;
    records.sort_by(|left, right| {
        left.profile
            .priority
            .cmp(&right.profile.priority)
            .then_with(|| left.profile.id.as_str().cmp(right.profile.id.as_str()))
    });

    println!("ACTIVE\tPRIORITY\tPROFILE\tSTATE\tPLAN\tEMAIL\tCOOLDOWN\tLABEL");
    for record in records {
        let active = runtime_state.active_profile_id.as_ref() == Some(&record.profile.id);
        let runtime = runtime_by_id.get(&record.profile.id).copied();
        let cooldown = format_cooldown(runtime);
        let (plan, email) = load_profile_identity(&config, &record.profile).await;
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            if active { "*" } else { "" },
            record.profile.priority,
            record.profile.id,
            match record.state {
                AccountProfileState::PendingLogin => "pending_login",
                AccountProfileState::Ready => "ready",
            },
            plan.unwrap_or_else(|| "-".to_string()),
            email.unwrap_or_else(|| "-".to_string()),
            cooldown,
            record.profile.label.as_deref().unwrap_or("-"),
        );
    }
    std::process::exit(0);
}

pub(crate) async fn run_account_use(
    cli_config_overrides: CliConfigOverrides,
    profile_id: String,
    force: bool,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let profile_id = parse_profile_id_or_exit(&profile_id);
    let store = AccountProfileStore::new(config.codex_home.to_path_buf());
    let records = match store.load_profile_records() {
        Ok(records) => records,
        Err(error) => {
            eprintln!("Error reading account profiles: {error}");
            std::process::exit(1);
        }
    };
    let Some(record) = records.iter().find(|record| record.profile.id == profile_id) else {
        eprintln!("Unknown Codex account profile: {profile_id}");
        std::process::exit(1);
    };
    if record.state != AccountProfileState::Ready {
        eprintln!("Account profile {profile_id} has not completed login.");
        std::process::exit(1);
    }

    let runtime_store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    let mut runtime = runtime_store.load().unwrap_or_default();
    let profile_state = runtime
        .profiles
        .iter_mut()
        .find(|state| state.profile_id == profile_id);
    if let Some(state) = profile_state {
        if let Some(reset) = state.exhausted_until.as_ref()
            && reset > &Utc::now()
            && !force
        {
            eprintln!(
                "Account profile {profile_id} is cooling down until {reset}. Use --force to probe it now."
            );
            std::process::exit(1);
        }
        if force {
            state.exhausted_until = None;
        }
    }
    runtime.active_profile_id = Some(profile_id.clone());
    if let Err(error) = runtime_store.save(&runtime) {
        eprintln!("Error persisting active account: {error}");
        std::process::exit(1);
    }
    eprintln!("Selected Codex account profile {profile_id}.");
    std::process::exit(0);
}

pub(crate) async fn run_account_remove(
    cli_config_overrides: CliConfigOverrides,
    profile_id: String,
    keep_credentials: bool,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let profile_id = parse_profile_id_or_exit(&profile_id);
    let store = AccountProfileStore::new(config.codex_home.to_path_buf());
    let records = match store.load_profile_records() {
        Ok(records) => records,
        Err(error) => {
            eprintln!("Error reading account profiles: {error}");
            std::process::exit(1);
        }
    };
    let Some(record) = records.into_iter().find(|record| record.profile.id == profile_id) else {
        eprintln!("Unknown Codex account profile: {profile_id}");
        std::process::exit(1);
    };

    if !keep_credentials && profile_id.as_str() != "legacy-root" {
        if let Err(error) = logout_with_revoke(
            &record.profile.credential_home,
            config.cli_auth_credentials_store_mode,
            config.auth_keyring_backend_kind(),
            &config.auth_route_config(),
        )
        .await
        {
            eprintln!("Error revoking account credentials: {error}");
            std::process::exit(1);
        }
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

    if !keep_credentials && profile_id.as_str() != "legacy-root" {
        if let Err(error) = store.purge_managed_credentials(&profile_id) {
            eprintln!("Error deleting account credentials: {error}");
            std::process::exit(1);
        }
    }

    if profile_id.as_str() == "legacy-root" {
        eprintln!(
            "Removed legacy-root from the account pool. Root Codex credentials were left untouched."
        );
    } else if keep_credentials {
        eprintln!("Removed account profile {profile_id}; its credential directory was preserved.");
    } else {
        eprintln!("Removed account profile {profile_id} and revoked its credentials.");
    }
    std::process::exit(0);
}

async fn register_existing_root_login(
    config: &Config,
    store: &AccountProfileStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let manager = AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false).await?;
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

async fn load_profile_identity(
    config: &Config,
    profile: &codex_login::AccountProfile,
) -> (Option<String>, Option<String>) {
    let mut auth_config = config.auth_config();
    auth_config.codex_home = profile.credential_home.clone();
    match AuthManager::shared_from_auth_config(auth_config, /*enable_codex_api_key_env*/ false).await
    {
        Ok(manager) => match manager.auth().await {
            Some(auth) => (
                auth.account_plan_type().map(|plan| format!("{plan:?}")),
                auth.get_account_email(),
            ),
            None => (None, None),
        },
        Err(_) => (None, None),
    }
}

fn format_cooldown(state: Option<&AccountRuntimeProfileState>) -> String {
    state
        .and_then(|state| state.exhausted_until.as_ref())
        .filter(|reset| *reset > &Utc::now())
        .map(ToString::to_string)
        .unwrap_or_else(|| "-".to_string())
}

fn parse_profile_id_or_exit(value: &str) -> AccountProfileId {
    match AccountProfileId::new(value.to_string()) {
        Ok(profile_id) => profile_id,
        Err(error) => {
            eprintln!("Invalid account profile id: {error}");
            std::process::exit(1);
        }
    }
}

async fn load_config_or_exit(cli_config_overrides: CliConfigOverrides) -> Config {
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
