//! Host sign-in selection is independent from inference scheduling.

use clap::Args;
use clap::Subcommand;
use codex_core::config::ConfigBuilder;
use codex_login::AccountProfileStore;
use codex_login::PrimaryLoginStore;
use codex_utils_cli::CliConfigOverrides;

#[derive(Debug, Args)]
pub(crate) struct AccountPrimaryArgs {
    #[command(subcommand)]
    action: PrimaryAction,
}

#[derive(Debug, Subcommand)]
enum PrimaryAction {
    /// Show the host sign-in used for Remote Control.
    Status,
    /// Use one subscription profile for host sign-in without changing inference selection.
    Use { account: String },
    /// Return to credentials saved by codex login at the root home.
    Root,
    /// Sign out the host while retaining all pool credentials.
    Logout,
}

pub(crate) async fn run(
    overrides: CliConfigOverrides,
    args: AccountPrimaryArgs,
) -> anyhow::Result<()> {
    let config = ConfigBuilder::default()
        .cli_overrides(overrides.parse_overrides().map_err(anyhow::Error::msg)?)
        .build()
        .await?;
    let store = PrimaryLoginStore::new(config.codex_home.to_path_buf());
    match args.action {
        PrimaryAction::Status => {}
        PrimaryAction::Use { account } => {
            let profiles =
                AccountProfileStore::new(config.codex_home.to_path_buf()).load_profile_records()?;
            let profile = crate::account_selector::resolve_account_with_config(
                &profiles,
                &account,
                &config.auth_config(),
            )
            .map_err(anyhow::Error::msg)?;
            store
                .select_profile(&config.auth_config(), &profile.profile.id)
                .await?;
            println!("Host sign-in source saved. Inference selection was not changed.");
            println!(
                "Running hosts apply this selection automatically. An enabled Remote service reconnects; a disabled service stays disabled."
            );
        }
        PrimaryAction::Root => {
            store.select_root(&config.auth_config()).await?;
            println!(
                "Host sign-in now uses the root login. Pool inference selection was not changed."
            );
        }
        PrimaryAction::Logout => {
            store.sign_out()?;
            println!("Host signed out. Subscription pool credentials were retained.");
        }
    }
    let view = codex_login::observe_primary_login(&config.auth_config());
    println!("Saved source: {}", view.label);
    if let Some(email) = &view.email {
        println!("Account: {email}");
    }
    println!(
        "Host authentication: {}",
        match view.status {
            codex_login::PrimaryLoginStatus::StoredReady => "Stored sign-in available",
            codex_login::PrimaryLoginStatus::NeedsLogin => "Sign-in required",
            codex_login::PrimaryLoginStatus::RuntimeResolutionRequired =>
                "Resolved by running host",
            codex_login::PrimaryLoginStatus::Invalid => "Saved source unavailable",
            codex_login::PrimaryLoginStatus::SignedOut => "Signed out",
        }
    );
    if let Some(message) = &view.message {
        println!("{message}");
    }
    if let Some(runtime) =
        codex_login::PrimaryRuntimeStatusStore::read_current(&config.codex_home, view.revision)
    {
        println!(
            "Observed host: {}",
            runtime.email.as_deref().unwrap_or("Not reported")
        );
        println!(
            "Remote service: {}",
            match runtime.remote_status {
                codex_login::PrimaryRemoteStatus::Disabled => "Disabled",
                codex_login::PrimaryRemoteStatus::Connecting => "Connecting",
                codex_login::PrimaryRemoteStatus::Connected => "Connected to relay",
                codex_login::PrimaryRemoteStatus::Errored => "Connection needs attention",
                codex_login::PrimaryRemoteStatus::RequirementsDisabled =>
                    "Disabled by account requirements",
                codex_login::PrimaryRemoteStatus::AuthenticationDenied =>
                    "Host authentication denied by requirements",
            }
        );
    } else {
        println!("Remote service: Not reported by a recent running host.");
    }
    println!(
        "Host sign-in and inference selection are independent. Devices may need pairing for a new owner."
    );
    Ok(())
}
