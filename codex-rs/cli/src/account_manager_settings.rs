//! Named choices and explicit previews for common account-pool settings.

use super::input::prompt;
use codex_app_server::account_management::AccountManagerOperation as Operation;
use serde_json::Value;

pub(super) async fn choose(settings: &Value) -> anyhow::Result<Option<Operation>> {
    println!(
        "\nSettings\n1 Rotation strategy  2 Early switch percent  3 Return to preferred\n4 Standby warmup  5 Warmup interval  6 Wait for quota recovery\n7 Maximum waiting minutes  8 Automatic reset credits  9 Credit waiting threshold\nB Balanced preset  L Longer standby preset  J Advanced JSON  Enter returns."
    );
    println!("Presets preserve your reset-credit and paid API choices.");
    let choice = prompt("Setting", "").await?.to_ascii_lowercase();
    let values = match choice.as_str() {
        "" => return Ok(None),
        "b" | "l" => {
            let values = serde_json::json!({
                "rotation_strategy": if choice == "l" { "earliest_reset" } else { "fill_first" },
                "preemptive_switch_percent":95,
                "return_to_preferred": choice == "b",
                "window_warmup":true,
                "window_warmup_interval_minutes": if choice == "l" { 10 } else { 5 },
                "resume_after_reset":true
            });
            println!(
                "Preview: {}\nWarmup makes a small request to start eligible standby windows.",
                serde_json::to_string_pretty(&values)?
            );
            if prompt("Type APPLY to save this preset", "").await? != "APPLY" {
                return Ok(None);
            }
            values
        }
        "j" => {
            println!("{}", serde_json::to_string_pretty(settings)?);
            let value = prompt("Settings JSON (Enter returns)", "").await?;
            if value.is_empty() {
                return Ok(None);
            }
            serde_json::from_str(&value)?
        }
        "1" | "8" => {
            let (key, options) = if choice == "1" {
                (
                    "rotation_strategy",
                    vec![
                        (
                            "fill_first",
                            "Keep using an account until early switch or exhaustion",
                        ),
                        (
                            "earliest_reset",
                            "Prefer the account whose quota resets sooner",
                        ),
                    ],
                )
            } else {
                println!(
                    "Automatic redemption consumes reset credits only when all eligible subscriptions are exhausted."
                );
                (
                    "auto_reset_credits",
                    vec![
                        ("never", "Never consume reset credits automatically"),
                        (
                            "when_pool_exhausted",
                            "Allow one credit after the subscription pool is exhausted",
                        ),
                    ],
                )
            };
            for (index, (value, description)) in options.iter().enumerate() {
                println!(
                    "{} {}: {}{}",
                    index + 1,
                    value,
                    description,
                    if settings[key].as_str() == Some(value) {
                        " (current)"
                    } else {
                        ""
                    }
                );
            }
            let value = prompt("Choice number (Enter returns)", "").await?;
            if value.is_empty() {
                return Ok(None);
            }
            let selected = value
                .parse::<usize>()
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| options.get(index))
                .ok_or_else(|| anyhow::anyhow!("Choose a listed number"))?;
            if choice == "8"
                && selected.0 == "when_pool_exhausted"
                && prompt("Type ENABLE to authorize automatic reset-credit use", "").await?
                    != "ENABLE"
            {
                return Ok(None);
            }
            serde_json::json!({key: selected.0})
        }
        "3" | "4" | "6" => {
            let (key, label) = match choice.as_str() {
                "3" => (
                    "return_to_preferred",
                    "Return to the preferred account after recovery",
                ),
                "4" => (
                    "window_warmup",
                    "Start eligible standby windows with a small quota request",
                ),
                "6" => (
                    "resume_after_reset",
                    "Keep an exhausted turn waiting for quota recovery",
                ),
                _ => unreachable!(),
            };
            println!(
                "{label}\nCurrent: {}\n1 Enabled  2 Disabled  Enter keeps current.",
                if settings[key].as_bool().unwrap_or(true) {
                    "Enabled"
                } else {
                    "Disabled"
                }
            );
            let value = prompt("Choice", "").await?;
            let value = match value.as_str() {
                "1" => true,
                "2" => false,
                "" => return Ok(None),
                _ => anyhow::bail!("No change made. Choose 1 or 2."),
            };
            serde_json::json!({key:value})
        }
        "2" | "5" | "7" | "9" => {
            let (key, label, fallback) = match choice.as_str() {
                "2" => (
                    "preemptive_switch_percent",
                    "Switch at used percent (0 or 100 disables)",
                    "95",
                ),
                "5" => (
                    "window_warmup_interval_minutes",
                    "Warmup interval in minutes",
                    "5",
                ),
                "7" => (
                    "max_reset_wait_minutes",
                    "Maximum waiting minutes per turn",
                    "360",
                ),
                "9" => (
                    "auto_reset_credit_min_wait_minutes",
                    "Natural-reset waiting threshold before using a credit",
                    "60",
                ),
                _ => unreachable!(),
            };
            let current = settings
                .get(key)
                .filter(|value| !value.is_null())
                .map_or_else(|| fallback.into(), Value::to_string);
            let value = prompt(label, &current).await?;
            if choice == "2" {
                let value = value.parse::<f64>()?;
                anyhow::ensure!(
                    value.is_finite() && (0.0..=100.0).contains(&value),
                    "Enter a percentage from 0 to 100"
                );
                serde_json::json!({key:value})
            } else {
                let value = value.parse::<u32>()?;
                if choice == "5" {
                    anyhow::ensure!(value >= 5, "Warmup interval must be at least 5 minutes");
                }
                if choice == "7" {
                    anyhow::ensure!(value <= 1440, "Maximum waiting time is 1440 minutes");
                }
                serde_json::json!({key:value})
            }
        }
        _ => anyhow::bail!("Choose a listed setting or preset."),
    };
    Ok(Some(Operation::Settings { values }))
}
