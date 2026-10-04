//! Typed effective pool settings produce sparse, opposite-value confirmation patches.

use super::*;

pub(super) fn setting_change(
    settings: &serde_json::Value,
    index: usize,
    language: NativeAccountLanguage,
) -> anyhow::Result<(&'static str, serde_json::Value, String)> {
    let settings: codex_config::AccountPoolConfigToml = serde_json::from_value(settings.clone())?;
    let toggle = |enabled| {
        language.text(
            if enabled { "On → Off" } else { "Off → On" },
            if enabled {
                "开启 → 关闭"
            } else {
                "关闭 → 开启"
            },
        )
    };
    let (key, value, description) = match index {
        0 => {
            let next = match settings.effective_rotation_strategy() {
                codex_config::AccountPoolRotationStrategy::FillFirst => {
                    codex_config::AccountPoolRotationStrategy::EarliestReset
                }
                codex_config::AccountPoolRotationStrategy::EarliestReset => {
                    codex_config::AccountPoolRotationStrategy::FillFirst
                }
            };
            (
                "rotation_strategy",
                serde_json::to_value(next)?,
                language
                    .text(
                        if next == codex_config::AccountPoolRotationStrategy::EarliestReset {
                            "Rotation: fill first → earliest reset"
                        } else {
                            "Rotation: earliest reset → fill first"
                        },
                        if next == codex_config::AccountPoolRotationStrategy::EarliestReset {
                            "轮换：顺序用完 → 最早重置优先"
                        } else {
                            "轮换：最早重置优先 → 顺序用完"
                        },
                    )
                    .into(),
            )
        }
        1 => (
            "window_warmup",
            serde_json::json!(!settings.effective_window_warmup()),
            format!(
                "{}: {}",
                language.text("Window warmup", "窗口预热"),
                toggle(settings.effective_window_warmup())
            ),
        ),
        2 => {
            let enabled = settings.resume_after_reset.unwrap_or(true);
            (
                "resume_after_reset",
                serde_json::json!(!enabled),
                format!(
                    "{}: {}",
                    language.text("Resume after reset", "重置后接续"),
                    toggle(enabled)
                ),
            )
        }
        3 => {
            let current = configured_reset_wait_minutes(&settings);
            let next = if current == 0 {
                configured_reset_wait_minutes(&codex_config::AccountPoolConfigToml::default())
            } else {
                0
            };
            (
                "max_reset_wait_minutes",
                serde_json::json!(next),
                format!(
                    "{}: {current} → {next} {}",
                    language.text("Reset wait limit", "重置等待上限"),
                    language.text("min", "分钟")
                ),
            )
        }
        4 => {
            let next = match settings.effective_auto_reset_credits() {
                codex_config::AutoResetCredits::Never => {
                    codex_config::AutoResetCredits::WhenPoolExhausted
                }
                codex_config::AutoResetCredits::WhenPoolExhausted => {
                    codex_config::AutoResetCredits::Never
                }
            };
            (
                "auto_reset_credits",
                serde_json::to_value(next)?,
                language
                    .text(
                        if next == codex_config::AutoResetCredits::Never {
                            "Automatic credits: after pool exhaustion → never"
                        } else {
                            "Automatic credits: never → after pool exhaustion"
                        },
                        if next == codex_config::AutoResetCredits::Never {
                            "自动用券：订阅池耗尽后 → 从不"
                        } else {
                            "自动用券：从不 → 订阅池耗尽后"
                        },
                    )
                    .into(),
            )
        }
        5 => (
            "return_to_preferred",
            serde_json::json!(!settings.effective_return_to_preferred()),
            format!(
                "{}: {}",
                language.text("Return to preferred", "回到首选账号"),
                toggle(settings.effective_return_to_preferred())
            ),
        ),
        _ => anyhow::bail!("Unknown pool setting"),
    };
    let description = format!(
        "{description}\n{}",
        if index == 4 {
            language.text("Automatic redemption can consume reset credits only after the subscription pool is exhausted.", "自动兑换仅在订阅池耗尽后发生，并会消耗重置券。")
        } else {
            language.text("Applies after configuration refresh.", "配置刷新后生效。")
        }
    );
    Ok((key, value, description))
}
