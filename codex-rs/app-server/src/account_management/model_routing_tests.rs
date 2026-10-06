use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn routing_view_distinguishes_catalog_saved_and_effective_roles() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.toml"),
        "[model_routing.model_roles]\ngpt-6-luna='capability'\n",
    )?;
    let mut config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().into())
        .cli_overrides(vec![(
            "model_routing.model_roles.gpt-6-luna".into(),
            toml::Value::String("balanced".into()),
        )])
        .build()
        .await?;
    let mut catalog = codex_models_manager::bundled_models_response()?;
    catalog.models.retain(|model| model.slug == "gpt-6-luna");
    for model in &mut catalog.models {
        model.supported_in_api = true;
    }
    config.model_catalog = Some(catalog);
    let manager = AccountManager::new(config);
    let view = manager.model_routing_view().await?;
    let models = serde_json::to_value(&view.models)?;
    let roles = serde_json::json!({
        "savedRole": view.config.model_roles["gpt-6-luna"],
        "model": models[0]["model"],
        "catalogRole": models[0]["catalogRole"],
        "effectiveRole": models[0]["effectiveRole"],
        "role": models[0]["role"],
    });
    assert_eq!(
        roles,
        serde_json::json!({
            "savedRole": "capability", "model": "gpt-6-luna",
            "catalogRole": "economy", "effectiveRole": "balanced", "role": "balanced",
        })
    );
    insta::assert_snapshot!(
        "webui_model_routing_roles",
        serde_json::to_string_pretty(&roles)?
    );
    Ok(())
}

#[tokio::test]
async fn model_routing_save_is_local_and_rejects_a_stale_page_or_missing_external_consent()
-> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let view = manager.model_routing_view().await?;
    let mut policy = view.config;
    policy.mode = ModelRoutingMode::Automatic;
    policy.preference = 10;
    manager
        .save_model_routing(policy.clone(), Some(view.user_config_version.clone()))
        .await?;
    assert_eq!(
        manager.config.model_routing_snapshot().await?.preference,
        10
    );
    assert!(
        manager
            .save_model_routing(policy.clone(), Some(view.user_config_version))
            .await
            .is_err()
    );
    policy.source = ModelRoutingSource::DecisionService;
    assert!(manager.save_model_routing(policy, None).await.is_err());
    assert_eq!(
        manager.config.model_routing_snapshot().await?.source,
        ModelRoutingSource::Local
    );
    Ok(())
}

#[tokio::test]
async fn local_preview_respects_allowlist_without_enabling_external_task_sharing()
-> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let mut config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await?;
    let mut models = codex_models_manager::bundled_models_response()?;
    models
        .models
        .retain(|model| matches!(model.slug.as_str(), "gpt-6.1-sol" | "gpt-6-luna"));
    for model in &mut models.models {
        model.supported_in_api = true;
    }
    config.model = Some("gpt-6.1-sol".into());
    config.model_catalog = Some(models);
    let manager = AccountManager::new(config);
    let policy = ModelRoutingConfigToml {
        mode: ModelRoutingMode::Automatic,
        source: ModelRoutingSource::DecisionService,
        preference: 0,
        allowed_models: vec!["gpt-6-luna".into()],
        ..Default::default()
    };
    let result = manager
        .preview_model_routing("Translate this sentence.", policy)
        .await?;
    assert_eq!(
        (result.data["model"].clone(), result.data["effort"].clone()),
        (serde_json::json!("gpt-6-luna"), serde_json::json!("low"))
    );
    assert_eq!(
        manager.config.model_routing_snapshot().await?,
        ModelRoutingConfigToml::default()
    );
    assert!(!home.path().join("config.toml").exists());
    Ok(())
}

#[tokio::test]
async fn changing_decision_service_revokes_external_model_routing_consent() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.toml"),
        "[model_routing]\nmode='automatic'\nsource='decision_service'\nsend_task_description=true\npreference=25\n[decision_advisor]\nprovider='typesafe'\nmode='off'\nendpoint='https://api.typesafe.ai/v1/systemone'\nmodel='jev-1.13.0'\n",
    )?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let current = manager.decision_advisor_view().await?;
    let mut service = current.config;
    service.endpoint = Some("https://service.example/v1/systemone".into());
    manager
        .save_decision_advisor(
            service,
            super::DecisionAdvisorCredentialAction::Keep,
            false,
            Some(current.user_config_version),
        )
        .await?;
    let routing = manager.config.model_routing_snapshot().await?;
    assert_eq!(
        (
            routing.mode,
            routing.send_task_description,
            routing.preference
        ),
        (ModelRoutingMode::Off, false, 25)
    );
    Ok(())
}
