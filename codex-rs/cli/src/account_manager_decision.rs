//! Guided setup for independent decision services without shell environment edits.

use super::Locale;
use super::clean;
use super::input::prompt;
use super::input::secret_prompt;
use codex_app_server::account_management::AccountManager;
use codex_app_server::account_management::AccountManagerOperation;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

pub(super) fn render(view: &Value, locale: Locale) -> String {
    let config = &view["config"];
    let service = match config["provider"].as_str() {
        Some("cloudflare") => "Cloudflare Clef",
        _ => "TypeSafe Jev",
    };
    let mode = locale.text(match config["mode"].as_str() {
        Some("rank") => "Enabled",
        Some("shadow") => "Observe only",
        _ => "Disabled",
    });
    let credential = locale.text(if view["credentialPresent"].as_bool().unwrap_or(false) {
        "Token available"
    } else {
        "Token not configured"
    });
    [
        locale.text("Decision assistance").to_string(),
        locale.format("Service: {}", &[service]),
        locale.format("Mode: {}", &[mode]),
        credential.to_string(),
        locale
            .text("Settings apply to subsequent advisor calls. Other hosts have not been observed.")
            .into(),
        locale
            .text("[S] Choose one service  [T] Test saved connection  [X] Disable  [K] Remove saved token  [Enter] Back")
            .into(),
    ]
    .join("\n")
}

pub(super) async fn choose(manager: &Arc<AccountManager>, locale: Locale) -> anyhow::Result<()> {
    let inventory = manager.inventory().await?;
    let inventory_json = serde_json::to_value(&inventory)?;
    let view = &inventory_json["decisionAdvisor"];
    let columns = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
    for line in render(view, locale).lines() {
        for wrapped in textwrap::wrap(line, columns.max(20)) {
            println!("{wrapped}");
        }
    }
    let choice = prompt(locale, "Decision assistance action", "").await?;
    let mut config = view["config"].clone();
    if !config.is_object() {
        config = json!({"provider":"cloudflare","model":"clef-flash","mode":"off"});
    }
    let mut payload = match choice.trim().to_ascii_lowercase().as_str() {
        "" => return Ok(()),
        "x" => {
            config["mode"] = json!("off");
            json!({"type":"decisionSave","config":config,"credential":{"type":"keep"},"consent":false})
        }
        "k" => {
            if prompt(
                locale,
                "Type REMOVE to delete this service token and disable assistance",
                "",
            )
            .await?
                != "REMOVE"
            {
                return Ok(());
            }
            config["mode"] = json!("off");
            config["credential_source"] = json!("stored");
            json!({"type":"decisionSave","config":config,"credential":{"type":"remove"},"consent":false})
        }
        "t" => {
            println!(
                "{}",
                locale.text(
                    "Testing sends only built-in example text and may incur a separate service fee."
                )
            );
            if prompt(locale, "Type TEST to send one connection check", "").await? != "TEST" {
                return Ok(());
            }
            json!({"type":"decisionProbe","config":config,"credential":{"type":"keep"},"consent":true})
        }
        "s" => {
            println!(
                "{}",
                locale.text("1 Cloudflare Clef  2 TypeSafe Jev  Enter returns")
            );
            println!(
                "{}",
                locale.text("Choose one service; only that service is called.")
            );
            let service = prompt(locale, "Service number", "").await?;
            let (provider, model, endpoint, environment) = match service.as_str() {
                "" => return Ok(()),
                "1" => {
                    let account = prompt(locale, "Cloudflare Account ID", "").await?;
                    anyhow::ensure!(
                        !account.is_empty()
                            && account.len() <= 128
                            && account
                                .chars()
                                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'),
                        locale.text(
                            "Enter the Cloudflare Account ID shown on the Workers AI REST API page"
                        )
                    );
                    println!("{}", locale.text("1 Clef-flash  2 Clef"));
                    let model = match prompt(locale, "Model number", "1").await?.as_str() {
                        "1" => "clef-flash",
                        "2" => "clef",
                        _ => anyhow::bail!(locale.text("Choose a listed number")),
                    };
                    (
                        "cloudflare",
                        model.to_string(),
                        format!(
                            "https://api.cloudflare.com/client/v4/accounts/{account}/ai/run/@cf/cloudflare/{model}"
                        ),
                        "CLOUDFLARE_AI_TOKEN",
                    )
                }
                "2" => (
                    "typesafe",
                    "jev-1.13.0".into(),
                    "https://api.typesafe.ai/v1/systemone".into(),
                    "TYPESAFE_API_KEY",
                ),
                _ => anyhow::bail!(locale.text("Choose a listed number")),
            };
            println!("{}", locale.text("Enter a separate service token. Leave blank to keep the token for this exact service."));
            let secret = secret_prompt(locale).await?;
            println!("{}", locale.text("1 Disabled  2 Observe only  3 Enabled"));
            let mode = match prompt(locale, "Mode number", "1").await?.as_str() {
                "1" => "off",
                "2" => "shadow",
                "3" => "rank",
                _ => anyhow::bail!(locale.text("Choose a listed number")),
            };
            println!("{}", locale.text("Observe only sends requests but keeps the original results. Enabled applies validated suggestions."));
            let skills = prompt(locale, "Suggest skills too? 1 Yes / 2 No", "2").await?;
            anyhow::ensure!(
                matches!(skills.as_str(), "1" | "2"),
                locale.text("Choose a listed number")
            );
            println!("{}", locale.text("Cloud services receive search text and tool or skill descriptions, and may charge separately. Saving sends no request."));
            if prompt(locale, "Type SAVE to confirm these settings", "").await? != "SAVE" {
                return Ok(());
            }
            let credential = if secret.is_empty() {
                json!({"type":"keep"})
            } else {
                json!({"type":"replace","value":secret})
            };
            json!({"type":"decisionSave","config":{
                "mode":mode,"provider":provider,"model":model,"endpoint":endpoint,
                "api_key_env":environment,"suggest_skills":skills=="1","timeout_ms":650,"min_confidence":0.35,
                "credential_source":view["config"]["credential_source"].as_str().unwrap_or("environment")
            },"credential":credential,"consent":mode!="off"})
        }
        _ => anyhow::bail!(locale.text("Choose a listed action")),
    };
    payload["expectedVersion"] = view["userConfigVersion"].clone();
    let operation: AccountManagerOperation = serde_json::from_value(payload)?;
    let result = manager.execute(operation).await?;
    println!("{}", clean(locale.message(&result.message)));
    let _ = prompt(locale, "Press Enter to return", "").await?;
    Ok(())
}

#[cfg(test)]
#[path = "account_manager_decision_tests.rs"]
mod tests;
