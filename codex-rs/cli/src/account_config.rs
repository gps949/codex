use std::fs;
use std::path::Path;

use codex_config::AccountPoolConfigToml;
use codex_config::AccountPoolRotationStrategy;
use codex_config::CONFIG_TOML_FILE;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;
use toml::Value as TomlValue;

pub(crate) fn patch_account_pool_config(
    codex_home: &Path,
    update: impl FnOnce(&mut AccountPoolConfigToml),
) -> Result<AccountPoolConfigToml, String> {
    let config_path = codex_home.join(CONFIG_TOML_FILE);
    let mut root = if config_path.is_file() {
        let contents = fs::read_to_string(&config_path)
            .map_err(|err| format!("failed to read {}: {err}", config_path.display()))?;
        toml::from_str::<TomlValue>(&contents)
            .map_err(|err| format!("failed to parse {}: {err}", config_path.display()))?
    } else {
        TomlValue::Table(toml::map::Map::new())
    };
    let table = root
        .as_table_mut()
        .ok_or_else(|| format!("{} root must be a table", config_path.display()))?;
    let mut account_pool: AccountPoolConfigToml = match table.get("account_pool") {
        Some(value) => value
            .clone()
            .try_into()
            .map_err(|err| format!("invalid [account_pool]: {err}"))?,
        None => AccountPoolConfigToml::default(),
    };
    let before = TomlValue::try_from(&account_pool)
        .map_err(|err| format!("failed to read account-pool settings: {err}"))?;
    update(&mut account_pool);
    let after = TomlValue::try_from(&account_pool)
        .map_err(|err| format!("failed to serialize account-pool settings: {err}"))?;
    let mut edits = Vec::new();
    let updated = after
        .as_table()
        .ok_or("account-pool settings must serialize as a table")?;
    for (key, value) in updated {
        if before.get(key) != Some(value) {
            edits.push(ConfigEdit::SetPath {
                segments: vec!["account_pool".into(), key.clone()],
                value: value
                    .to_string()
                    .parse()
                    .map_err(|err| format!("failed to encode {key}: {err}"))?,
            });
        }
    }
    // Preserve comments and unrelated settings, and use the existing atomic writer.
    ConfigEditsBuilder::new(codex_home)
        .with_edits(edits)
        .apply_blocking()
        .map_err(|err| format!("failed to update {}: {err}", config_path.display()))?;
    Ok(account_pool)
}

pub(crate) fn parse_rotation_strategy(value: &str) -> Result<AccountPoolRotationStrategy, String> {
    match value {
        "fill_first" | "fill-first" => Ok(AccountPoolRotationStrategy::FillFirst),
        "earliest_reset" | "earliest-reset" => Ok(AccountPoolRotationStrategy::EarliestReset),
        other => Err(format!(
            "invalid rotation strategy {other:?}; expected fill_first or earliest_reset"
        )),
    }
}

pub(crate) fn format_rotation_strategy(strategy: AccountPoolRotationStrategy) -> &'static str {
    match strategy {
        AccountPoolRotationStrategy::FillFirst => "fill_first",
        AccountPoolRotationStrategy::EarliestReset => "earliest_reset",
    }
}

pub(crate) async fn run_account_config_set(
    overrides: codex_utils_cli::CliConfigOverrides,
    key: &str,
    value: TomlValue,
) -> ! {
    let config = crate::account_cmd::load_config_or_exit(overrides).await;
    let result = patch_account_pool_config(&config.codex_home, |pool| match key {
        "window_warmup" => pool.window_warmup = value.as_bool(),
        "resume_after_reset" => pool.resume_after_reset = value.as_bool(),
        "max_reset_wait_minutes" => {
            pool.max_reset_wait_minutes = value.as_integer().map(|n| n as u64)
        }
        "window_warmup_interval_minutes" => {
            pool.window_warmup_interval_minutes = value.as_integer().map(|n| n as u64)
        }
        "auto_reset_credit_min_wait_minutes" => {
            pool.auto_reset_credit_min_wait_minutes = value.as_integer()
        }
        "auto_reset_credits" => {
            pool.auto_reset_credits = Some(match value.as_str() {
                Some("never") => codex_config::AutoResetCredits::Never,
                Some("when_pool_exhausted") => codex_config::AutoResetCredits::WhenPoolExhausted,
                _ => unreachable!("validated reset-credit mode"),
            })
        }
        _ => unreachable!("supported account-pool setting"),
    });
    match result {
        Ok(_) => {
            eprintln!(
                "Saved account_pool.{key}={value}. Inspect effective settings with `codex account config show`."
            );
            eprintln!(
                "Running sessions adopt settings at their next configuration refresh; command-line overrides take precedence."
            );
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("Error updating config.toml: {error}");
            std::process::exit(1);
        }
    }
}
