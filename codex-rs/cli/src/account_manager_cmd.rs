use clap::Args;
use codex_app_server::account_management::AccountManager;
use codex_app_server::account_management::AccountManagerWebOptions;
use codex_utils_cli::CliConfigOverrides;
use std::net::SocketAddr;
use std::sync::Arc;

/// Start the dedicated account manager; loopback is the safe default.
#[derive(Debug, Args)]
pub(crate) struct AccountManageArgs {
    /// Use the task-independent terminal manager instead of a browser.
    #[arg(long, conflicts_with = "listen")]
    tui: bool,
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: SocketAddr,
    /// Browser origin served by your authenticated HTTPS reverse proxy.
    #[arg(long = "allow-origin")]
    allowed_origins: Vec<String>,
    /// Keep the manager URL in the terminal without opening a browser.
    #[arg(long)]
    no_open: bool,
}

pub(crate) async fn run(
    overrides: CliConfigOverrides,
    args: AccountManageArgs,
) -> anyhow::Result<()> {
    let config = codex_core::config::ConfigBuilder::default()
        .cli_overrides(overrides.parse_overrides().map_err(anyhow::Error::msg)?)
        .build()
        .await?;
    let manager = AccountManager::new(config);
    if args.tui {
        let result = crate::account_manager_tui::run(Arc::clone(&manager)).await;
        manager.shutdown_logins().await;
        return result;
    }
    codex_app_server::account_management::serve(manager, AccountManagerWebOptions {
        listen: args.listen, allowed_origins: args.allowed_origins,
    }, |url| {
        eprintln!("Codex account manager: {url}");
        eprintln!("Keep this host process running. Ctrl+C closes the manager. The pairing link grants account administration; share it only with your own browser.");
        if !args.no_open {
            #[cfg(target_os = "macos")]
            let command = std::process::Command::new("open").arg(url).spawn();
            #[cfg(target_os = "linux")]
            let command = std::process::Command::new("xdg-open").arg(url).spawn();
            #[cfg(target_os = "windows")]
            let command = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            if let Err(error) = command { eprintln!("Open the manager URL in your browser: {error}"); }
        }
    }).await
}
