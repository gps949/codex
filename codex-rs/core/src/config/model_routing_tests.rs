use pretty_assertions::assert_eq;

#[tokio::test]
async fn routing_settings_reload_only_the_user_table_and_keep_cli_override() -> std::io::Result<()>
{
    let home = tempfile::tempdir()?;
    let config = crate::config::ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await?;
    std::fs::write(
        home.path().join("config.toml"),
        "[model_routing]\nmode='automatic'\npreference=20\n",
    )?;
    assert_eq!(config.model_routing_snapshot().await?.preference, 20);
    let fixed = crate::config::ConfigBuilder::default()
        .codex_home(home.path().into())
        .cli_overrides(vec![(
            "model_routing.preference".into(),
            toml::Value::Integer(80),
        )])
        .build()
        .await?;
    std::fs::write(
        home.path().join("config.toml"),
        "[model_routing]\npreference=0\n",
    )?;
    assert_eq!(fixed.model_routing_snapshot().await?.preference, 80);
    Ok(())
}
