use pretty_assertions::assert_eq;

#[tokio::test]
async fn current_decision_settings_follow_user_changes_and_preserve_session_overrides()
-> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let config = crate::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let path = home.path().join("config.toml");
    assert_eq!(
        config.decision_advisor_snapshot().await?.effective.mode,
        codex_model_provider::DecisionAdvisorMode::Off
    );
    std::fs::write(&path, "[decision_advisor]\nmode = 'rank'\n")?;
    assert_eq!(
        config.decision_advisor_snapshot().await?.effective.mode,
        codex_model_provider::DecisionAdvisorMode::Rank
    );
    std::fs::write(&path, "[decision_advisor]\nmode = 'off'\n")?;
    assert_eq!(
        config.decision_advisor_snapshot().await?.effective.mode,
        codex_model_provider::DecisionAdvisorMode::Off
    );
    let overridden = crate::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .cli_overrides(vec![(
            "decision_advisor.mode".into(),
            toml::Value::String("off".into()),
        )])
        .build()
        .await?;
    std::fs::write(&path, "[decision_advisor]\nmode = 'rank'\n")?;
    assert_eq!(
        overridden.decision_advisor_snapshot().await?.effective.mode,
        codex_model_provider::DecisionAdvisorMode::Off
    );
    Ok(())
}

#[tokio::test]
async fn decision_settings_do_not_reenable_ignored_user_configuration() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.toml"),
        "[decision_advisor]\nmode = 'rank'\n",
    )?;
    let config = crate::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(codex_config::LoaderOverrides {
            ignore_user_config: true,
            ..Default::default()
        })
        .build()
        .await?;
    std::fs::write(home.path().join("config.toml"), "invalid = [")?;
    assert_eq!(
        config.decision_advisor_snapshot().await?.effective.mode,
        codex_model_provider::DecisionAdvisorMode::Off
    );
    Ok(())
}

#[tokio::test]
async fn decision_refresh_preserves_project_overrides() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let mut config = crate::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let mut layers = config
        .config_layer_stack
        .all_layers_low_to_high()
        .filter(|layer| !matches!(layer.name, codex_config::ConfigLayerSource::Project { .. }))
        .cloned()
        .collect::<Vec<_>>();
    layers.push(codex_config::ConfigLayerEntry::new(
        codex_config::ConfigLayerSource::Project {
            dot_codex_folder: codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
                home.path().join("project/.codex"),
            )?,
        },
        toml::from_str("[decision_advisor]\nmode = 'shadow'\n").unwrap(),
    ));
    layers.sort_by_key(|layer| layer.name.precedence());
    config.config_layer_stack =
        codex_config::ConfigLayerStack::new(layers, Default::default(), Default::default())?;
    std::fs::write(
        home.path().join("config.toml"),
        "[decision_advisor]\nmode = 'rank'\n",
    )?;
    assert_eq!(
        config.decision_advisor_snapshot().await?.effective.mode,
        codex_model_provider::DecisionAdvisorMode::Shadow
    );
    Ok(())
}

#[tokio::test]
async fn decision_refresh_after_a_user_layer_reload_keeps_following_disk_changes()
-> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let mut config = crate::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let path = config
        .config_layer_stack
        .get_user_config_file()
        .unwrap()
        .clone();
    config.config_layer_stack = config.config_layer_stack.with_user_config(
        &path,
        toml::from_str("[decision_advisor]\nmode = 'rank'\n").unwrap(),
    )?;
    std::fs::write(&path, "[decision_advisor]\nmode = 'rank'\n")?;
    assert_eq!(
        config.decision_advisor_snapshot().await?.effective.mode,
        codex_model_provider::DecisionAdvisorMode::Rank
    );
    std::fs::write(&path, "[decision_advisor]\nmode = 'off'\n")?;
    assert_eq!(
        config.decision_advisor_snapshot().await?.effective.mode,
        codex_model_provider::DecisionAdvisorMode::Off
    );
    Ok(())
}

#[tokio::test]
async fn decision_refresh_reads_the_active_profile_file_and_retains_base_user_precedence()
-> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let profile = home.path().join("profiles/work.toml");
    std::fs::create_dir_all(profile.parent().unwrap())?;
    std::fs::write(&profile, "[decision_advisor]\nmode = 'off'\n")?;
    let config = crate::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(codex_config::LoaderOverrides {
            user_config_path: Some(
                codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(profile.clone())?,
            ),
            user_config_profile: Some("work".parse().unwrap()),
            ..Default::default()
        })
        .build()
        .await?;
    std::fs::write(
        home.path().join("config.toml"),
        "[decision_advisor]\nmode = 'rank'\n",
    )?;
    std::fs::write(&profile, "[decision_advisor]\nmode = 'shadow'\n")?;
    let snapshot = config.decision_advisor_snapshot().await?;
    assert_eq!(
        (snapshot.configured.mode, snapshot.effective.mode),
        (
            codex_config::DecisionAdvisorModeToml::Shadow,
            codex_model_provider::DecisionAdvisorMode::Shadow
        )
    );
    Ok(())
}
