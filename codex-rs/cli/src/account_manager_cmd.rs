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
    /// Interface language: en or zh-CN. The terminal defaults to English.
    #[arg(long, value_enum)]
    lang: Option<crate::account_manager_tui::Locale>,
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
        let result =
            crate::account_manager_tui::run(Arc::clone(&manager), args.lang.unwrap_or_default())
                .await;
        manager.shutdown_logins().await;
        return result;
    }
    codex_app_server::account_management::serve(manager, AccountManagerWebOptions {
        listen: args.listen, allowed_origins: args.allowed_origins,
    }, |url| {
        let url = args.lang.and_then(|locale| {
            let mut url = url::Url::parse(url).ok()?;
            url.query_pairs_mut().append_pair("lang", match locale {
                crate::account_manager_tui::Locale::English => "en",
                crate::account_manager_tui::Locale::SimplifiedChinese => "zh-CN",
            });
            Some(url.to_string())
        }).unwrap_or_else(|| url.to_string());
        eprintln!("Codex account manager: {url}");
        eprintln!("Use Stop manager or Ctrl+C to exit. Closing the last tab normally stops this process after 30 seconds; lost tabs expire after 5 minutes. An unopened manager exits after 10 minutes.");
        eprintln!("The pairing link grants account administration; share it only with your own browser.");
        if !args.no_open {
            #[cfg(target_os = "macos")]
            let command = tokio::process::Command::new("open").arg(&url).spawn();
            #[cfg(target_os = "linux")]
            let command = tokio::process::Command::new("xdg-open").arg(&url).spawn();
            #[cfg(target_os = "windows")]
            let command = tokio::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn();
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            match command {
                Ok(mut child) => {
                    tokio::spawn(async move {
                        let _ = child.wait().await;
                    });
                }
                Err(error) => eprintln!("Open the manager URL in your browser: {error}"),
            }
        }
    }).await
}
