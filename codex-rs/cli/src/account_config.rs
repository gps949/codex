use std::fs;
use std::path::Path;

use codex_config::AccountPoolConfigToml;
use codex_config::AccountPoolRotationStrategy;
use codex_config::CONFIG_TOML_FILE;
use toml::Value as TomlValue;

pub(crate) fn patch_account_pool_config(
    codex_home: &Path,
    update: impl FnOnce(&mut AccountPoolConfigToml),
) -> Result<AccountPoolConfigToml, String> {
    let config_path = codex_home.join(CONFIG_TOML_FILE);
    let mut root = if config_path.is_file() {
        let contents = fs::read_to_string(&config_path)
            .map_err(|err| format!("failed to read {}: {err}", config_path.display()))?;
        contents
            .parse::<TomlValue>()
            .map_err(|err| format!("failed to parse {}: {err}", config_path.display()))?
    } else {
        TomlValue::Table(toml::map::Map::new())
    };
    let table = root
        .as_table_mut()
        .ok_or_else(|| format!("{} root must be a table", config_path.display()))?;

    let mut account_pool = table
        .get("account_pool")
        .map(|value| {
            toml::from_str(&toml::to_string(value).unwrap_or_default()).unwrap_or_default()
        })
        .unwrap_or_default();
    update(&mut account_pool);
    table.insert(
        "account_pool".to_string(),
        TomlValue::try_from(&account_pool)
            .map_err(|err| format!("failed to serialize [account_pool]: {err}"))?,
    );

    let serialized = toml::to_string_pretty(&root)
        .map_err(|err| format!("failed to serialize {}: {err}", config_path.display()))?;
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    fs::write(&config_path, serialized)
        .map_err(|err| format!("failed to write {}: {err}", config_path.display()))?;
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
