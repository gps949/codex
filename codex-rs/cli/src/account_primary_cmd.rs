//! Host sign-in selection is independent from inference scheduling.

use clap::Args;
use clap::Subcommand;
use codex_core::config::ConfigBuilder;
use codex_login::AccountProfileStore;
use codex_login::PrimaryLoginRuntime;
use codex_login::PrimaryLoginSource;
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
                "Remote Control follows this account and workspace; reconnect or pair again if the owner changed."
            );
        }
        PrimaryAction::Root => {
            store.use_root()?;
            println!(
                "Host sign-in now uses the root login. Pool inference selection was not changed."
            );
        }
        PrimaryAction::Logout => {
            store.sign_out()?;
            println!("Host signed out. Subscription pool credentials were retained.");
        }
    }
    let state = store.load()?;
    let runtime = PrimaryLoginRuntime::start(config.auth_config()).await?;
    let auth = runtime.auth_manager().auth_cached();
    match state.source {
        PrimaryLoginSource::RootLogin => println!("Source: Root login"),
        PrimaryLoginSource::Profile { profile_id, .. } => {
            println!("Source: Pool profile {profile_id}")
        }
        PrimaryLoginSource::SignedOut => println!("Source: Signed out"),
    }
    if let Some(auth) = &auth {
        println!(
            "Account: {}",
            auth.get_account_email()
                .unwrap_or_else(|| "Unknown email".into())
        );
    }
    let ready = auth
        .as_ref()
        .is_some_and(|auth| auth.uses_codex_backend() && auth.get_account_id().is_some());
    println!(
        "Remote authentication: {}",
        if ready {
            "Ready"
        } else {
            "Host sign-in required"
        }
    );
    println!(
        "This is the host login source. Remote connection availability and device pairing are separate."
    );
    Ok(())
}
